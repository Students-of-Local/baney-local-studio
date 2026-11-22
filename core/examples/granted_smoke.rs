use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::plugins::{self as plugin_store, Capability, Grants};
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, Item, ItemKind, KernelState, PluginInfo, Project, PromptDraft, Studio,
    StudioConfig, ThreadRow, ThreadStatus, TranscriptOp, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{Map, Value};

#[path = "support/real_model.rs"]
mod real_model;

const ID: &str = "granted";
const STEP: Duration = Duration::from_mins(3);
const KERNEL: Duration = Duration::from_mins(10);
const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
with open(sys.argv[1], "a") as f:
    f.write(json.dumps({"pid": os.getpid(), "granted": sorted(k for k in os.environ if k.startswith("LOCAL_STUDIO_GRANTED_"))}) + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}}, "serverInfo": {"name": "granted", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "granted_report", "description": "Lists the sensitive permissions this plugin holds", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": ",".join(sorted(k for k in os.environ if k.startswith("LOCAL_STUDIO_GRANTED_")))}]}
    elif method == "ping":
        result = {}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": str(method)}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#;

#[derive(Default)]
struct Signal {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Waker for Signal {
    fn wake(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.ready.notify_one();
    }
}

#[derive(Default)]
struct View {
    connection: Option<Connection>,
    kernel: Option<KernelState>,
    projects: Vec<Project>,
    threads: Vec<ThreadRow>,
    started: Vec<(String, String)>,
    transcripts: Vec<(String, Vec<Item>)>,
    plugins: Vec<PluginInfo>,
    done: Vec<(String, Result<String, String>)>,
    usage: Option<UsageReport>,
}

impl View {
    fn thread(&self, draft: &str) -> Option<String> {
        self.started.iter().find(|(key, _)| key == draft).map(|(_, thread)| thread.clone())
    }

    fn replied(&self, thread: &str) -> bool {
        let idle = self
            .threads
            .iter()
            .find(|row| row.id == thread)
            .is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval));
        let items = self.transcripts.iter().find(|(key, _)| key == thread).map_or(&[][..], |(_, items)| items);
        idle && items.iter().any(
            |item| matches!(&item.kind, ItemKind::Assistant { streaming: false, blocks, .. } if !blocks.is_empty()),
        )
    }

    fn outcome(&self, request: &str) -> Option<&Result<String, String>> {
        self.done.iter().find(|(key, _)| key == request).map(|(_, outcome)| outcome)
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
}

impl Harness {
    fn pump_until(&mut self, step: &str, limit: Duration, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + limit;
        loop {
            for update in self.studio.tick() {
                self.apply(update)?;
            }
            if done(&self.view) {
                println!("ok   {step}");
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("timed out: {step}");
            };
            let pending = self.signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut pending, _) = self
                .signal
                .ready
                .wait_timeout_while(pending, left.min(Duration::from_millis(500)), |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner);
            *pending = false;
        }
    }

    fn apply(&mut self, update: Update) -> anyhow::Result<()> {
        match update {
            Update::Connection { state } => self.view.connection = Some(state),
            Update::Kernel { state } => self.view.kernel = Some(state),
            Update::Projects { projects } => self.view.projects = projects,
            Update::Threads { threads } => self.view.threads = threads,
            Update::ThreadStarted { draft, thread } => self.view.started.push((draft, thread)),
            Update::Plugins { plugins } => self.view.plugins = plugins,
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestDone { request, message } => self.view.done.push((request, Ok(message))),
            Update::RequestFailed { request, message } => self.view.done.push((request, Err(message))),
            Update::Usage { report, .. } => self.view.usage = Some(report),
            Update::Transcript { thread, op } => {
                if !self.view.transcripts.iter().any(|(key, _)| *key == thread) {
                    self.view.transcripts.push((thread.clone(), Vec::new()));
                }
                let items =
                    &mut self.view.transcripts.iter_mut().find(|(key, _)| *key == thread).context("transcript")?.1;
                match op {
                    TranscriptOp::Reset { items: fresh } => *items = fresh,
                    TranscriptOp::Insert { index, item } => {
                        let index = usize::try_from(index)?.min(items.len());
                        items.insert(index, item);
                    }
                    TranscriptOp::Update { index, item } => {
                        if let Some(slot) = items.get_mut(usize::try_from(index)?) {
                            *slot = item;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn turn(&mut self, draft: &str, project: &str) -> anyhow::Result<()> {
        let settings = real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local);
        let prompt =
            PromptDraft { text: "Reply with only the word ready.".to_string(), images: Vec::new(), files: Vec::new() };
        self.studio.start_thread(draft.to_string(), Some(project.to_string()), prompt, settings);
        self.pump_until(&format!("thread {draft} started"), STEP, |view| view.thread(draft).is_some())?;
        let thread = self.view.thread(draft).context("thread")?;
        self.studio.select_thread(Some(thread.clone()));
        self.pump_until(&format!("the local model replied in thread {draft}"), STEP, |view| view.replied(&thread))
    }

    fn switch(&mut self, capability: &str, on: bool) -> anyhow::Result<String> {
        let request = format!("switch-{capability}-{on}");
        self.studio.set_plugin_permission(request.clone(), ID.to_string(), capability.to_string(), on);
        self.pump_until(&format!("Settings switch {capability} {}", if on { "on" } else { "off" }), STEP, |view| {
            view.outcome(&request).is_some()
        })?;
        match self.view.outcome(&request) {
            Some(Ok(message)) => Ok(message.clone()),
            other => bail!("the switch failed: {other:?}"),
        }
    }
}

fn write_private(path: &Path, text: &str, mode: u32) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn run_entry(agent: &Path, log: &Path) -> anyhow::Result<Vec<String>> {
    let settings: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
    let entry = settings.pointer(&format!("/mcpServers/plugin-{ID}")).context("no plugin entry in settings.json")?;
    let command = entry["command"].as_str().context("entry command")?;
    let args: Vec<&str> = entry["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut child = Command::new(command);
    child.args(&args).env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default());
    if let Some(cwd) = entry["cwd"].as_str() {
        child.current_dir(cwd);
    }
    for (name, value) in entry["env"].as_object().into_iter().flatten() {
        let host = value["env"].as_str().with_context(|| format!("env {name} is not a host variable reference"))?;
        child.env(name, std::env::var_os(host).unwrap_or_default());
    }
    let before = std::fs::read_to_string(log).unwrap_or_default().lines().count();
    let mut child = child.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn()?;
    let mut stdin = child.stdin.take().context("stdin")?;
    stdin.write_all(
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"granted_report\"}}\n",
    )?;
    drop(stdin);
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "the server from the entry exited with {}", output.status);
    let reply: Value = serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap_or_default())?;
    let text = reply.pointer("/result/content/0/text").and_then(Value::as_str).context("no tool reply")?;
    let lines: Vec<String> = std::fs::read_to_string(log)?.lines().skip(before).map(str::to_string).collect();
    ensure!(lines.len() == 1, "the server logged {} starts, not 1", lines.len());
    let row: Value = serde_json::from_str(&lines[0])?;
    let seen: Vec<String> =
        row["granted"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    let told: Vec<String> = text.split(',').filter(|name| !name.is_empty()).map(str::to_string).collect();
    ensure!(seen == told, "the server's start log {seen:?} and its tool reply {told:?} differ");
    Ok(seen)
}

fn entry_env(agent: &Path) -> anyhow::Result<Vec<String>> {
    let settings: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
    let env = settings.pointer(&format!("/mcpServers/plugin-{ID}/env")).and_then(Value::as_object);
    Ok(env.into_iter().flat_map(Map::keys).filter(|key| key.starts_with("LOCAL_STUDIO_GRANTED_")).cloned().collect())
}

fn stop_daemon(socket: &Path) -> anyhow::Result<()> {
    if !socket.exists() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(socket).await?;
        client
            .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
            .await?;
        println!("ok   scratch daemon stopped");
        anyhow::Ok(())
    })
}

fn fixture(plugins: &Path, log: &Path) -> anyhow::Result<()> {
    let dir = plugins.join(ID);
    std::fs::create_dir_all(&dir)?;
    let manifest = format!(
        "id = \"{ID}\"\nname = \"Granted\"\nversion = \"1.0.0\"\ndescription = \"Reports the sensitive permissions it holds\"\ncapabilities = [\"mcp-server\", \"screen-capture\", \"site-secrets\"]\n\n[gates]\nscreen-capture = \"page screenshots\"\n\n[mcp]\ncommand = \"server.py\"\nargs = [{:?}]\n",
        log.to_string_lossy()
    );
    write_private(&dir.join("plugin.toml"), &manifest, 0o644)?;
    write_private(&dir.join("server.py"), SERVER, 0o755)?;
    let plugin = plugin_store::load(&dir).map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
    let _lock = Grants::lock(plugins).map_err(anyhow::Error::msg)?;
    let mut grants = Grants::load(plugins).map_err(anyhow::Error::msg)?;
    grants.grant(&plugin, &[Capability::McpServer], None).map_err(anyhow::Error::msg)?;
    grants.save(plugins).map_err(anyhow::Error::msg)?;
    println!("ok   {ID} asks for mcp-server, screen-capture and site-secrets; only mcp-server is granted");
    Ok(())
}

fn drive(harness: &mut Harness, agent: &Path, work: &Path, log: &Path) -> anyhow::Result<()> {
    harness.pump_until("daemon connected", STEP, |view| matches!(view.connection, Some(Connection::Ready)))?;
    harness.pump_until("Python kernel installed (MCP servers run in it)", KERNEL, |view| {
        matches!(view.kernel, Some(KernelState::Installed { .. }))
    })?;
    harness.studio.add_project(work.to_string_lossy().into_owned());
    harness.pump_until("project added", STEP, |view| view.projects.len() == 1)?;
    let project = harness.view.projects[0].id.clone();
    harness.studio.list_plugins();
    harness.pump_until("Settings, Plugins lists the plugin", STEP, |view| {
        view.plugins.iter().any(|plugin| plugin.id == ID)
    })?;
    let row = harness.view.plugins.iter().find(|plugin| plugin.id == ID).context("row")?;
    println!("     row: ready={} status={:?} granted={:?}", row.ready, row.status, row.granted);
    ensure!(row.ready && row.granted == ["mcp-server"], "the plugin is not on with only mcp-server: {row:?}");
    let env = entry_env(agent)?;
    ensure!(env.is_empty(), "the settings.json entry has granted variables before any grant: {env:?}");
    println!("ok   plugin-{ID} is in settings.json with no LOCAL_STUDIO_GRANTED_* variable");

    harness.turn("none", &project)?;
    let seen = run_entry(agent, log)?;
    ensure!(seen.is_empty(), "with no sensitive grant the server saw {seen:?}");
    println!("ok   the plugin server started from the core's settings.json entry runs and sees an empty granted list");

    let message = harness.switch("screen-capture", true)?;
    println!("     {message}");
    let env = entry_env(agent)?;
    ensure!(env == ["LOCAL_STUDIO_GRANTED_SCREEN_CAPTURE"], "after the switch the entry has {env:?}");
    harness.turn("screen", &project)?;
    let seen = run_entry(agent, log)?;
    ensure!(
        seen == ["LOCAL_STUDIO_GRANTED_SCREEN_CAPTURE"],
        "after screen-capture was granted the new server saw {seen:?}"
    );
    println!("ok   after Settings turned screen-capture on, the server started from the entry sees only LOCAL_STUDIO_GRANTED_SCREEN_CAPTURE");

    let message = harness.switch("screen-capture", false)?;
    println!("     {message}");
    ensure!(entry_env(agent)?.is_empty(), "after the switch went off the entry still has a granted variable");
    harness.turn("off", &project)?;
    let seen = run_entry(agent, log)?;
    ensure!(seen.is_empty(), "after screen-capture was turned off the new server saw {seen:?}");
    println!(
        "ok   after Settings turned screen-capture off, the server started from the entry sees an empty granted list again"
    );

    harness.studio.load_usage("usage".to_string(), UsagePeriod::Day);
    harness.pump_until("usage report loaded", STEP, |view| view.usage.is_some())?;
    real_model::check_usage(harness.view.usage.as_ref().context("usage")?)
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: granted_smoke <eukhe> <scratch dir> | granted_smoke --stop <socket>";
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.first().is_some_and(|arg| arg.as_os_str() == "--stop") {
        return stop_daemon(args.get(1).context(usage)?);
    }
    let [daemon, base] = args.as_slice() else { bail!(usage) };
    std::fs::create_dir_all(base)?;
    let base = base.canonicalize()?;
    let root = base.join(format!("r{}", std::process::id()));
    let (data, agent, work, home) = (root.join("data"), root.join("agent"), root.join("work"), base.join("home"));
    let plugins = data.join("plugins");
    for dir in [&plugins, &agent, &work, &home] {
        std::fs::create_dir_all(dir)?;
    }
    Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
    real_model::install(&agent)?;
    let log = root.join("granted.log");
    fixture(&plugins, &log)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    let socket = root.join("d.sock");
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        controller_main: None,
        take_over_serve: false,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
    };
    let signal = Arc::new(Signal::default());
    let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
    let mut harness = Harness { studio, signal, view: View::default() };
    let outcome = drive(&mut harness, &agent, &work, &log);
    harness.studio.shutdown();
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped?;
    println!("PASS granted smoke ({})", root.display());
    Ok(())
}
