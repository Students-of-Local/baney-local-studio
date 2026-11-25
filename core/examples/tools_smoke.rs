use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, Delivery, PromptDraft, Settings, Studio, StudioConfig, ThreadRow,
    ThreadStatus, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};

#[path = "support/real_model.rs"]
#[allow(dead_code)]
mod real_model;

const STEP: Duration = Duration::from_mins(15);
const TRIES: usize = 4;
const DRAFT: &str = "tools";
const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
LOG = "@LOG@"
def put(entry):
    entry["pid"] = os.getpid()
    with open(LOG, "a") as out:
        out.write(json.dumps(entry) + "\n")
def send(ident, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": ident, "result": result}) + "\n")
    sys.stdout.flush()
put({"args": sys.argv[1:], "display": os.environ.get("DISPLAY"), "other": os.environ.get("TOOLS_SMOKE_OTHER")})
TOOL = {"name": "note", "description": "Writes down one word.",
        "inputSchema": {"type": "object", "properties": {"word": {"type": "string"}}, "required": ["word"]}}
for line in sys.stdin:
    request = json.loads(line)
    method, ident, params = request.get("method"), request.get("id"), request.get("params") or {}
    if ident is None:
        continue
    if method == "initialize":
        send(ident, {"protocolVersion": params.get("protocolVersion") or "2024-11-05", "capabilities": {"tools": {}},
                     "serverInfo": {"name": "stub", "version": "1.0.0"}})
    elif method == "tools/list":
        send(ident, {"tools": [TOOL]})
    elif method == "tools/call":
        put({"word": (params.get("arguments") or {}).get("word")})
        send(ident, {"content": [{"type": "text", "text": "written down"}]})
    else:
        send(ident, {})
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
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    started: HashMap<String, String>,
    settings: Option<Settings>,
    usage: Option<UsageReport>,
}

impl View {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Connection { state } => self.connection = Some(state),
            Update::Projects { projects } => self.projects = projects.into_iter().map(|project| project.id).collect(),
            Update::Threads { threads } => self.threads = threads,
            Update::ThreadStarted { draft, thread } => {
                self.started.insert(draft, thread);
            }
            Update::Settings { settings } => self.settings = Some(settings),
            Update::Usage { report, .. } => self.usage = Some(report),
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestFailed { request, message } => println!("     request {request} failed: {message}"),
            _ => {}
        }
    }

    fn idle(&self) -> bool {
        self.started
            .get(DRAFT)
            .and_then(|id| self.threads.iter().find(|row| &row.id == id))
            .is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }
}

struct Run {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    ours: PathBuf,
    own: PathBuf,
    sent: usize,
}

impl Run {
    fn until(&mut self, step: &str, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + STEP;
        loop {
            for update in self.studio.tick() {
                self.view.apply(update);
            }
            if done(&self.view) {
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("timed out waiting for: {step}");
            };
            let pending = self.signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut pending, _) = self
                .signal
                .ready
                .wait_timeout_while(pending, left.min(Duration::from_millis(200)), |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner);
            *pending = false;
        }
    }

    fn turn_on(&mut self, browser_use: bool, computer_use: bool) -> anyhow::Result<()> {
        let now = self.view.settings.clone().context("no settings")?;
        self.studio.update_settings(Settings { browser_use, computer_use, ..now });
        self.until("the settings change", |view| {
            view.settings.as_ref().is_some_and(|now| now.browser_use == browser_use && now.computer_use == computer_use)
        })
    }

    fn note(&mut self, project: &str, word: &str) -> anyhow::Result<()> {
        let text = format!(
            "Call the tool named python (not codemode, not dyn) with exactly this code, then reply with the single word done.\n\nr = await mcp.call_tool(\"local-studio\", \"note\", {{\"word\": \"{word}\"}})\nprint(r)"
        );
        for attempt in 1..=TRIES {
            let draft = PromptDraft { text: text.clone(), images: Vec::new(), files: Vec::new() };
            self.sent += 1;
            let request = format!("note-{}", self.sent);
            if let Some(thread) = self.view.started.get(DRAFT).cloned() {
                self.studio.send(request, thread, draft, Delivery::Queue);
                self.until("the turn starts", |view| !view.idle())?;
            } else {
                let settings = real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local);
                self.studio.start_thread(DRAFT.to_string(), Some(project.to_string()), draft, settings);
                self.until("the thread starts", |view| view.started.contains_key(DRAFT))?;
            }
            self.until("the turn ends", View::idle)?;
            if [&self.ours, &self.own].iter().any(|log| noted(log, word)) {
                return Ok(());
            }
            println!("     turn {attempt} of {TRIES}: the model did not call note with {word}");
        }
        bail!("the model never called note with {word}")
    }

    fn expect(&self, word: &str, tools: Option<&str>) -> anyhow::Result<()> {
        let thread = self.view.started.get(DRAFT).context("no thread")?;
        let (wanted, other) = if tools.is_some() { (&self.ours, &self.own) } else { (&self.own, &self.ours) };
        ensure!(noted(wanted, word) && !noted(other, word), "{word} reached the wrong server");
        let Some(tools) = tools else {
            println!("ok   {word}: the person's own local-studio server got the call");
            return Ok(());
        };
        let started = lines(wanted).into_iter().rfind(|entry| entry.get("args").is_some()).context("no start")?;
        let args =
            json!(["mcp", "--name", "local-studio", "--tools", tools, "--session", thread, "--profile", "small"]);
        ensure!(started["args"] == args, "the program started with {} and not {args}", started["args"]);
        ensure!(started["display"] == ":smoke" && started["other"].is_null(), "wrong environment: {started}");
        println!("ok   {word}: Local Studio's program got the call with --tools {tools}");
        Ok(())
    }
}

fn lines(log: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

fn noted(log: &Path, word: &str) -> bool {
    lines(log).iter().any(|entry| entry["word"] == word)
}

fn program(path: &Path, log: &Path) -> anyhow::Result<()> {
    std::fs::write(path, SERVER.replace("@LOG@", &log.to_string_lossy()))?;
    Ok(std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?)
}

fn drive(run: &mut Run, work: &Path, bin: &Path) -> anyhow::Result<()> {
    let ours = run.ours.clone();
    run.until("the core is ready", |view| {
        matches!(view.connection, Some(Connection::Ready)) && view.settings.is_some()
    })?;
    run.studio.add_project(work.to_string_lossy().into_owned());
    run.until("the project", |view| !view.projects.is_empty())?;
    let project = run.view.projects[0].clone();
    let settings = run.view.settings.as_ref().context("no settings")?;
    ensure!(!settings.browser_use && !settings.computer_use, "browser or computer use is on by default");
    run.note(&project, "willow")?;
    run.expect("willow", None)?;
    ensure!(lines(&run.ours).is_empty(), "the program started while both were off");
    for (word, browser, computer, tools) in [
        ("copper", true, false, "browser"),
        ("harbor", true, true, "browser,computer"),
        ("lantern", false, true, "computer"),
    ] {
        run.turn_on(browser, computer)?;
        run.note(&project, word)?;
        run.expect(word, Some(tools))?;
    }
    let running = || {
        let pid = lines(&ours).iter().rev().find_map(|entry| entry["pid"].as_i64()).unwrap_or_default().to_string();
        std::process::Command::new("kill").args(["-0", &pid]).stderr(std::process::Stdio::null()).status()
    };
    ensure!(running()?.success(), "the program is not running while computer use is on");
    run.turn_on(false, false)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while running()?.success() {
        ensure!(Instant::now() < deadline, "the program still runs 30 s after both were turned off");
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("ok   turning both off stopped the program before another message");
    run.note(&project, "meadow")?;
    run.expect("meadow", None)?;
    std::fs::rename(bin, bin.with_extension("away"))?;
    run.turn_on(true, true)?;
    run.note(&project, "granite")?;
    run.expect("granite", None)?;
    run.studio.load_usage("usage".to_string(), UsagePeriod::Day);
    run.until("the usage report", |view| view.usage.is_some())?;
    real_model::check_usage(run.view.usage.as_ref().context("no usage report")?)
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: tools_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let eukhe = std::fs::canonicalize(PathBuf::from(args.next().context(usage)?))?;
    let root = PathBuf::from(args.next().context(usage)?);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let socket = root.join("d.sock");
    let root = root.join(format!("r{}", std::process::id()));
    let [agent, data, home, work] = ["agent", "data", "home", "work"].map(|name| root.join(name));
    for dir in [&agent, &data, &home, &work] {
        std::fs::create_dir_all(dir)?;
    }
    let (bin, ours) = (root.join("local-studio-browser"), root.join("ours.jsonl"));
    let (person, own) = (root.join("person.py"), root.join("own.jsonl"));
    program(&bin, &ours)?;
    program(&person, &own)?;
    real_model::install(&agent)?;
    let path = agent.join("settings.json");
    let mut settings: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let declared = json!({ "local-studio": { "type": "stdio", "command": person, "args": [] } });
    settings["mcpServers"] = declared.clone();
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    let status = std::process::Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
    ensure!(status.success(), "git init failed");
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    std::env::set_var("LOCAL_STUDIO_BROWSER_BIN", &bin);
    std::env::set_var("DISPLAY", ":smoke");
    std::env::set_var("TOOLS_SMOKE_OTHER", "kept from the program");
    let signal = Arc::new(Signal::default());
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: Some(data.to_string_lossy().into_owned()),
        launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
        controller_main: None,
        take_over_serve: false,
    };
    let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
    let mut run = Run { studio, signal, view: View::default(), ours, own, sent: 0 };
    let outcome = drive(&mut run, &work, &bin);
    run.studio.shutdown();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let stopped = runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(&socket).await?;
        let command = DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() };
        client.request_ok(command).await.map(|_| ())
    });
    outcome?;
    stopped?;
    let settings: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    ensure!(settings["mcpServers"] == declared, "settings.json mcpServers changed: {}", settings["mcpServers"]);
    println!("ok   the person's own settings.json entry is as it was");
    println!("PASS tools smoke ({})", root.display());
    Ok(())
}
