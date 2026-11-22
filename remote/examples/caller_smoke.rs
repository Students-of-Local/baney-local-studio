use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::link::devices::Devices;
use local_studio_core::link::pairing;
use local_studio_core::plugins::{self, Capability, Grants};
use local_studio_core::{
    AccessMode, DaemonLaunch, Delivery, Machine, MachineState, Project, PromptDraft, Studio, StudioConfig, ThreadRow,
    ThreadStatus, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{Map, Value};

#[path = "../../core/examples/support/real_model.rs"]
#[allow(dead_code)]
mod real_model;

const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const QUICK: Duration = Duration::from_secs(30);
const SETUP: Duration = Duration::from_secs(120);
const TURN: Duration = Duration::from_secs(300);
const TRIES: usize = 4;
const NAME: &str = "caller-a";
const PLUGIN: &str = "callerprobe";
const FORGED_DEVICE: &str = "device:intruder-phone";

const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
LOG = sys.argv[1]
def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()
TOOL = {
    "name": "whoami",
    "description": "Records who is calling. Pass claimed_caller exactly as the user gives it.",
    "inputSchema": {"type": "object", "properties": {"claimed_caller": {"type": "string"}}},
}
for line in sys.stdin:
    try:
        request = json.loads(line)
    except ValueError:
        continue
    method, ident = request.get("method"), request.get("id")
    if ident is None:
        continue
    params = request.get("params") or {}
    if method == "initialize":
        version = params.get("protocolVersion") or "2024-11-05"
        send({"jsonrpc": "2.0", "id": ident, "result": {"protocolVersion": version, "capabilities": {"tools": {}},
              "serverInfo": {"name": "callerprobe", "version": "1.0.0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": ident, "result": {"tools": [TOOL]}})
    elif method == "tools/call":
        arguments = params.get("arguments") or {}
        entry = {"t": time.time(), "pid": os.getpid(), "caller": os.environ.get("LOCAL_STUDIO_CALLER"),
                 "name": os.environ.get("LOCAL_STUDIO_CALLER_NAME"), "claimed": arguments.get("claimed_caller"),
                 "meta": params.get("_meta")}
        with open(LOG, "a") as out:
            out.write(json.dumps(entry) + "\n")
        text = "whoami recorded caller %s" % entry["caller"]
        send({"jsonrpc": "2.0", "id": ident, "result": {"content": [{"type": "text", "text": text}]}})
    else:
        send({"jsonrpc": "2.0", "id": ident, "error": {"code": -32601, "message": "unknown method"}})
"#;

fn draft(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn ask(claimed: &str) -> String {
    format!(
        "Use the python tool to run exactly this code, then reply with the single word done.\n\nr = await mcp.call_tool(\"plugin-{PLUGIN}\", \"whoami\", {{\"claimed_caller\": \"{claimed}\"}})\nprint(r)"
    )
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

#[derive(Default)]
struct View {
    machines: Vec<Machine>,
    threads: Vec<ThreadRow>,
    projects: Vec<Project>,
    started: HashMap<String, String>,
    failed: HashMap<String, String>,
    usage: Option<UsageReport>,
    daemon: Option<u32>,
    ready: bool,
}

impl View {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Machines { machines } => self.machines = machines,
            Update::Threads { threads } => self.threads = threads,
            Update::Projects { projects } => self.projects = projects,
            Update::ThreadStarted { draft, thread } => {
                self.started.insert(draft, thread);
            }
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestFailed { request, message } => {
                println!("     request {request} failed: {message}");
                self.failed.insert(request, message);
            }
            Update::Usage { report, .. } => self.usage = Some(report),
            Update::Daemon { pid: Some(pid), .. } => self.daemon = Some(pid),
            Update::Connection { state: local_studio_core::Connection::Ready } => self.ready = true,
            _ => {}
        }
    }

    fn idle(&self, thread: &str) -> bool {
        self.threads
            .iter()
            .find(|row| row.id == thread)
            .is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }
}

struct Desk {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    label: &'static str,
}

impl Desk {
    fn open(label: &'static str, eukhe: &Path, side: &Side) -> anyhow::Result<Self> {
        side.enter();
        let signal = Arc::new(Signal::default());
        let config = StudioConfig {
            socket_path: Some(side.socket.to_string_lossy().into_owned()),
            session_dir: None,
            data_dir: Some(side.data.to_string_lossy().into_owned()),
            launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
            controller_main: None,
            take_over_serve: false,
        };
        let studio =
            Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Self { studio, signal, view: View::default(), label })
    }

    fn until(&mut self, step: &str, bound: Duration, mut check: impl FnMut(&View) -> bool) -> anyhow::Result<()> {
        let started = Instant::now();
        loop {
            for update in self.studio.tick() {
                self.view.apply(update);
            }
            if check(&self.view) {
                println!("ok   {}: {step} ({} ms)", self.label, started.elapsed().as_millis());
                return Ok(());
            }
            let Some(left) = bound.checked_sub(started.elapsed()) else {
                bail!("{}: timed out after {} s: {step}", self.label, bound.as_secs());
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

    fn close(self) -> Option<u32> {
        self.studio.shutdown();
        self.view.daemon
    }
}

struct Side {
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    socket: PathBuf,
}

impl Side {
    fn new(root: &Path, name: &str) -> anyhow::Result<Self> {
        let root = root.join(name);
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(root.join("agent"))?;
        Ok(Self { data: root.join("data"), socket: root.join("d.sock"), home, root })
    }

    fn agent(&self) -> PathBuf {
        self.root.join("agent")
    }

    fn enter(&self) {
        std::env::set_var("HOME", &self.home);
        std::env::set_var("EUKHE_DAEMON_SOCKET", &self.socket);
        std::env::set_var("EUKHE_CODING_AGENT_DIR", self.agent());
        std::env::set_var("LOCAL_STUDIO_DATA_DIR", &self.data);
    }

    fn isolate(&self, command: &mut Command) {
        command
            .env("HOME", &self.home)
            .env("EUKHE_DAEMON_SOCKET", &self.socket)
            .env("EUKHE_CODING_AGENT_DIR", self.agent())
            .env("LOCAL_STUDIO_DATA_DIR", &self.data);
    }
}

struct Serve {
    bin: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl Serve {
    fn start(&mut self, side: &Side, eukhe: &Path) -> anyhow::Result<()> {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(side.root.join("serve.log"))?;
        let mut command = Command::new(&self.bin);
        side.isolate(&mut command);
        let child = command
            .args(["serve", "--port", &self.port.to_string(), "--name", NAME, "--eukhe"])
            .arg(eukhe)
            .arg("--data-dir")
            .arg(&side.data)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .context("start local-studio-serve")?;
        println!("     serve pid {} on 127.0.0.1:{}", child.id(), self.port);
        self.child = Some(child);
        let deadline = Instant::now() + QUICK;
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            ensure!(Instant::now() < deadline, "serve did not listen on {} within 30 s", self.port);
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn run(&self, side: &Side, args: &[&str]) -> anyhow::Result<String> {
        let mut command = Command::new(&self.bin);
        side.isolate(&mut command);
        let output = command.args(args).arg("--data-dir").arg(&side.data).output()?;
        ensure!(output.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        let Some(mut child) = self.child.take() else { return Ok(()) };
        let pid = child.id().to_string();
        Command::new("kill").args(["-TERM", &pid]).status()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                Command::new("kill").args(["-KILL", &pid]).status()?;
                child.wait()?;
                bail!("serve {pid} ignored SIGTERM for 15 s and was killed");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("ok   serve {pid} stopped by PID");
        Ok(())
    }
}

fn write_file(path: &Path, text: &str, mode: u32) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn scratch_agent(agent: &Path) -> anyhow::Result<()> {
    write_file(&agent.join("auth.json"), "{}", 0o600)?;
    write_file(&agent.join("settings.json"), r#"{"onboardingShown":true}"#, 0o600)?;
    local_studio_core::check_scratch_agent(agent).map_err(anyhow::Error::msg)
}

fn fixture(plugins_dir: &Path, log: &Path) -> anyhow::Result<()> {
    let dir = plugins_dir.join(PLUGIN);
    std::fs::create_dir_all(&dir)?;
    let manifest = format!(
        "id = \"{PLUGIN}\"\nversion = \"1.0.0\"\ndescription = \"caller smoke fixture\"\ncapabilities = [\"mcp-server\"]\n\n[mcp]\ncommand = \"server.py\"\nargs = [{:?}]\n",
        log.to_string_lossy()
    );
    write_file(&dir.join("plugin.toml"), &manifest, 0o644)?;
    write_file(&dir.join("server.py"), SERVER, 0o755)?;
    let plugin = plugins::load(&dir).map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
    let mut grants = Grants::load(plugins_dir).map_err(anyhow::Error::msg)?;
    grants.grant(&plugin, &[Capability::McpServer], None).map_err(anyhow::Error::msg)?;
    grants.save(plugins_dir).map_err(anyhow::Error::msg)?;
    println!("ok   {PLUGIN} installed in {} with mcp-server granted", plugins_dir.display());
    Ok(())
}

fn calls(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn claimed(log: &Path, forged: &str) -> Option<Value> {
    calls(log).into_iter().find(|call| call["claimed"] == forged)
}

fn call_tool(desk: &mut Desk, thread: &str, log: &Path, forged: &str, label: &str) -> anyhow::Result<Value> {
    for attempt in 1..=TRIES {
        desk.until(&format!("{label} turn {attempt} settles"), TURN, |view| view.idle(thread))?;
        if let Some(call) = claimed(log, forged) {
            return Ok(call);
        }
        if attempt == TRIES {
            break;
        }
        println!("     the model did not call whoami yet; asking again");
        desk.studio.send(format!("{label}-{attempt}"), thread.to_string(), draft(&ask(forged)), Delivery::Queue);
        desk.until("the retry starts", TURN, |view| !view.idle(thread) || claimed(log, forged).is_some())?;
    }
    bail!("{label}: the model never called whoami claiming {forged} in {TRIES} turns")
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn stop_daemon(socket: &Path, known: Option<u32>) -> anyhow::Result<()> {
    let mut pid = known;
    if let Ok((client, _events)) = DaemonClient::connect(socket).await {
        pid =
            client.hello().get("supervisorPid").and_then(Value::as_u64).and_then(|pid| u32::try_from(pid).ok()).or(pid);
        let command = DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() };
        client.request_ok(command).await.ok();
        client.close();
    }
    let Some(pid) = pid else {
        println!("ok   no daemon was running on {}", socket.display());
        return Ok(());
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        ensure!(Instant::now() < deadline, "daemon {pid} is still running after Shutdown");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("ok   daemon {pid} stopped");
    Ok(())
}

struct Run {
    eukhe: PathBuf,
    a: Side,
    b: Side,
    work: PathBuf,
    log: PathBuf,
    uid: u32,
    serve: Serve,
    daemons: Vec<(PathBuf, Option<u32>)>,
}

fn host_turn(run: &mut Run) -> anyhow::Result<Value> {
    let mut desk = Desk::open("host A", &run.eukhe, &run.a)?;
    let outcome = (|| {
        desk.until("the host core is ready", SETUP, |view| view.ready)?;
        let path = run.work.to_string_lossy().into_owned();
        desk.studio.add_project(path.clone());
        desk.until("adds the work folder", SETUP, |view| view.projects.iter().any(|project| project.path == path))?;
        let project = desk.view.projects.iter().find(|project| project.path == path).context("no project")?.id.clone();
        let settings = real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local);
        desk.studio.start_thread("host-1".to_string(), Some(project), draft(&ask(FORGED_DEVICE)), settings);
        desk.until("the host thread starts", TURN, |view| view.started.contains_key("host-1"))?;
        let thread = desk.view.started["host-1"].clone();
        call_tool(&mut desk, &thread, &run.log, FORGED_DEVICE, "host")
    })();
    run.daemons.push((run.a.socket.clone(), desk.close()));
    outcome
}

fn device_turn(run: &mut Run, forged: &str) -> anyhow::Result<(Value, String)> {
    run.serve.start(&run.a, &run.eukhe)?;
    let url = format!("ws://127.0.0.1:{}/v1/ws", run.serve.port);
    let printed = run.serve.run(&run.a, &["pair", "--url", &url, "--name", NAME])?;
    let own = format!("{}://pair?", pairing::scheme(CHANNEL));
    let link = printed.lines().find(|line| line.starts_with(&own)).context("pair printed no link")?.to_string();
    println!("ok   local-studio-serve pair printed a {} link", pairing::scheme(CHANNEL));
    let mut desk = Desk::open("device B", &run.eukhe, &run.b)?;
    let outcome = (|| {
        desk.studio.pair_machine(link);
        desk.until("B pairs with A's serve", QUICK, |view| {
            view.machines.iter().any(|machine| machine.name == NAME && machine.state == MachineState::Connected)
        })?;
        let path = run.work.to_string_lossy().into_owned();
        desk.until("B lists A's project", QUICK, |view| {
            view.projects.iter().any(|project| project.path == path && project.machine.is_some())
        })?;
        let project = desk
            .view
            .projects
            .iter()
            .find(|project| project.path == path && project.machine.is_some())
            .context("no remote project")?
            .id
            .clone();
        let settings = real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local);
        desk.studio.start_thread("device-1".to_string(), Some(project), draft(&ask(forged)), settings);
        desk.until("B's thread on A starts", TURN, |view| view.started.contains_key("device-1"))?;
        let thread = desk.view.started["device-1"].clone();
        ensure!(thread.starts_with('@'), "the device thread {thread} is not a remote id");
        desk.studio.select_thread(Some(thread.clone()));
        call_tool(&mut desk, &thread, &run.log, forged, "device")
    })();
    run.daemons.push((run.b.socket.clone(), desk.close()));
    let call = outcome?;
    let paired = Devices::new(&run.a.data).list()?;
    ensure!(paired.len() == 1, "A lists {} paired devices, not 1", paired.len());
    run.serve.stop()?;
    Ok((call, paired[0].info.id.clone()))
}

fn usage(run: &mut Run) -> anyhow::Result<()> {
    let mut desk = Desk::open("host A", &run.eukhe, &run.a)?;
    let outcome = (|| {
        desk.until("the host core is ready again", SETUP, |view| view.ready)?;
        desk.studio.load_usage("usage-1".to_string(), UsagePeriod::Day);
        desk.until("the usage report loads", SETUP, |view| {
            view.usage.is_some() || view.failed.contains_key("usage-1")
        })?;
        real_model::check_usage(desk.view.usage.as_ref().context("the usage report failed")?)
    })();
    run.daemons.push((run.a.socket.clone(), desk.close()));
    outcome
}

fn drive(run: &mut Run) -> anyhow::Result<()> {
    let host = format!("local:{}", run.uid);
    let host_call = host_turn(run)?;
    println!("     host call logged {host_call}");
    let (device_call, device) = device_turn(run, &host)?;
    println!("     device call logged {device_call}");
    let expected = format!("device:{device}");
    ensure!(
        host_call["caller"] == host.as_str(),
        "the host thread's server saw caller {} instead of {host}",
        host_call["caller"]
    );
    ensure!(host_call["name"].as_str().is_some_and(|name| !name.is_empty()), "the host caller has no name");
    println!("ok   the host session's call arrived as {host} although its arguments claimed {FORGED_DEVICE}");
    ensure!(
        device_call["caller"] == expected.as_str(),
        "the paired device's server saw caller {} instead of {expected}",
        device_call["caller"]
    );
    ensure!(device_call["name"].as_str().is_some_and(|name| !name.is_empty()), "the device caller has no name");
    println!("ok   the paired device's call arrived as {expected} although its arguments claimed {host}");
    ensure!(host_call["caller"] != device_call["caller"], "both sessions logged the same caller");
    ensure!(host_call["pid"] != device_call["pid"], "both sessions shared one plugin server process");
    println!("ok   two sessions logged two different caller ids from two server processes");
    usage(run)
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: caller_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let eukhe = std::fs::canonicalize(PathBuf::from(args.next().context(usage)?))?;
    let root = PathBuf::from(args.next().context(usage)?);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?.join(format!("r{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let bin = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("local-studio-serve"))
        .filter(|bin| bin.is_file())
        .context("build local-studio-serve first: cargo build -p local-studio-remote --bins --examples")?;
    let a = Side::new(&root, "a")?;
    let b = Side::new(&root, "b")?;
    real_model::install(&a.agent())?;
    scratch_agent(&b.agent())?;
    let log = root.join("callers.log");
    fixture(&a.data.join("plugins"), &log)?;
    let work = a.root.join("work");
    std::fs::create_dir_all(&work)?;
    let status = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
    ensure!(status.success(), "git init failed");
    let uid = std::fs::metadata(&root)?.uid();
    let serve = Serve { bin, port: std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port(), child: None };
    let mut run = Run { eukhe, a, b, work, log, uid, serve, daemons: Vec::new() };
    let outcome = drive(&mut run);
    if outcome.is_err() {
        let serve_log = std::fs::read_to_string(run.a.root.join("serve.log")).unwrap_or_default();
        let lines: Vec<&str> = serve_log.lines().collect();
        println!("     serve log tail:\n{}", lines[lines.len().saturating_sub(25)..].join("\n"));
        println!("     plugin calls: {:?}", calls(&run.log));
    }
    let stopped = run.serve.stop();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let mut daemons: Vec<(PathBuf, Option<u32>)> = Vec::new();
    for (socket, pid) in run.daemons.drain(..) {
        match daemons.iter_mut().find(|(known, _)| *known == socket) {
            Some(slot) => slot.1 = slot.1.or(pid),
            None => daemons.push((socket, pid)),
        }
    }
    let mut daemon_errors = Vec::new();
    for (socket, pid) in &daemons {
        if let Err(error) = runtime.block_on(stop_daemon(socket, *pid)) {
            daemon_errors.push(error);
        }
    }
    outcome?;
    stopped?;
    if let Some(error) = daemon_errors.into_iter().next() {
        return Err(error);
    }
    println!("PASS caller smoke ({})", root.display());
    Ok(())
}
