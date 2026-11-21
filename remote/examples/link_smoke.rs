use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::devices::{unhex, Devices};
use local_studio_core::link::{noise, pairing, snow};
use local_studio_core::{
    AccessMode, Approval, ApprovalAnswer, Block, DaemonLaunch, Delivery, Harness, Item, ItemKind, Machine,
    MachineState, ModelOption, Project, PromptDraft, Studio, StudioConfig, ThreadRow, ThreadSettings, ThreadStatus,
    TranscriptOp, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::Message;

const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const QUICK: Duration = Duration::from_secs(30);
const SETUP: Duration = Duration::from_secs(120);
const MODEL_STEP: Duration = Duration::from_secs(300);
const RECONNECT: Duration = Duration::from_secs(90);
const SILENT: Duration = Duration::from_secs(50);
const MARGIN: Duration = Duration::from_secs(15);
const MARKER: &str = "the fake far side says hello";
const FAKE_SOCKET: &str = "/fake-far-side/d.sock";
const NAME: &str = "smoke-a";
const PONG: &str = "Reply with only the word pong.";
const PING: &str = "Reply with only the word ping.";
const QUEUED: &str = "Reply with only the word queued.";

const PROVIDER: &str = "claude-code";
const STORY: &str = "Write a 600 word short story about a lighthouse keeper on a northern island, in plain prose.";
const PROOF: &str = "link-proof.txt";
const WRITE: &str = "Create a file named link-proof.txt in the current directory containing the single line hello. Use a file writing tool, not a shell command, and do not ask me first.";

fn draft(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
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
    models: Vec<ModelOption>,
    threads: Vec<ThreadRow>,
    projects: Vec<Project>,
    started: HashMap<String, String>,
    items: HashMap<String, Vec<Item>>,
    resets: HashMap<String, usize>,
    approvals: HashMap<String, Vec<Approval>>,
    notices: Vec<String>,
    failed: HashMap<String, String>,
    pairing: Option<(String, String, i64)>,
    usage: Option<UsageReport>,
    daemon: Option<u32>,
    sockets: Vec<String>,
    settings: usize,
    ready: bool,
}

impl View {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Machines { machines } => self.machines = machines,
            Update::Models { models, .. } => self.models = models,
            Update::Threads { threads } => self.threads = threads,
            Update::Projects { projects } => self.projects = projects,
            Update::ThreadStarted { draft, thread } => {
                self.started.insert(draft, thread);
            }
            Update::Transcript { thread, op } => {
                let items = self.items.entry(thread.clone()).or_default();
                match op {
                    TranscriptOp::Reset { items: all } => {
                        *items = all;
                        *self.resets.entry(thread).or_default() += 1;
                    }
                    TranscriptOp::Insert { index, item } => {
                        let index = usize::try_from(index).unwrap_or(usize::MAX).min(items.len());
                        items.insert(index, item);
                    }
                    TranscriptOp::Update { index, item } => {
                        if let Some(slot) = usize::try_from(index).ok().and_then(|index| items.get_mut(index)) {
                            *slot = item;
                        }
                    }
                }
            }
            Update::Approvals { thread, approvals } => {
                self.approvals.insert(thread, approvals);
            }
            Update::Notice { level, text } => {
                println!("     notice {level:?}: {text}");
                self.notices.push(text);
            }
            Update::RequestFailed { request, message } => {
                println!("     request {request} failed: {message}");
                self.failed.insert(request, message);
            }
            Update::Pairing { request, link, expires_at, .. } => self.pairing = Some((request, link, expires_at)),
            Update::Usage { report, .. } => self.usage = Some(report),
            Update::Daemon { socket, pid, .. } => {
                self.sockets.push(socket);
                if pid.is_some() {
                    self.daemon = pid;
                }
            }
            Update::Settings { .. } => self.settings += 1,
            Update::Connection { state: local_studio_core::Connection::Ready } => self.ready = true,
            _ => {}
        }
    }

    fn machine(&self, id: &str) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.id == id)
    }

    fn row(&self, thread: &str) -> Option<&ThreadRow> {
        self.threads.iter().find(|row| row.id == thread)
    }

    fn idle(&self, thread: &str) -> bool {
        self.row(thread).is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }

    fn items(&self, thread: &str) -> &[Item] {
        self.items.get(thread).map_or(&[], Vec::as_slice)
    }

    fn replies(&self, thread: &str) -> usize {
        self.items(thread).iter().filter(|item| reply(item).is_some()).count()
    }
}

fn reply(item: &Item) -> Option<String> {
    let ItemKind::Assistant { blocks, streaming: false, error: None, .. } = &item.kind else { return None };
    let text: String = blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(text.as_str()),
            Block::Thinking { .. } => None,
        })
        .collect();
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn user_texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

struct Desk {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    label: &'static str,
}

impl Desk {
    fn open(label: &'static str, eukhe: &Path, data: &Path, socket: &Path) -> anyhow::Result<Self> {
        let signal = Arc::new(Signal::default());
        let config = StudioConfig {
            socket_path: Some(socket.to_string_lossy().into_owned()),
            session_dir: None,
            data_dir: Some(data.to_string_lossy().into_owned()),
            launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
            controller_main: None,
        };
        let studio =
            Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Self { studio, signal, view: View::default(), label })
    }

    fn pump(&mut self) {
        for update in self.studio.tick() {
            self.view.apply(update);
        }
    }

    fn until(&mut self, step: &str, bound: Duration, mut check: impl FnMut(&View) -> bool) -> anyhow::Result<Duration> {
        let started = Instant::now();
        loop {
            self.pump();
            if check(&self.view) {
                let elapsed = started.elapsed();
                println!("ok   {}: {step} ({} ms)", self.label, elapsed.as_millis());
                return Ok(elapsed);
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

    fn hold(&mut self, span: Duration) {
        let started = Instant::now();
        while started.elapsed() < span {
            self.pump();
            std::thread::sleep(Duration::from_millis(100));
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
    fn new(root: &Path, name: &str, home: Option<PathBuf>) -> anyhow::Result<Self> {
        let root = root.join(name);
        let home = home.unwrap_or_else(|| root.join("home"));
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

    fn pause(&self, signal: &str) -> anyhow::Result<()> {
        let child = self.child.as_ref().context("serve is not running")?;
        let pid = child.id().to_string();
        let status = Command::new("kill").args([signal, &pid]).status()?;
        ensure!(status.success(), "kill {signal} {pid} failed");
        println!("ok   serve {pid} sent {signal}");
        Ok(())
    }

    fn run(&self, side: &Side, args: &[&str]) -> anyhow::Result<String> {
        let mut command = Command::new(&self.bin);
        side.isolate(&mut command);
        let output = command.args(args).arg("--data-dir").arg(&side.data).output()?;
        ensure!(output.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn signal(&mut self, signal: &str) -> anyhow::Result<()> {
        let Some(mut child) = self.child.take() else { return Ok(()) };
        let pid = child.id().to_string();
        Command::new("kill").args([signal, &pid]).status()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                Command::new("kill").args(["-KILL", &pid]).status()?;
                child.wait()?;
                bail!("serve {pid} ignored {signal} for 15 s and was killed");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("ok   serve {pid} stopped by PID ({signal})");
        Ok(())
    }
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn git(work: &Path, home: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(work)
        .env("HOME", home)
        .stdout(Stdio::null())
        .status()?;
    ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

fn scratch_agent(agent: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(agent)?;
    for (name, data) in [("auth.json", "{}"), ("settings.json", r#"{"onboardingShown":true}"#)] {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(agent.join(name))
            .with_context(|| format!("create {}", agent.join(name).display()))?
            .write_all(data.as_bytes())?;
    }
    local_studio_core::check_scratch_agent(agent).map_err(|error| anyhow::anyhow!(error))
}

fn mode(path: &Path) -> anyhow::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
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
    serve: Serve,
    model: String,
    daemons: Vec<(PathBuf, Option<u32>)>,
    report: Vec<String>,
}

fn add_project(run: &mut Run) -> anyhow::Result<()> {
    run.a.enter();
    let mut desk = Desk::open("desktop A", &run.eukhe, &run.a.data, &run.a.socket)?;
    desk.studio.add_project(run.work.to_string_lossy().into_owned());
    let path = run.work.to_string_lossy().into_owned();
    desk.until("adds the work folder as a project", SETUP, |view| {
        view.projects.iter().any(|project| project.path == path && project.machine.is_none())
    })?;
    desk.until("lists the Claude Code models", SETUP, |view| {
        view.models.iter().any(|model| model.provider == PROVIDER)
    })?;
    let model = desk
        .view
        .models
        .iter()
        .find(|model| model.provider == PROVIDER && model.id == "haiku")
        .map(|model| model.id.clone());
    run.daemons.push((run.a.socket.clone(), desk.close()));
    run.model = model.context("Claude Code lists no haiku model")?;
    println!("     A runs {PROVIDER}/{} with the claude CLI's own sign-in", run.model);
    Ok(())
}

fn pairing_link(run: &Run) -> anyhow::Result<String> {
    let url = format!("ws://127.0.0.1:{}/v1/ws", run.serve.port);
    let printed = run.serve.run(&run.a, &["pair", "--url", &url, "--name", NAME])?;
    let own = format!("{}://pair?", pairing::scheme(CHANNEL));
    let link = printed.lines().find(|line| line.starts_with(&own)).context("pair printed no link")?;
    println!("ok   local-studio-serve pair printed a {} link", pairing::scheme(CHANNEL));
    Ok(link.to_string())
}

fn pair_checks(run: &mut Run, b: &mut Desk) -> anyhow::Result<String> {
    let foreign = format!(
        "{}x://pair?url=ws%3A%2F%2F127.0.0.1%3A1%2Fv1%2Fws&name=x&id=0123456789abcdef#AAAA",
        pairing::scheme(CHANNEL)
    );
    let before = b.view.notices.len();
    b.studio.pair_machine(foreign);
    b.until("a link for another channel is refused with a notice", QUICK, |view| {
        view.notices[before..].iter().any(|notice| notice.contains("pairs only"))
    })?;
    b.studio.open_pairing("pair-b".to_string());
    b.until("open_pairing answers with a link", QUICK, |view| view.pairing.is_some())?;
    let (request, link, expires_at) = b.view.pairing.clone().context("no pairing")?;
    ensure!(request == "pair-b", "open_pairing answered request {request}");
    let parsed = pairing::parse(&link, local_studio_core::CHANNEL).map_err(|error| anyhow::anyhow!(error))?;
    let minted: Value = serde_json::from_slice(&std::fs::read(run.b.data.join("remote/pairing.json"))?)?;
    ensure!(minted["id"] == parsed.id.as_str(), "the pairing link's id is not the one in pairing.json");
    ensure!(
        expires_at > pairing_now() && expires_at <= pairing_now() + 10 * 60 * 1000 + 5_000,
        "expires_at {expires_at}"
    );
    println!("ok   open_pairing minted {} for {} (same pairing.json as serve pair)", parsed.id, parsed.url);

    let link = pairing_link(run)?;
    let started = Instant::now();
    b.studio.pair_machine(link.clone());
    b.until("pair_machine connects the machine", QUICK, |view| {
        view.machines.iter().any(|machine| machine.name == NAME && machine.state == MachineState::Connected)
    })?;
    let paired = started.elapsed();
    let machine = b.view.machines.iter().find(|machine| machine.name == NAME).cloned().context("no machine")?;
    ensure!(machine.os == std::env::consts::OS, "machine os {}", machine.os);
    ensure!(machine.channel == CHANNEL, "machine channel {}", machine.channel);
    ensure!(!machine.version.is_empty(), "the machine reports no version");
    ensure!(machine.url == format!("ws://127.0.0.1:{}/v1/ws", run.serve.port), "machine url {}", machine.url);
    run.report.push(format!("pair_machine to connected (XXpsk3 + hello): {} ms", paired.as_millis()));
    let links = run.b.data.join("remote/links.json");
    ensure!(mode(&links)? == 0o600, "links.json mode {:o}", mode(&links)?);
    ensure!(
        mode(&run.b.data.join("remote"))? == 0o700,
        "the remote folder mode {:o}",
        mode(&run.b.data.join("remote"))?
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&links)?)?;
    ensure!(saved["machines"][0]["id"] == machine.id.as_str(), "links.json does not hold the machine");
    println!("ok   links.json holds the credential (0600 in a 0700 folder)");
    let path = run.work.to_string_lossy().into_owned();
    let id = machine.id.clone();
    b.until("Projects carries A's project with machine set", QUICK, |view| {
        view.projects.iter().any(|project| {
            project.path == path
                && project.machine.as_deref() == Some(&id)
                && project.id.starts_with(&format!("@{id}/"))
        })
    })?;
    Ok(id)
}

fn pairing_now() -> i64 {
    i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis())
        .unwrap_or_default()
}

fn model_checks(run: &mut Run, b: &mut Desk, machine: &str) -> anyhow::Result<String> {
    let path = run.work.to_string_lossy().into_owned();
    let project = b
        .view
        .projects
        .iter()
        .find(|project| project.path == path && project.machine.is_some())
        .context("no remote project")?
        .id
        .clone();
    let settings = ThreadSettings {
        provider: Some(PROVIDER.to_string()),
        model: Some(run.model.clone()),
        thinking_level: None,
        access: AccessMode::Supervised,
        worktree: WorktreeChoice::Local,
    };
    let started = Instant::now();
    b.studio.start_thread("d-1".to_string(), Some(project), draft(PONG), settings);
    b.until("start_thread on A's project answers thread_started", MODEL_STEP, |view| view.started.contains_key("d-1"))?;
    let thread = b.view.started["d-1"].clone();
    ensure!(thread.starts_with(&format!("@{machine}/")), "the started thread id {thread} is not a remote id");
    b.studio.select_thread(Some(thread.clone()));
    b.until("the first reply streams back and the thread settles", MODEL_STEP, |view| {
        view.replies(&thread) >= 1 && view.idle(&thread)
    })?;
    let row = b.view.row(&thread).context("no row for the remote thread")?;
    ensure!(row.machine.as_deref() == Some(machine), "the thread row has machine {:?}", row.machine);
    ensure!(row.harness == Harness::ClaudeCode, "the remote row shows harness {:?}", row.harness);
    ensure!(
        b.view
            .threads
            .iter()
            .filter(|row| row.machine.is_some())
            .all(|row| row.id.starts_with(&format!("@{machine}/"))),
        "a remote row has a local id"
    );
    run.report.push(format!("start_thread to first reply done: {} ms", started.elapsed().as_millis()));

    let before = b.view.items(&thread).len();
    let sent = Instant::now();
    b.studio.send("r-2".to_string(), thread.clone(), draft(PING), Delivery::Default);
    let first =
        b.until("send's first transcript update arrives", MODEL_STEP, |view| view.items(&thread).len() > before)?;
    run.report.push(format!("send to first transcript update: {} ms", first.as_millis()));
    b.until("the second reply finishes", MODEL_STEP, |view| view.replies(&thread) >= 2 && view.idle(&thread))?;
    println!("     reply lasted {} ms after send", sent.elapsed().as_millis());

    let replies = b.view.replies(&thread);
    b.studio.send("r-3".to_string(), thread.clone(), draft(STORY), Delivery::Default);
    b.until("the slow turn is running", MODEL_STEP, |view| {
        view.row(&thread).is_some_and(|row| row.status == ThreadStatus::Working)
    })?;
    b.studio.send("r-4".to_string(), thread.clone(), draft(QUEUED), Delivery::Queue);
    println!("     Claude Code keeps follow-ups in the CLI, so the core sends no Queue rows for this thread");
    b.until("the follow-up sent mid-turn runs after the slow turn", MODEL_STEP, |view| {
        view.idle(&thread) && view.replies(&thread) >= replies + 2 && !view.failed.contains_key("r-4")
    })?;
    let users = user_texts(b.view.items(&thread));
    let story = users.iter().position(|text| text == STORY).context("the slow turn's prompt is missing")?;
    let queued = users.iter().position(|text| text == QUEUED).context("the follow-up's prompt is missing")?;
    ensure!(story < queued, "the follow-up ran before the slow turn: {users:?}");
    let last = b.view.items(&thread).iter().rev().find_map(reply).unwrap_or_default();
    ensure!(last.to_ascii_lowercase().contains("queued"), "the follow-up's reply is {last:?}");
    println!("ok   the follow-up sent with Delivery::Queue during the slow turn ran after it");

    let proof = run.work.join(PROOF);
    let replies = b.view.replies(&thread);
    b.studio.send("r-5".to_string(), thread.clone(), draft(WRITE), Delivery::Default);
    let mut answered = 0;
    loop {
        b.until("an approval reaches B or the write turn ends", MODEL_STEP, |view| {
            view.approvals.get(&thread).is_some_and(|approvals| !approvals.is_empty())
                || (view.idle(&thread) && view.replies(&thread) > replies)
        })?;
        let Some(approval) = b.view.approvals.get(&thread).and_then(|approvals| approvals.first()).cloned() else {
            break;
        };
        if answered == 0 {
            ensure!(!proof.exists(), "{PROOF} was written before the approval");
        }
        println!("     approval on B: {}", approval.command);
        b.studio.answer_approval(thread.clone(), approval.request_id.clone(), ApprovalAnswer::Approve);
        answered += 1;
        let request = approval.request_id;
        b.until("the approval answered from B settles", MODEL_STEP, |view| {
            view.approvals
                .get(&thread)
                .is_none_or(|approvals| approvals.iter().all(|known| known.request_id != request))
        })?;
    }
    ensure!(answered > 0, "Supervised did not ask before writing {PROOF}");
    let written = std::fs::read_to_string(&proof).with_context(|| format!("{PROOF} is missing after the approval"))?;
    ensure!(written.contains("hello"), "{PROOF} holds {written:?}");
    println!("ok   the approval answered from B let the write run on A");

    let before = b.view.notices.len();
    b.studio.delete_thread(thread.clone());
    b.until("delete_thread on a remote id posts a notice naming the machine", QUICK, |view| {
        view.notices[before..].iter().any(|notice| notice.contains(NAME))
    })?;
    b.hold(Duration::from_secs(1));
    ensure!(b.view.row(&thread).is_some(), "delete_thread removed the remote row");
    Ok(thread)
}

fn connected(view: &View, machine: &str) -> bool {
    view.machine(machine).is_some_and(|machine| machine.state == MachineState::Connected)
}

fn offline(view: &View, machine: &str) -> bool {
    view.machine(machine).is_some_and(|machine| matches!(machine.state, MachineState::Offline { .. }))
}

fn freeze_checks(run: &mut Run, b: &mut Desk, machine: &str) -> anyhow::Result<()> {
    let project = b
        .view
        .projects
        .iter()
        .find(|project| project.machine.as_deref() == Some(machine))
        .context("no remote project")?
        .id
        .clone();
    b.until("the machine is connected before the freeze", QUICK, |view| connected(view, machine))?;
    run.serve.pause("-STOP")?;
    let frozen = (|| {
        b.studio.list_commands("frozen-1".to_string(), Some(project));
        let left = b.until("a frozen machine leaves Connected within the silence limit", SILENT + MARGIN, |view| {
            !connected(view, machine)
        })?;
        b.until("the request sent during the freeze fails as disconnected", QUICK, |view| {
            view.failed.get("frozen-1").is_some_and(|message| message.contains("disconnected"))
        })?;
        anyhow::Ok(left)
    })();
    run.serve.pause("-CONT")?;
    let left = frozen?;
    run.report.push(format!("SIGSTOP of serve to leaving Connected: {} ms", left.as_millis()));
    b.until("B reconnects after the freeze ends", RECONNECT, |view| connected(view, machine))?;
    Ok(())
}

fn fake_far_side(run: &Run) -> anyhow::Result<std::thread::JoinHandle<anyhow::Result<()>>> {
    let devices = Devices::new(&run.a.data);
    let key = devices.static_key()?;
    let machine = devices.machine_id()?;
    let known = devices.list()?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", run.serve.port))?;
    listener.set_nonblocking(true)?;
    Ok(std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let (tcp, _) = tokio::time::timeout(RECONNECT, listener.accept()).await.context("B never came")??;
            let mut socket = tokio_tungstenite::accept_async(tcp).await?;
            let Some(Ok(Message::Text(header))) = socket.next().await else { bail!("B sent no KK header") };
            let parsed = noise::parse_header(&header).map_err(|error| anyhow::anyhow!(error))?;
            let device = known
                .iter()
                .find(|device| Some(&device.info.id) == parsed.device.as_ref())
                .context("the KK header names no paired device")?;
            let public = unhex(&device.key).context("the device key is not hex")?;
            let mut state = snow::Builder::new(noise::KK.parse()?)
                .local_private_key(&key.private)?
                .remote_public_key(&public)?
                .prologue(&noise::prologue(&header))?
                .build_responder()?;
            let Some(Ok(Message::Binary(first))) = socket.next().await else { bail!("B sent no KK message") };
            noise::read(&mut state, &first)?;
            socket.send(Message::binary(noise::write(&mut state, &[])?)).await?;
            let channel = noise::Channel::new(state.into_transport_mode()?);
            let Some(Ok(Message::Binary(_))) = socket.next().await else { bail!("B sent no hello") };
            let hello = json!({
                "op": "hello",
                "machine": { "id": machine, "name": NAME, "os": std::env::consts::OS, "channel": CHANNEL, "version": "fake" },
                "snapshot": [
                    { "type": "daemon", "socket": FAKE_SOCKET, "version": "fake", "pid": null, "mismatch": null },
                    { "type": "settings", "settings": {} },
                    { "type": "notice", "level": "info", "text": MARKER },
                ],
            });
            for record in noise::json_records(&hello.to_string()) {
                socket.send(Message::binary(channel.seal(&record)?)).await?;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(Ok(_)) = socket.next().await {}
            })
            .await;
            socket.close(None).await.ok();
            Ok(())
        })
    }))
}

fn far_side_checks(run: &Run, b: &mut Desk, machine: &str) -> anyhow::Result<()> {
    let settings = b.view.settings;
    let fake = fake_far_side(run)?;
    let heard = b.until("B reaches a fake far side and shows its notice", RECONNECT, |view| {
        view.notices.iter().any(|notice| notice.contains(MARKER))
    });
    let joined = fake.join().map_err(|_| anyhow::anyhow!("the fake far side panicked"))?;
    heard?;
    joined?;
    ensure!(!b.view.sockets.iter().any(|socket| socket == FAKE_SOCKET), "a daemon update from the far side reached B");
    ensure!(b.view.settings == settings, "a settings update from the far side reached B");
    println!("ok   the far side's daemon and settings updates stay off B while its notice arrives");
    b.until("the machine shows offline after the fake far side closes", RECONNECT, |view| offline(view, machine))?;
    Ok(())
}

fn reconnect_checks(run: &mut Run, b: &mut Desk, machine: &str, thread: &str) -> anyhow::Result<()> {
    let resets = b.view.resets.get(thread).copied().unwrap_or_default();
    run.serve.signal("-KILL")?;
    b.until("the machine shows offline", RECONNECT, |view| offline(view, machine))?;
    ensure!(b.view.row(thread).is_some(), "an offline machine dropped its rows");
    far_side_checks(run, b, machine)?;
    let started = Instant::now();
    run.serve.start(&run.a, &run.eukhe)?;
    b.until("B reconnects through KK", RECONNECT, |view| {
        view.machine(machine).is_some_and(|machine| machine.state == MachineState::Connected)
    })?;
    run.report.push(format!(
        "serve restart to reconnected (KK + hello, after backoff): {} ms",
        started.elapsed().as_millis()
    ));
    b.until("the watched thread comes back in a snapshot reset", RECONNECT, |view| {
        view.resets.get(thread).copied().unwrap_or_default() > resets && view.replies(thread) >= 4
    })?;
    if let Some(latency) = b.view.machine(machine).and_then(|machine| machine.latency_ms) {
        run.report.push(format!("ping round trip: {latency} ms"));
    }
    Ok(())
}

fn revoke_checks(run: &mut Run, b: &mut Desk, machine: &str) -> anyhow::Result<()> {
    run.serve.run(&run.a, &["revoke", "--all"])?;
    b.until("revoking B on A turns the machine to pair_again", QUICK, |view| {
        view.machine(machine).is_some_and(|machine| machine.state == MachineState::PairAgain)
    })?;
    b.hold(Duration::from_secs(5));
    let state = b.view.machine(machine).map(|machine| machine.state.clone());
    ensure!(state == Some(MachineState::PairAgain), "the machine left pair_again on its own: {state:?}");
    println!("ok   no reconnect after 4401");
    b.studio.forget_machine(machine.to_string());
    b.until("forget_machine removes the machine, its rows and projects", QUICK, |view| {
        view.machines.is_empty()
            && view.threads.iter().all(|row| row.machine.is_none())
            && view.projects.iter().all(|project| project.machine.is_none())
    })?;
    let saved: Value = serde_json::from_slice(&std::fs::read(run.b.data.join("remote/links.json"))?)?;
    ensure!(saved["machines"].as_array().is_some_and(Vec::is_empty), "links.json still holds the machine");
    println!("ok   links.json is empty after forget_machine");
    Ok(())
}

fn a_agrees(run: &mut Run, thread: &str, seen: &[Item]) -> anyhow::Result<()> {
    let (_, local) = thread.strip_prefix('@').and_then(|id| id.split_once('/')).context("not a remote id")?;
    run.a.enter();
    let mut desk = Desk::open("desktop A", &run.eukhe, &run.a.data, &run.a.socket)?;
    desk.until("A's core is ready", SETUP, |view| view.ready)?;
    desk.studio.select_thread(Some(local.to_string()));
    let expected = user_texts(seen);
    desk.until("A's own transcript loads", SETUP, |view| user_texts(view.items(local)) == expected)?;
    let mine: Vec<String> = desk.view.items(local).iter().filter_map(reply).collect();
    let theirs: Vec<String> = seen.iter().filter_map(reply).collect();
    ensure!(mine == theirs, "A's replies {mine:?} differ from what B saw {theirs:?}");
    ensure!(
        desk.view.items(local).len() == seen.len(),
        "A has {} items, B saw {}",
        desk.view.items(local).len(),
        seen.len()
    );
    println!("ok   A's own transcript agrees with B's ({} items, {} replies)", seen.len(), theirs.len());
    desk.studio.load_usage("usage-1".to_string(), UsagePeriod::Day);
    desk.until("the usage report loads", SETUP, |view| view.usage.is_some() || view.failed.contains_key("usage-1"))?;
    let report = desk.view.usage.clone().context("the usage report failed")?;
    let rows: Vec<_> = report.by_model.iter().filter(|row| row.provider == PROVIDER).collect();
    for row in &rows {
        println!(
            "     usage {}/{}: {} tokens, {} requests, ${:.4}",
            row.provider, row.model, row.tokens, row.requests, row.cost_usd
        );
    }
    ensure!(rows.iter().any(|row| row.tokens > 0), "the usage report has no {PROVIDER} tokens");
    ensure!(report.output_tokens > 0, "the usage report counts no output tokens");
    println!("ok   usage report counts real tokens");
    let daemon = desk.close();
    if let Some(slot) = run.daemons.iter_mut().find(|(socket, _)| *socket == run.a.socket) {
        slot.1 = slot.1.or(daemon);
    }
    Ok(())
}

fn drive(run: &mut Run) -> anyhow::Result<()> {
    add_project(run)?;
    run.serve.start(&run.a, &run.eukhe)?;
    run.b.enter();
    let mut b = Desk::open("desktop B", &run.eukhe, &run.b.data, &run.b.socket)?;
    let driven = (|| {
        let machine = pair_checks(run, &mut b)?;
        let thread = model_checks(run, &mut b, &machine)?;
        freeze_checks(run, &mut b, &machine)?;
        reconnect_checks(run, &mut b, &machine, &thread)?;
        let seen = b.view.items(&thread).to_vec();
        revoke_checks(run, &mut b, &machine)?;
        anyhow::Ok((thread, seen))
    })();
    let daemon = b.close();
    run.daemons.push((run.b.socket.clone(), daemon));
    let (thread, seen) = driven?;
    run.serve.signal("-TERM")?;
    a_agrees(run, &thread, &seen)
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: link_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let eukhe = std::fs::canonicalize(PathBuf::from(args.next().context(usage)?))?;
    let root = PathBuf::from(args.next().context(usage)?);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let bin = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("local-studio-serve"))
        .filter(|bin| bin.is_file())
        .context("build local-studio-serve first: cargo build -p local-studio-remote --bins --examples")?;
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is unset")?;
    let a = Side::new(&root, "a", Some(home))?;
    let b = Side::new(&root, "b", None)?;
    scratch_agent(&b.agent())?;
    let work = a.root.join("work");
    std::fs::create_dir_all(&work)?;
    std::fs::write(work.join("notes.txt"), "link smoke notes\n")?;
    git(&work, &b.home, &["init", "-q", "-b", "main"])?;
    git(&work, &b.home, &["add", "."])?;
    git(&work, &b.home, &["commit", "-q", "-m", "seed"])?;
    let serve = Serve { bin, port: free_port()?, child: None };
    let mut run = Run { eukhe, a, b, work, serve, model: String::new(), daemons: Vec::new(), report: Vec::new() };
    let outcome = drive(&mut run);
    if outcome.is_err() {
        let log = std::fs::read_to_string(run.a.root.join("serve.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        println!("     serve log tail:\n{}", lines[lines.len().saturating_sub(25)..].join("\n"));
    }
    let stopped = run.serve.signal("-TERM");
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
    for line in &run.report {
        println!("     {line}");
    }
    outcome?;
    stopped?;
    if let Some(error) = daemon_errors.into_iter().next() {
        return Err(error);
    }
    println!("PASS link smoke");
    Ok(())
}
