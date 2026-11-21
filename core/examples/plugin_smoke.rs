use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::plugins::{self as plugin_store, Capability, Catalog, Consent, Grants, Source};
use local_studio_core::{
    AccessMode, CommandKind, Connection, DaemonLaunch, Item, ItemKind, ModelKey, Project, PromptDraft, SkillEntry,
    SlashCommand, Studio, StudioConfig, ThreadRow, ThreadSettings, ThreadStatus, ToolState, TranscriptOp, Update,
    UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};

const PROVIDER: &str = "local-fleet";
const MID: &str = "hf.co/LiquidAI/LFM2.5-8B-A1B-GGUF:Q4_K_M";
const SMALL: &str = "hf.co/LiquidAI/LFM3-1.7B-GGUF:Q4_K_M";
const MAX_TOKENS: u64 = 16_384;
const STEP: Duration = Duration::from_secs(120);
const BUILD: Duration = Duration::from_mins(25);
const REQUEST: &str = "a fan guard for a 40 mm fan. Run the stone commands with the bash tool in the current directory and stop when stone says the part is ready.";

struct Target {
    id: String,
    command: String,
    model: String,
    request: String,
    expanded: String,
    skill: String,
    mark: String,
    check: Option<String>,
    step: Duration,
    build: Duration,
}

impl Target {
    fn from_env() -> anyhow::Result<Self> {
        let set = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let var = |name: &str, default: &str| set(name).unwrap_or_else(|| default.to_string());
        let secs = |name: &str, default: Duration| -> anyhow::Result<Duration> {
            set(name).map_or(Ok(default), |value| {
                value.trim().parse().map(Duration::from_secs).with_context(|| format!("{name} must be whole seconds"))
            })
        };
        Ok(Self {
            id: var("PLUGIN_SMOKE_ID", "smoothstone"),
            command: var("PLUGIN_SMOKE_COMMAND", "stone"),
            model: var("PLUGIN_SMOKE_MODEL", MID),
            request: var("PLUGIN_SMOKE_REQUEST", REQUEST),
            expanded: var("PLUGIN_SMOKE_EXPANDED", "stone status"),
            skill: var("PLUGIN_SMOKE_SKILL", "smoothstone"),
            mark: var("PLUGIN_SMOKE_MARK", "NEXT:"),
            check: set("PLUGIN_SMOKE_CHECK"),
            step: secs("PLUGIN_SMOKE_STEP_SECS", STEP)?,
            build: secs("PLUGIN_SMOKE_BUILD_SECS", BUILD)?,
        })
    }

    fn source(&self) -> String {
        format!("plugin:{}", self.id)
    }
}

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

impl Signal {
    fn wait(&self, timeout: Duration) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
}

#[derive(Default)]
struct View {
    connection: Option<Connection>,
    projects: Vec<Project>,
    threads: Vec<ThreadRow>,
    started: Vec<(String, String)>,
    transcripts: Vec<(String, Vec<Item>)>,
    notices: Vec<String>,
    commands: Option<(String, Vec<SlashCommand>)>,
    skills: Option<Vec<SkillEntry>>,
    refusals: Vec<(String, String, Option<ModelKey>)>,
    usage: Option<UsageReport>,
}

impl View {
    fn thread(&self, draft: &str) -> Option<String> {
        self.started.iter().find(|(key, _)| key == draft).map(|(_, thread)| thread.clone())
    }

    fn items(&self, thread: &str) -> &[Item] {
        self.transcripts.iter().find(|(key, _)| key == thread).map_or(&[], |(_, items)| items.as_slice())
    }

    fn idle(&self, thread: &str) -> bool {
        self.threads
            .iter()
            .find(|row| row.id == thread)
            .is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }

    fn commands(&self, request: &str) -> Option<&[SlashCommand]> {
        self.commands.as_ref().filter(|(key, _)| key == request).map(|(_, commands)| commands.as_slice())
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    step: Duration,
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
                bail!("timed out waiting for: {step}");
            };
            self.signal.wait(left.min(Duration::from_millis(500)));
        }
    }

    fn apply(&mut self, update: Update) -> anyhow::Result<()> {
        match update {
            Update::Connection { state } => self.view.connection = Some(state),
            Update::Projects { projects } => self.view.projects = projects,
            Update::Threads { threads } => self.view.threads = threads,
            Update::ThreadStarted { draft, thread } => self.view.started.push((draft, thread)),
            Update::Notice { level, text } => {
                println!("     notice {level:?}: {text}");
                self.view.notices.push(text);
            }
            Update::RequestFailed { request, message } => println!("     request {request} failed: {message}"),
            Update::Commands { request, commands } => self.view.commands = Some((request, commands)),
            Update::Skills { skills, .. } => self.view.skills = Some(skills),
            Update::Transcript { thread, op } => self.transcript(&thread, op)?,
            Update::LocalOnly { thread, plugin, message, local } => {
                println!("     local-only refusal for {plugin}: {message}");
                self.view.refusals.push((thread, message, local));
            }
            Update::Usage { report, .. } => self.view.usage = Some(report),
            _ => {}
        }
        Ok(())
    }

    fn transcript(&mut self, thread: &str, op: TranscriptOp) -> anyhow::Result<()> {
        if !self.view.transcripts.iter().any(|(key, _)| key == thread) {
            self.view.transcripts.push((thread.to_string(), Vec::new()));
        }
        let items = &mut self.view.transcripts.iter_mut().find(|(key, _)| key == thread).context("transcript")?.1;
        match op {
            TranscriptOp::Reset { items: fresh } => *items = fresh,
            TranscriptOp::Insert { index, item } => {
                let index = usize::try_from(index)?;
                ensure!(index <= items.len(), "insert at {index} past {} items", items.len());
                items.insert(index, item);
            }
            TranscriptOp::Update { index, item } => {
                let index = usize::try_from(index)?;
                *items.get_mut(index).context("update past the end")? = item;
            }
        }
        Ok(())
    }

    fn list_commands(&mut self, request: &str) -> anyhow::Result<Vec<SlashCommand>> {
        self.studio.list_commands(request.to_string(), None);
        self.pump_until(&format!("command list {request}"), self.step, |view| view.commands(request).is_some())?;
        Ok(self.view.commands(request).unwrap_or_default().to_vec())
    }
}

fn settings(model: &str) -> ThreadSettings {
    ThreadSettings {
        provider: Some(PROVIDER.to_string()),
        model: Some(model.to_string()),
        thinking_level: None,
        access: AccessMode::FullAccess,
        worktree: WorktreeChoice::Local,
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn user_text(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn plugin_calls(items: &[Item], mark: &str) -> usize {
    items
        .iter()
        .filter(|item| {
            matches!(&item.kind, ItemKind::Tool { state: ToolState::Succeeded | ToolState::Failed, output, .. } if output.contains(mark))
        })
        .count()
}

fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(data)?;
    Ok(())
}

fn agent_dir(root: &Path, models: &Path, model: &str) -> anyhow::Result<PathBuf> {
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent)?;
    let mut catalog: Value = serde_json::from_slice(&std::fs::read(models)?)?;
    let provider = catalog
        .get_mut("providers")
        .and_then(|providers| providers.get_mut(PROVIDER))
        .with_context(|| format!("{} has no {PROVIDER} provider", models.display()))?
        .clone();
    let mut provider = provider;
    for model in provider.get_mut("models").and_then(Value::as_array_mut).context("models")? {
        model["maxTokens"] = json!(MAX_TOKENS);
    }
    let catalog = json!({ "providers": { PROVIDER: provider } });
    write_private(&agent.join("models.json"), &serde_json::to_vec_pretty(&catalog)?)?;
    write_private(
        &agent.join("settings.json"),
        &serde_json::to_vec_pretty(&json!({ "defaultProvider": PROVIDER, "defaultModel": model }))?,
    )?;
    write_private(&agent.join("auth.json"), b"{}")?;
    println!("     scratch agent dir {} with only {PROVIDER} (auth.json is {{}})", agent.display());
    Ok(agent)
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: [PLUGIN_SMOKE_ID=smoothstone PLUGIN_SMOKE_COMMAND=stone PLUGIN_SMOKE_MODEL=<model> PLUGIN_SMOKE_REQUEST=<text after /command> PLUGIN_SMOKE_EXPANDED='stone status' PLUGIN_SMOKE_SKILL=smoothstone PLUGIN_SMOKE_MARK='NEXT:' PLUGIN_SMOKE_CHECK=<sh command run in the work dir> PLUGIN_SMOKE_STEP_SECS=120 PLUGIN_SMOKE_BUILD_SECS=1500] plugin_smoke <eukhe> <scratch dir> <models.json> <plugin dir | catalog id> | plugin_smoke --stop <socket>";
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.first().is_some_and(|arg| arg.as_os_str() == "--stop") {
        return stop_daemon(args.get(1).context(usage)?);
    }
    if args.first().is_some_and(|arg| arg.as_os_str() == "--local-only") {
        let [_, daemon, root] = args.as_slice() else { bail!(LOCAL_USAGE) };
        return local_only::run(daemon, root);
    }
    let [daemon, root, models, plugin] = args.as_slice() else { bail!(usage) };
    let target = Target::from_env()?;
    ensure!(
        target.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "PLUGIN_SMOKE_ID must be a plain plugin id, not a path: {:?}",
        target.id
    );
    let agent = agent_dir(root, models, &target.model)?;
    let data = root.join("data");
    let plugins = data.join("plugins");
    if plugin.is_dir() {
        copy_dir(plugin, &plugins.join(&target.id))?;
    } else {
        install_from_catalog(&plugins, &plugin.to_string_lossy(), &target)?;
    }
    std::fs::create_dir_all(plugins.join("broken"))?;
    std::fs::write(plugins.join("broken").join("plugin.toml"), "id = \"broken\"\nversion = 1\n")?;
    let work = root.join("work");
    std::fs::create_dir_all(&work)?;
    Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    let socket = root.join("d.sock");
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
    let mut harness = Harness { studio, signal, view: View::default(), step: target.step };
    let outcome = drive(&mut harness, &work, &plugins, &target).and_then(|()| no_plugin_servers(&agent));
    harness.studio.shutdown();
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped?;
    println!("PASS plugin smoke");
    Ok(())
}

fn install_from_catalog(plugins: &Path, id: &str, target: &Target) -> anyhow::Result<()> {
    ensure!(id == target.id, "catalog id {id} is not PLUGIN_SMOKE_ID {}", target.id);
    let catalog = Catalog::bundled().map_err(anyhow::Error::msg)?;
    let entry = catalog.entry(id).with_context(|| format!("{id} is not in the bundled catalog"))?;
    let installed = plugin_store::install(plugins, &Source::Catalog(id.to_string()), &catalog)
        .map_err(|errors| anyhow::anyhow!("catalog install refused: {}", errors.join("; ")))?;
    ensure!(installed.tree == entry.tree, "installed tree {} is not the pinned {}", installed.tree, entry.tree);
    ensure!(installed.plugin.dir == plugins.join(id), "installed into {}", installed.plugin.dir.display());
    ensure!(installed.consent == Consent::Ready, "{id} needs consent: {:?}", installed.consent);
    println!(
        "ok   check 1: {id} {} installed through the catalog at {} ({} files, {})",
        installed.plugin.version,
        &entry.rev[..12],
        installed.files,
        installed.tree
    );
    Ok(())
}

fn no_plugin_servers(agent: &Path) -> anyhow::Result<()> {
    let settings: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
    let servers = settings.get("mcpServers").and_then(Value::as_object);
    ensure!(
        servers.is_none_or(|servers| !servers.keys().any(|name| name.starts_with(plugin_store::MCP_PREFIX))),
        "a plugin without the mcp-server capability got an mcpServers entry: {servers:?}"
    );
    println!("ok   no plugin MCP server registered for a plugin that asks for none");
    Ok(())
}

fn drive(harness: &mut Harness, work: &Path, plugins: &Path, target: &Target) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    let source = target.source();
    let name = &target.command;
    let step = target.step;
    let gone_prompt = format!("/{name} Reply with only the word gone.");
    harness.pump_until("daemon connected", step, |view| matches!(view.connection, Some(Connection::Ready)))?;
    studio.add_project(work.to_string_lossy().into_owned());
    harness.pump_until("project added", step, |view| view.projects.len() == 1)?;
    let project = harness.view.projects[0].id.clone();

    let commands = harness.list_commands("before")?;
    let listed = commands
        .iter()
        .find(|command| command.name == *name)
        .with_context(|| format!("/{name} is not in the palette"))?;
    ensure!(listed.source == source && listed.kind == CommandKind::Plugin, "/{name} has the wrong source: {listed:?}");
    println!("ok   check 2 core: /{name} listed, source {}, kind {:?}", listed.source, listed.kind);
    let skill = format!("skill:{}", target.skill);
    ensure!(
        commands.iter().any(|command| command.name == skill && command.source == source),
        "the plugin skill {skill} is not in the palette"
    );
    ensure!(commands.iter().any(|command| command.name == "compact"), "session commands are missing");
    ensure!(
        harness.view.notices.iter().any(|notice| notice.contains("broken")),
        "the broken plugin raised no notice: {:?}",
        harness.view.notices
    );
    println!("ok   check 5: broken plugin reported in a notice, {} and session commands still listed", target.id);

    studio.start_thread(
        "stone".to_string(),
        Some(project.clone()),
        prompt(&format!("/{name} {}", target.request)),
        settings(&target.model),
    );
    harness.pump_until("plugin thread started", step, |view| view.thread("stone").is_some())?;
    let thread = harness.view.thread("stone").context("no plugin thread")?;
    studio.select_thread(Some(thread.clone()));
    harness.pump_until(&format!("the /{name} prompt reached the model expanded"), step, |view| {
        user_text(view.items(&thread))
            .iter()
            .any(|text| text.contains(target.expanded.as_str()) && text.contains(target.request.as_str()))
    })?;
    let started = Instant::now();
    harness.pump_until(&format!("the /{name} turn finished on {}", target.model), target.build, |view| {
        view.idle(&thread)
    })?;
    studio.select_thread(Some(thread.clone()));
    harness.pump_until("plugin transcript reloaded", step, |view| !view.items(&thread).is_empty())?;
    let calls = plugin_calls(harness.view.items(&thread), &target.mark);
    println!("     {calls} plugin tool calls in {:?}", started.elapsed());
    for item in harness.view.items(&thread).iter().rev().take(2) {
        println!("     last item {:.400}", format!("{:?}", item.kind));
    }
    ensure!(calls > 0, "the /{name} turn ended without a plugin tool call");
    match &target.check {
        Some(check) => run_check(work, check)?,
        None => check_part(work)?,
    }
    std::fs::remove_dir_all(plugins.join(&target.id))?;
    let commands = harness.list_commands("after")?;
    ensure!(!commands.iter().any(|command| command.source == source), "the removed plugin is still listed");
    ensure!(commands.iter().any(|command| command.name == "compact"), "session commands vanished with the plugin");
    studio.start_thread("gone".to_string(), Some(project), prompt(&gone_prompt), settings(SMALL));
    harness.pump_until("gone thread started", step, |view| view.thread("gone").is_some())?;
    let gone = harness.view.thread("gone").context("no gone thread")?;
    studio.select_thread(Some(gone.clone()));
    harness.pump_until("the next turn after removal finished", step * 3, |view| {
        view.idle(&gone) && !user_text(view.items(&gone)).is_empty()
    })?;
    let sent = user_text(harness.view.items(&gone));
    ensure!(sent.iter().all(|text| text.trim() == gone_prompt), "/{name} still expanded after removal: {sent:?}");
    println!("ok   check 4: plugin folder removed, /{name} gone from the palette and sent as plain text on the next turn, no restart");
    Ok(())
}

fn run_check(work: &Path, check: &str) -> anyhow::Result<()> {
    let output = Command::new("sh").arg("-c").arg(check).current_dir(work).output().context("run the plugin check")?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    ensure!(output.status.success(), "the plugin check failed: {}", output.status);
    println!("ok   check 3: the plugin check passed: {check}");
    Ok(())
}

fn check_part(work: &Path) -> anyhow::Result<()> {
    let job = work.join("stone-job");
    let status: Value = serde_json::from_slice(&std::fs::read(job.join("status.json")).context("no stone job")?)?;
    println!("     stone status: {}", status.get("last_result").unwrap_or(&Value::Null));
    let stl = job.join("fan_guard.stl");
    ensure!(stl.is_file(), "no fan_guard.stl in {}", job.display());
    let output = Command::new("stone").arg("check").arg(&stl).output().context("run stone check")?;
    let text = String::from_utf8_lossy(&output.stdout);
    print!("{text}");
    ensure!(output.status.success() && !text.contains("FAIL"), "stone check rejected {}", stl.display());
    println!("ok   check 3: {} passes stone check", stl.display());
    Ok(())
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

const LOCAL_USAGE: &str =
    "usage: plugin_smoke --local-only <eukhe> <scratch dir> (needs Ollama on 127.0.0.1:11434 with the LFM2.5 8B model)";

mod local_only {
    use super::{
        ensure, json, plugin_store, prompt, stop_daemon, user_text, write_private, Arc, AtomicUsize, Capability,
        Command, Connection, Context, DaemonClient, DaemonCommand, DaemonLaunch, Duration, Grants, Harness, Instant,
        Map, OpenOptionsExt, Ordering, Path, PathBuf, Read, Signal, Studio, StudioConfig, TcpListener, ThreadSettings,
        ThreadStatus, UsagePeriod, UsageReport, Value, View, Waker, Write, MAX_TOKENS, MID, STEP,
    };

    const LOCAL: &str = "ollama-local";
    const CLOUD: &str = "cloud-stub";
    const CLOUD_MODEL: &str = "stub-1";
    const CLOUD_HOST: &str = "localtest.me";
    const MARK: &str = "LEDGER-SKILL-7Q";
    const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
name = sys.argv[1]
out = os.environ.get("LEDGER_OUT") or sys.argv[2]
with open(out, "a") as f:
    f.write(json.dumps({"server": name, "pid": os.getpid(), "LOCAL_STUDIO_LOCAL_ONLY": os.environ.get("LOCAL_STUDIO_LOCAL_ONLY")}) + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": name.replace("-", "_"), "description": "Fixture tool for " + name, "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "ok"}]}
    elif method == "ping":
        result = {}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": str(method)}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#;

    fn executable(path: &Path, text: &str) -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(path)?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }

    fn listen() -> anyhow::Result<(u16, Arc<AtomicUsize>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buffer = vec![0_u8; 65_536];
                let _ = stream.read(&mut buffer);
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        Ok((port, hits))
    }

    fn agent(root: &Path, port: u16, web: &Path) -> anyhow::Result<PathBuf> {
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent)?;
        let models = json!({ "providers": {
            LOCAL: {
                "name": "Ollama on this machine",
                "baseUrl": "http://127.0.0.1:11434/v1",
                "api": "openai-completions",
                "apiKey": "ollama",
                "models": [{ "id": MID, "name": "LFM2.5 8B", "contextWindow": 32768, "maxTokens": MAX_TOKENS }],
            },
            CLOUD: {
                "name": "Cloud stub",
                "baseUrl": format!("http://{CLOUD_HOST}:{port}/v1"),
                "api": "openai-completions",
                "apiKey": "none",
                "models": [{ "id": CLOUD_MODEL, "name": "Stub", "contextWindow": 32768, "maxTokens": 1024 }],
            },
        }});
        write_private(&agent.join("models.json"), &serde_json::to_vec_pretty(&models)?)?;
        let settings = json!({
            "defaultProvider": LOCAL,
            "defaultModel": MID,
            "mcpServers": { "web-search": {
                "type": "stdio",
                "command": web.join("server.py").to_string_lossy(),
                "args": ["web-search", web.join("started.jsonl").to_string_lossy()],
            }},
        });
        write_private(&agent.join("settings.json"), &serde_json::to_vec_pretty(&settings)?)?;
        write_private(&agent.join("auth.json"), b"{}")?;
        println!(
            "     scratch agent dir {}: {LOCAL} at 127.0.0.1:11434, {CLOUD} at {CLOUD_HOST}:{port}, auth.json is {{}}",
            agent.display()
        );
        Ok(agent)
    }

    fn put_server(agent: &Path, name: &str, server: Option<Value>) -> anyhow::Result<()> {
        let path = agent.join("settings.json");
        let mut settings: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let servers = settings.get_mut("mcpServers").and_then(Value::as_object_mut).context("mcpServers")?;
        match server {
            Some(server) => servers.insert(name.to_string(), server),
            None => servers.remove(name),
        };
        write_private(&path, &serde_json::to_vec_pretty(&settings)?)?;
        Ok(())
    }

    fn plugin(plugins: &Path) -> anyhow::Result<()> {
        let dir = plugins.join("ledger");
        std::fs::create_dir_all(dir.join("skills").join("ledger"))?;
        std::fs::write(
            dir.join("plugin.toml"),
            r#"id = "ledger"
version = "0.1.0"
description = "Fixture plugin for the local-only smoke"
local_only = true
capabilities = ["mcp-server"]

[[commands]]
name = "ledger"
description = "File a private ledger entry"
skill = "ledger"

[mcp]
command = "server.py"
args = ["plugin-ledger"]

[mcp.env]
LEDGER_OUT = "LOCAL_STUDIO_PLUGIN_LEDGER__OUT"
"#,
        )?;
        std::fs::write(
            dir.join("skills").join("ledger").join("SKILL.md"),
            format!("---\nname: ledger\ndescription: File a private ledger entry.\n---\n\n# ledger {MARK}\n\nRun this once in the Python tool: `print(await mcp.call_tool(\"plugin-ledger\", \"plugin_ledger\"))`. Then reply with only the word filed.\n"),
        )?;
        executable(&dir.join("server.py"), SERVER)?;
        let loaded = plugin_store::load(&dir).map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        ensure!(loaded.local_only, "the fixture did not load as local_only");
        let mut grants = Grants::load(plugins).map_err(anyhow::Error::msg)?;
        grants.grant(&loaded, &[Capability::McpServer]).map_err(anyhow::Error::msg)?;
        grants.save(plugins).map_err(anyhow::Error::msg)?;
        Ok(())
    }

    fn settings(provider: &str, model: &str) -> ThreadSettings {
        ThreadSettings { provider: Some(provider.to_string()), ..super::settings(model) }
    }

    fn server_starts(path: &Path, server: &str) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|row| row.get("server").and_then(Value::as_str) == Some(server))
            .collect()
    }

    fn connections(socket: &Path, thread: &str) -> anyhow::Result<Vec<(String, bool)>> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        runtime.block_on(async {
            let (client, _events) = DaemonClient::connect(socket).await?;
            let list = client
                .request_ok(DaemonCommand::List {
                    id: None,
                    all: Some(true),
                    cwd: None,
                    session_dir: None,
                    include_client_owned: Some(true),
                    rest: Map::default(),
                })
                .await?;
            let sessions =
                list.as_array().or_else(|| list.get("sessions").and_then(Value::as_array)).context("list")?;
            let summary = sessions
                .iter()
                .map(|session| session.get("summary").unwrap_or(session))
                .find(|summary| summary.get("sessionId").and_then(Value::as_str) == Some(thread))
                .with_context(|| format!("thread {thread} is not a live session: {list}"))?;
            let active = summary
                .get("activeSessionId")
                .or_else(|| summary.get("id"))
                .and_then(Value::as_str)
                .with_context(|| format!("no active session id in {summary}"))?
                .to_string();
            let data = client
                .request_ok(DaemonCommand::GetMcpConnections {
                    id: None,
                    active_session_id: active,
                    rest: Map::default(),
                })
                .await?;
            let rows = data.get("connections").and_then(Value::as_array).cloned().unwrap_or_default();
            anyhow::Ok(
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("server")?.as_str()?.to_string(),
                            row.get("connected").and_then(Value::as_bool) == Some(true),
                        ))
                    })
                    .collect(),
            )
        })
    }

    fn usage(harness: &mut Harness, request: &str) -> anyhow::Result<UsageReport> {
        harness.view.usage = None;
        harness.studio.load_usage(request.to_string(), UsagePeriod::Day);
        harness.pump_until(&format!("usage report {request}"), harness.step, |view| view.usage.is_some())?;
        let report = harness.view.usage.clone().context("usage")?;
        for row in &report.by_model {
            println!("     usage {}/{}: {} tokens, {} requests", row.provider, row.model, row.tokens, row.requests);
        }
        Ok(report)
    }

    fn refused(
        harness: &mut Harness,
        key: &str,
        text: &str,
        on: ThreadSettings,
        hits: &AtomicUsize,
    ) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let (start, before) = (hits.load(Ordering::SeqCst), cloud_tokens(&usage(harness, &format!("{key}-before"))?));
        let project = harness.view.projects[0].id.clone();
        studio.start_thread(key.to_string(), Some(project), prompt(text), on);
        harness.pump_until(&format!("{key} thread started"), STEP, |view| view.thread(key).is_some())?;
        let thread = harness.view.thread(key).context("thread")?;
        let refused = harness.pump_until(&format!("{key} refused"), Duration::from_secs(20), |view| {
            view.refusals.iter().any(|(id, _, _)| *id == thread)
        });
        std::thread::sleep(Duration::from_secs(3));
        let after = cloud_tokens(&usage(harness, &format!("{key}-after"))?);
        let sent = hits.load(Ordering::SeqCst) - start;
        studio.abort(thread.clone());
        harness.pump_until(&format!("{key} thread stopped"), STEP, |view| view.idle(&thread))?;
        let message = harness.view.refusals.iter().rev().find(|(id, _, _)| *id == thread).map(|(_, text, _)| text);
        ensure!(
            refused.is_ok() && sent == 0 && after == before,
            "{text:?}: refusal {message:?}, {sent} connections to {CLOUD_HOST}, {CLOUD} usage {before:?} -> {after:?}"
        );
        println!("     {key}: {message:?}");
        Ok(())
    }

    fn cloud_tokens(report: &UsageReport) -> (u64, u64) {
        report
            .by_model
            .iter()
            .filter(|row| row.provider == CLOUD)
            .fold((0, 0), |(tokens, requests), row| (tokens + row.tokens, requests + row.requests))
    }

    pub(super) fn run(daemon: &Path, root: &Path) -> anyhow::Result<()> {
        let (port, hits) = listen()?;
        let resolved: Vec<std::net::SocketAddr> =
            std::net::ToSocketAddrs::to_socket_addrs(&(CLOUD_HOST, port)).map(Iterator::collect).unwrap_or_default();
        ensure!(
            resolved.iter().any(|addr| addr.ip().is_loopback()),
            "{CLOUD_HOST} must resolve to loopback for the cloud stub to be observable: {resolved:?}"
        );
        let web = root.join("web");
        std::fs::create_dir_all(&web)?;
        executable(&web.join("server.py"), SERVER)?;
        let agent = agent(root, port, &web)?;
        let data = root.join("data");
        let plugins = data.join("plugins");
        plugin(&plugins)?;
        let ledger_out = root.join("ledger.jsonl");
        let work = root.join("work");
        std::fs::create_dir_all(&work)?;
        Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
        std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
        std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
        std::env::set_var("LOCAL_STUDIO_PLUGIN_LEDGER__OUT", &ledger_out);
        let socket = root.join("d.sock");
        let config = StudioConfig {
            socket_path: Some(socket.to_string_lossy().into_owned()),
            session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
            data_dir: None,
            controller_main: None,
            launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
            take_over_serve: false,
        };
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut harness = Harness { studio, signal, view: View::default(), step: STEP };
        let outcome = drive(&mut harness, &work, &agent, &socket, &ledger_out, &hits);
        harness.studio.shutdown();
        let stopped = stop_daemon(&socket);
        outcome?;
        stopped?;
        println!("PASS local-only plugin smoke");
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn drive(
        harness: &mut Harness,
        work: &Path,
        agent: &Path,
        socket: &Path,
        ledger_out: &Path,
        hits: &AtomicUsize,
    ) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let step = STEP;
        let ask = "/ledger File today's entry. You must call the python tool before you answer.";
        harness.pump_until("daemon connected", step, |view| matches!(view.connection, Some(Connection::Ready)))?;
        studio.add_project(work.to_string_lossy().into_owned());
        harness.pump_until("project added", step, |view| view.projects.len() == 1)?;
        let project = harness.view.projects[0].id.clone();

        let commands = harness.list_commands("label")?;
        let listed = commands.iter().find(|command| command.name == "ledger").context("/ledger is not listed")?;
        ensure!(
            listed.description.contains(plugin_store::LOCAL_ONLY_LABEL),
            "/ledger is listed without the label: {listed:?}"
        );
        println!("ok   label: /ledger listed as {:?}", listed.description);
        let settings_json: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
        ensure!(
            settings_json.pointer("/mcpServers/plugin-ledger").is_none(),
            "the local-only server was written to the global settings.json"
        );
        println!("ok   the local-only plugin's MCP server is not in the global settings.json");

        studio.start_thread("web".to_string(), Some(project.clone()), prompt(ask), settings(LOCAL, MID));
        harness.pump_until("local thread with web-search started", step, |view| view.thread("web").is_some())?;
        let web_thread = harness.view.thread("web").context("web thread")?;
        harness.pump_until("refused while web-search is connected", step, |view| {
            view.refusals.iter().any(|(thread, message, _)| *thread == web_thread && message.contains("web-search"))
        })?;
        println!("ok   check 4a: a local thread with a connected web-search MCP server refuses /ledger and names it");
        put_server(agent, "web-search", None)?;

        studio.start_thread("cloud".to_string(), Some(project.clone()), prompt(ask), settings(CLOUD, CLOUD_MODEL));
        harness.pump_until("cloud thread started", step, |view| view.thread("cloud").is_some())?;
        let cloud = harness.view.thread("cloud").context("cloud thread")?;
        harness.pump_until("refused on the cloud provider", step, |view| {
            view.refusals.iter().any(|(thread, _, _)| *thread == cloud)
        })?;
        let (_, message, local) =
            harness.view.refusals.iter().find(|(thread, _, _)| *thread == cloud).cloned().context("refusal")?;
        let local = local.context("the refusal offered no local model")?;
        ensure!(local.provider == LOCAL && local.model == MID, "offered {local:?}");
        ensure!(message.contains("runs only on local models"), "plain refusal text missing: {message}");
        studio.select_thread(Some(cloud.clone()));
        harness.pump_until("cloud thread selected", step, |view| view.threads.iter().any(|row| row.id == cloud))?;
        std::thread::sleep(Duration::from_secs(3));
        for update in studio.tick() {
            harness.apply(update)?;
        }
        let sent = user_text(harness.view.items(&cloud));
        ensure!(sent.is_empty(), "the refused prompt reached the session: {sent:?}");
        let report = usage(harness, "after-refusal")?;
        let (tokens, requests) = cloud_tokens(&report);
        ensure!(tokens == 0 && requests == 0, "{CLOUD} shows {tokens} tokens, {requests} requests");
        ensure!(hits.load(Ordering::SeqCst) == 0, "the cloud endpoint got a connection");
        println!(
            "ok   check 1: /ledger on {CLOUD} refused ({message}); transcript has no user message, usage shows {CLOUD} 0 tokens 0 requests, the {CLOUD_HOST} endpoint saw 0 connections; offered {}",
            local.model
        );

        let mut failed: Vec<String> = Vec::new();
        let mut record = |name: &str, result: anyhow::Result<()>| match result {
            Ok(()) => println!("ok   scenario {name}"),
            Err(error) => {
                println!("FAIL scenario {name}: {error:#}");
                failed.push(name.to_string());
            }
        };
        let off = json!({ "type": "stdio", "command": "/usr/bin/true", "enabled": false });
        put_server(agent, "web-off", Some(off))?;
        record("server-off", refused(harness, "off", ask, settings(LOCAL, MID), hits));
        put_server(agent, "web-off", None)?;
        ensure!(failed.is_empty(), "failed scenarios: {}", failed.join(", "));
        studio.run_local(cloud.clone());
        harness.pump_until("switched to the local model and running", step, |view| {
            view.threads.iter().any(|row| {
                row.id == cloud && row.model.as_deref() == Some(MID) && matches!(row.status, ThreadStatus::Working)
            })
        })?;
        studio.set_model(cloud.clone(), CLOUD.to_string(), CLOUD_MODEL.to_string());
        harness.pump_until("switch to cloud refused during the run", step, |view| {
            view.notices.iter().any(|notice| notice.contains("stays on local models"))
        })?;
        println!("ok   check 3: switching the running /ledger thread to {CLOUD} was refused");
        harness.pump_until("the /ledger turn finished locally", STEP * 5, |view| view.idle(&cloud))?;
        studio.select_thread(Some(cloud.clone()));
        harness.pump_until("ledger transcript reloaded", step, |view| !user_text(view.items(&cloud)).is_empty())?;
        let sent = user_text(harness.view.items(&cloud));
        ensure!(sent.iter().any(|text| text.contains(MARK)), "/ledger did not expand: {sent:?}");
        let row = harness.view.threads.iter().find(|row| row.id == cloud).context("row")?;
        ensure!(row.provider.as_deref() == Some(LOCAL) && row.model.as_deref() == Some(MID), "model moved: {row:?}");
        for item in harness.view.items(&cloud).iter().rev().take(2) {
            println!("     last item {:.300}", format!("{:?}", item.kind));
        }
        let report = usage(harness, "after-local")?;
        let local_tokens: u64 = report.by_model.iter().filter(|row| row.provider == LOCAL).map(|row| row.tokens).sum();
        ensure!(local_tokens > 0, "no local tokens counted");
        ensure!(cloud_tokens(&report) == (0, 0), "{CLOUD} usage after the local run");
        ensure!(hits.load(Ordering::SeqCst) == 0, "the cloud endpoint got a connection");
        println!("ok   check 2: the same /ledger completed on {LOCAL}/{MID} ({local_tokens} tokens), still on {MID}");

        let ledger = server_starts(ledger_out, "plugin-ledger");
        ensure!(
            !ledger.is_empty()
                && ledger.iter().all(|row| row.get("LOCAL_STUDIO_LOCAL_ONLY").and_then(Value::as_str) == Some("1")),
            "the plugin server did not start with LOCAL_STUDIO_LOCAL_ONLY=1: {ledger:?}"
        );
        let servers = connections(socket, &cloud)?;
        let connected: Vec<&str> =
            servers.iter().filter(|(name, up)| *up && name != "plugin-ledger").map(|(name, _)| name.as_str()).collect();
        ensure!(connected.is_empty(), "other MCP servers are connected in the /ledger session: {connected:?}");
        ensure!(!servers.iter().any(|(name, _)| name == "web-search"), "web-search is still in the /ledger session");
        println!(
            "ok   check 4b: the /ledger session lists {} MCP connections, none connected besides the plugin and no web-search; plugin-ledger started {} time(s), each with LOCAL_STUDIO_LOCAL_ONLY=1",
            servers.len(),
            ledger.len()
        );

        studio.start_thread(
            "control".to_string(),
            Some(project),
            prompt("Reply with only the word hi."),
            settings(CLOUD, CLOUD_MODEL),
        );
        harness.pump_until("control thread started", step, |view| view.thread("control").is_some())?;
        let control = harness.view.thread("control").context("control")?;
        let deadline = Instant::now() + step;
        while hits.load(Ordering::SeqCst) == 0 {
            ensure!(Instant::now() < deadline, "a plain prompt on {CLOUD} never reached {CLOUD_HOST}");
            for update in studio.tick() {
                harness.apply(update)?;
            }
            harness.signal.wait(Duration::from_millis(200));
        }
        studio.abort(control);
        println!(
            "ok   control: a plain prompt on {CLOUD} reached the endpoint ({} connections), so the refusal's 0 means nothing was sent",
            hits.load(Ordering::SeqCst)
        );
        Ok(())
    }
}
