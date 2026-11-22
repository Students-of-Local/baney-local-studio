use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::devices::{unhex, Devices};
use local_studio_core::link::{noise, pairing, snow};
use local_studio_core::{
    AccessMode, Approval, ApprovalAnswer, Block, DaemonLaunch, Delivery, Harness, ImageAttachment, Item, ItemKind,
    Machine, MachineState, ModelOption, PairedDevice, Project, PromptDraft, Studio, StudioConfig, ThreadRow,
    ThreadSettings, ThreadStatus, TranscriptOp, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::Message;

#[path = "../../core/examples/support/real_model.rs"]
mod real_model;

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
const B_NAME: &str = "smoke-b";
const LOCAL: &str = "Reply with only the word local.";
const TEST_CA: &str = "LOCAL_STUDIO_LINK_TEST_CA";
const FROZEN_IMAGE: usize = 20 << 20;

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
    serving: Option<bool>,
    devices: Option<Vec<PairedDevice>>,
    drops: usize,
    usage: Option<UsageReport>,
    daemon: Option<u32>,
    sockets: Vec<String>,
    settings: usize,
    ready: bool,
}

impl View {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Machines { machines } => {
                self.drops += machines.iter().filter(|machine| machine.state != MachineState::Connected).count();
                self.machines = machines;
            }
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
            Update::Pairing { request, link, expires_at, serving, .. } => {
                self.pairing = Some((request, link, expires_at));
                self.serving = Some(serving);
            }
            Update::PairedDevices { devices } => self.devices = Some(devices),
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
        Self::start(label, eukhe, data, socket, false)
    }

    fn app(label: &'static str, eukhe: &Path, data: &Path, socket: &Path) -> anyhow::Result<Self> {
        Self::start(label, eukhe, data, socket, true)
    }

    fn start(
        label: &'static str,
        eukhe: &Path,
        data: &Path,
        socket: &Path,
        take_over_serve: bool,
    ) -> anyhow::Result<Self> {
        let signal = Arc::new(Signal::default());
        let config = StudioConfig {
            socket_path: Some(socket.to_string_lossy().into_owned()),
            session_dir: None,
            data_dir: Some(data.to_string_lossy().into_owned()),
            launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
            controller_main: None,
            take_over_serve,
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
    b_work: PathBuf,
    tls: PathBuf,
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
    pair_link(&run.serve, &run.a, run.serve.port, NAME)
}

fn pair_link(serve: &Serve, side: &Side, port: u16, name: &str) -> anyhow::Result<String> {
    let url = format!("ws://127.0.0.1:{port}/v1/ws");
    let printed = serve.run(side, &["pair", "--url", &url, "--name", name])?;
    let own = format!("{}://pair?", pairing::scheme(CHANNEL));
    let link = printed.lines().find(|line| line.starts_with(&own)).context("pair printed no link")?;
    println!("ok   local-studio-serve pair printed a {} link for {name}", pairing::scheme(CHANNEL));
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
    ensure!(b.view.serving == Some(false), "open_pairing on B, which has no serve.json, reported serving");
    println!("ok   open_pairing on B reports serving == false (no serve.json)");
    b.studio.close_pairing("pair-b".to_string());
    let minted = run.b.data.join("remote/pairing.json");
    b.until("close_pairing removes the pairing it minted", QUICK, |_| !minted.exists())?;

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

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Phone {
    label: &'static str,
    machine: String,
    out: tokio::sync::mpsc::UnboundedSender<String>,
    frames: Arc<Mutex<Vec<Value>>>,
    next: u64,
}

async fn wire(
    socket: Socket,
    channel: noise::Channel,
    mut out: tokio::sync::mpsc::UnboundedReceiver<String>,
    frames: &Mutex<Vec<Value>>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut joiner = noise::Joiner::default();
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(20), Duration::from_secs(20));
    loop {
        let text = tokio::select! {
            text = out.recv() => {
                let Some(text) = text else {
                    sink.close().await.ok();
                    return;
                };
                text
            }
            _ = ping.tick() => json!({ "op": "ping" }).to_string(),
            message = stream.next() => {
                let Some(Ok(message)) = message else { return };
                let Message::Binary(bytes) = message else { continue };
                let Ok(record) = channel.open(&bytes) else { return };
                if let Ok(Some(noise::Inbound::Json(text))) = joiner.feed(record) {
                    if let Ok(frame) = serde_json::from_str(&text) {
                        frames.lock().unwrap_or_else(PoisonError::into_inner).push(frame);
                    }
                }
                continue;
            }
        };
        for record in noise::json_records(&text) {
            let Ok(sealed) = channel.seal(&record) else { return };
            if sink.send(Message::binary(sealed)).await.is_err() {
                return;
            }
        }
    }
}

impl Phone {
    async fn pair(label: &'static str, link: &str, device: &str) -> anyhow::Result<Self> {
        let parsed = pairing::parse(link, CHANNEL).map_err(anyhow::Error::msg)?;
        let (mut socket, _) = tokio_tungstenite::connect_async(parsed.url.as_str())
            .await
            .with_context(|| format!("{label}: connect to {}", parsed.url))?;
        let header = noise::header("XXpsk3", &parsed.id);
        socket.send(Message::text(header.clone())).await?;
        let keys = snow::Builder::new(noise::XX.parse()?).generate_keypair()?;
        let mut state = snow::Builder::new(noise::XX.parse()?)
            .local_private_key(&keys.private)?
            .psk(3, &parsed.secret)?
            .prologue(&noise::prologue(&header))?
            .build_initiator()?;
        socket.send(Message::binary(noise::write(&mut state, &[])?)).await?;
        let second = match tokio::time::timeout(QUICK, socket.next()).await.context("no second XX message")? {
            Some(Ok(Message::Binary(bytes))) => bytes,
            other => bail!("{label}: the machine answered the pairing with {other:?}"),
        };
        noise::read(&mut state, &second)?;
        let record =
            json!({ "id": device, "name": "Smoke Phone", "platform": "ios", "app": "0.4.0", "channel": CHANNEL });
        socket.send(Message::binary(noise::write(&mut state, record.to_string().as_bytes())?)).await?;
        let channel = noise::Channel::new(state.into_transport_mode()?);
        let (out, rx) = tokio::sync::mpsc::unbounded_channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&frames);
        tokio::spawn(async move {
            wire(socket, channel, rx, &kept).await;
            kept.lock().unwrap_or_else(PoisonError::into_inner).push(json!({ "op": "closed" }));
        });
        let mut phone = Self { label, machine: String::new(), out, frames, next: 0 };
        let hello = json!({ "op": "hello", "device": { "name": "Smoke Phone", "app": "0.4.0", "channel": CHANNEL }, "resume": { "watch": [] } });
        phone.out.send(hello.to_string()).ok();
        phone
            .until("pairs through XXpsk3 and gets hello", QUICK, |frames| {
                frames.iter().any(|frame| frame["op"] == "hello" || frame["op"] == "closed")
            })
            .await?;
        let hello = phone.frames().into_iter().find(|frame| frame["op"] == "hello");
        let hello = hello.with_context(|| format!("{label}: the socket closed before hello"))?;
        phone.machine = hello.pointer("/machine/id").and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(phone)
    }

    fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    async fn until(&self, step: &str, bound: Duration, check: impl Fn(&[Value]) -> bool) -> anyhow::Result<()> {
        let started = Instant::now();
        loop {
            if check(&self.frames()) {
                println!("ok   {}: {step} ({} ms)", self.label, started.elapsed().as_millis());
                return Ok(());
            }
            ensure!(started.elapsed() < bound, "{}: timed out after {} s: {step}", self.label, bound.as_secs());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn linked_ops(&mut self, remote: &str, remote_project: &str) -> anyhow::Result<()> {
        let text = json!({ "text": PING, "images": [], "files": [] });
        self.refused("watch", json!({ "thread": remote }), "unknown_thread").await?;
        self.refused(
            "send",
            json!({ "request": "p-send", "thread": remote, "prompt": text, "delivery": "default" }),
            "unknown_thread",
        )
        .await?;
        self.refused("load_earlier", json!({ "thread": remote }), "unknown_thread").await?;
        self.refused("mark_unread", json!({ "thread": remote, "unread": true }), "unknown_thread").await?;
        self.refused(
            "start_thread",
            json!({ "draft": "p-draft", "project": remote_project, "prompt": text, "settings": null }),
            "unknown_project",
        )
        .await?;
        let asks = [
            ("search_threads", json!({ "request": "p-search", "query": "Reply", "limit": 50 }), "thread_search"),
            ("list_archived", json!({ "request": "p-archived" }), "archived_threads"),
        ];
        for (op, args, kind) in asks {
            self.next += 1;
            self.out.send(json!({ "id": self.next, "op": op, "args": args }).to_string()).ok();
            self.until(&format!("{op} answers with {kind}"), QUICK, |frames| {
                wire_updates(frames).iter().any(|update| update["type"] == kind)
            })
            .await?;
        }
        Ok(())
    }

    async fn refused(&mut self, op: &str, args: Value, code: &str) -> anyhow::Result<()> {
        self.next += 1;
        let id = self.next;
        self.out.send(json!({ "id": id, "op": op, "args": args }).to_string()).ok();
        self.until(&format!("{op} is answered"), QUICK, |frames| frames.iter().any(|frame| frame["re"] == id)).await?;
        let reply = self.frames().into_iter().find(|frame| frame["re"] == id).unwrap_or_default();
        let got = reply.pointer("/error/code").and_then(Value::as_str);
        ensure!(got == Some(code), "{}: {op} answered {reply}, not {code}", self.label);
        println!(
            "ok   {}: {op} naming a linked id is refused with {code}: {}",
            self.label,
            reply.pointer("/error/message").unwrap_or(&Value::Null)
        );
        Ok(())
    }
}

fn wire_updates(frames: &[Value]) -> Vec<&Value> {
    frames
        .iter()
        .flat_map(|frame| match frame["op"].as_str() {
            Some("hello") => frame["snapshot"].as_array().map(|all| all.iter().collect()).unwrap_or_default(),
            Some("update") => vec![&frame["update"]],
            _ => Vec::new(),
        })
        .collect()
}

fn latest_ids(frames: &[Value], kind: &str) -> Vec<String> {
    wire_updates(frames)
        .into_iter()
        .rev()
        .find(|update| update["type"] == kind)
        .and_then(|update| update[kind].as_array())
        .map(|rows| rows.iter().filter_map(|row| row["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn threads_updates(frames: &[Value]) -> usize {
    wire_updates(frames).into_iter().filter(|update| update["type"] == "threads").count()
}

fn only_local(phone: &Phone, machine: &str) -> anyhow::Result<usize> {
    let frames = phone.frames();
    let needle = format!("@{machine}/");
    ensure!(
        !frames.iter().any(|frame| frame.to_string().contains(&needle)),
        "{}: a frame carries a {needle} id",
        phone.label
    );
    let updates = wire_updates(&frames);
    for update in &updates {
        let kind = update["type"].as_str().unwrap_or_default();
        ensure!(kind != "machines" && kind != "pairing", "{}: the hub relayed a {kind} update", phone.label);
        for row in update
            .get(kind)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|_| kind == "threads" || kind == "projects")
        {
            let id = row["id"].as_str().unwrap_or_default();
            ensure!(
                row.get("machine").is_none() && !id.starts_with('@'),
                "{}: a {kind} row from a linked machine: {row}",
                phone.label
            );
        }
    }
    Ok(updates.len())
}

fn own_thread(run: &Run, b: &mut Desk) -> anyhow::Result<(String, String)> {
    let path = run.b_work.to_string_lossy().into_owned();
    b.studio.add_project(path.clone());
    b.until("B adds its own project", SETUP, |view| {
        view.projects.iter().any(|project| project.path == path && project.machine.is_none())
    })?;
    let project = b
        .view
        .projects
        .iter()
        .find(|project| project.path == path && project.machine.is_none())
        .context("no local project on B")?
        .id
        .clone();
    let settings = real_model::thread_settings(AccessMode::Supervised, WorktreeChoice::Local);
    b.studio.start_thread("d-local".to_string(), Some(project.clone()), draft(LOCAL), settings);
    b.until("B starts its own thread on the local model", MODEL_STEP, |view| {
        view.started.contains_key("d-local") || view.failed.contains_key("d-local")
    })?;
    let thread = b.view.started.get("d-local").cloned().context("B's own thread did not start")?;
    ensure!(!thread.starts_with('@'), "B's own thread has a linked id {thread}");
    b.studio.watch_thread(thread.clone());
    b.until("B's own thread replies on the local model", MODEL_STEP, |view| {
        view.replies(&thread) >= 1 && view.idle(&thread)
    })?;
    b.studio.unwatch_thread(thread.clone());
    Ok((project, thread))
}

fn phone_checks(run: &mut Run, b: &mut Desk, machine: &str, remote: &str) -> anyhow::Result<()> {
    let (project, own) = own_thread(run, b)?;
    let port = free_port()?;
    let installed =
        run.serve.run(&run.b, &["install", "--no-load", "--json", "--port", &port.to_string(), "--name", B_NAME])?;
    ensure!(installed.contains("\"written\""), "install --no-load answered {installed}");
    let host = local_studio_remote::host::start(Arc::clone(&b.studio), Some(&run.b.data))?
        .context("the in-process host returned no handle")?;
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let checked = phones(run, b, &runtime, port, machine, remote, (&project, &own));
    drop(runtime);
    host.stop();
    println!("ok   B's in-process host stopped");
    checked?;
    let revoked = run.serve.run(&run.a, &["revoke", "phone-on-a"])?;
    println!("ok   A revokes the smoke phone so its device list holds only B again: {}", revoked.trim());
    b.studio.load_usage("usage-b".to_string(), UsagePeriod::Day);
    b.until("B's usage report loads", SETUP, |view| view.usage.is_some() || view.failed.contains_key("usage-b"))?;
    real_model::check_usage(b.view.usage.as_ref().context("B's usage report failed")?)?;
    b.view.usage = None;
    println!("ok   B's usage report counts the local model's real tokens");
    Ok(())
}

fn phones(
    run: &Run,
    b: &mut Desk,
    runtime: &tokio::runtime::Runtime,
    port: u16,
    machine: &str,
    remote: &str,
    (project, own): (&str, &str),
) -> anyhow::Result<()> {
    let deadline = Instant::now() + QUICK;
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        ensure!(Instant::now() < deadline, "B's in-process host did not listen on {port} within 30 s");
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("ok   B's in-process host listens on 127.0.0.1:{port} while B stays linked to A");
    ensure!(b.view.row(remote).is_some(), "B's own sidebar no longer lists A's thread {remote}");
    let link = pair_link(&run.serve, &run.b, port, B_NAME)?;
    let mut on_b = runtime.block_on(Phone::pair("phone on B", &link, "phone-on-b"))?;
    ensure!(on_b.machine != machine, "the phone on B was answered by A's machine id");
    let frames = on_b.frames();
    let threads = latest_ids(&frames, "threads");
    ensure!(threads.iter().any(|id| id == own), "the phone's snapshot from B lacks B's own thread: {threads:?}");
    ensure!(
        latest_ids(&frames, "projects").iter().any(|id| id == project),
        "the phone's snapshot from B lacks B's own project"
    );
    only_local(&on_b, machine)?;
    println!("ok   phone on B: the snapshot holds B's {} thread(s) and none of A's", threads.len());

    let remote_project = b
        .view
        .projects
        .iter()
        .find(|project| project.machine.as_deref() == Some(machine))
        .context("B lists no project from A")?
        .id
        .clone();
    runtime.block_on(on_b.linked_ops(remote, &remote_project))?;

    let seen = threads_updates(&on_b.frames());
    let replies = b.view.replies(remote);
    b.studio.send("r-phone".to_string(), remote.to_string(), draft(PING), Delivery::Default);
    b.until("a turn on A's thread finishes on B while the phone listens", MODEL_STEP, |view| {
        view.replies(remote) > replies && view.idle(remote)
    })?;
    b.studio.mark_unread(remote.to_string(), true);
    b.until("A's thread shows unread on B", QUICK, |view| view.row(remote).is_some_and(|row| row.unread))?;
    runtime.block_on(on_b.until("the hub forwards Threads updates the linked turn caused", QUICK, |frames| {
        threads_updates(frames) > seen
    }))?;
    b.hold(Duration::from_secs(2));
    b.studio.mark_unread(remote.to_string(), false);
    let count = only_local(&on_b, machine)?;
    ensure!(latest_ids(&on_b.frames(), "threads").iter().any(|id| id == own), "B's own thread left the phone's rows");
    println!(
        "ok   phone on B: {count} updates, {} Threads, none with a row, transcript, unread mark or notification from A",
        threads_updates(&on_b.frames())
    );
    ensure!(
        !on_b.frames().iter().any(|frame| frame["update"]["type"] == "request_failed"),
        "a refused op still reached the core"
    );

    let (_, a_thread) = remote.strip_prefix('@').and_then(|id| id.split_once('/')).context("not a linked id")?;
    let link = pairing_link(run)?;
    let on_a = runtime.block_on(Phone::pair("phone on A", &link, "phone-on-a"))?;
    ensure!(on_a.machine == machine, "the phone on A was answered by machine {}", on_a.machine);
    only_local(&on_a, &on_b.machine)?;
    let from_b = latest_ids(&on_b.frames(), "threads");
    let from_a = latest_ids(&on_a.frames(), "threads");
    let merged: Vec<(String, String)> = from_b
        .iter()
        .map(|id| (on_b.machine.clone(), id.clone()))
        .chain(from_a.iter().map(|id| (on_a.machine.clone(), id.clone())))
        .collect();
    ensure!(merged.contains(&(on_b.machine.clone(), own.to_string())), "the merged list lacks B's thread");
    ensure!(merged.contains(&(on_a.machine.clone(), a_thread.to_string())), "the merged list lacks A's thread");
    ensure!(!from_a.iter().any(|id| id == own), "A listed B's thread");
    ensure!(!from_b.iter().any(|id| id == a_thread), "B listed A's thread under A's own id");
    println!(
        "ok   the phone paired to B and to A lists {} threads by machine: {} from B (with {own}), {} from A (with {a_thread})",
        merged.len(),
        from_b.len(),
        from_a.len()
    );
    Ok(())
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
        access: Some(AccessMode::Supervised),
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

fn freeze_checks(run: &mut Run, b: &mut Desk, machine: &str, thread: &str) -> anyhow::Result<()> {
    let project = b
        .view
        .projects
        .iter()
        .find(|project| project.machine.as_deref() == Some(machine))
        .context("no remote project")?
        .id
        .clone();
    b.until("the machine is connected before the freeze", QUICK, |view| connected(view, machine))?;
    files_refused(run, b, thread)?;
    run.serve.pause("-STOP")?;
    let frozen = (|| {
        b.studio.list_commands("frozen-1".to_string(), Some(project));
        let image = ImageAttachment { mime_type: "image/png".to_string(), data: vec![0x5a; FROZEN_IMAGE] };
        let prompt = PromptDraft { text: PING.to_string(), images: vec![image], files: Vec::new() };
        b.studio.send("frozen-image".to_string(), thread.to_string(), prompt, Delivery::Default);
        let left = b.until("a frozen machine leaves Connected within the silence limit", SILENT + MARGIN, |view| {
            !connected(view, machine)
        })?;
        b.until("the request sent during the freeze fails as disconnected", QUICK, |view| {
            view.failed.get("frozen-1").is_some_and(|message| message.contains("disconnected"))
        })?;
        b.until("the 20 MB image send stuck on the frozen socket fails as disconnected", QUICK, |view| {
            view.failed.get("frozen-image").is_some_and(|message| message.contains("disconnected"))
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

fn lock_owner(data: &Path) -> Option<u32> {
    std::fs::read_to_string(data.join("studio.lock")).ok()?.trim().parse().ok()
}

fn device_checks(desk: &mut Desk, device: &str) -> anyhow::Result<()> {
    desk.studio.list_paired_devices();
    desk.until("list_paired_devices answers PairedDevices", QUICK, |view| view.devices.is_some())?;
    let listed = desk.view.devices.clone().unwrap_or_default();
    ensure!(listed.len() == 1 && listed[0].id == device, "A lists {listed:?}, not exactly B's device {device}");
    ensure!(listed[0].platform == std::env::consts::OS, "B's device platform is {}", listed[0].platform);
    println!("ok   A lists exactly B ({}, platform {})", listed[0].name, listed[0].platform);
    desk.view.devices = None;
    let before = desk.view.notices.len();
    desk.studio.revoke_paired_device("no-such-id".to_string());
    desk.until("revoking an unknown device posts a notice and republishes the list", QUICK, |view| {
        view.devices.is_some()
            && view.notices[before..].iter().any(|notice| notice == "That device was already revoked")
    })?;
    let ids = |devices: &[PairedDevice]| devices.iter().map(|device| device.id.clone()).collect::<Vec<_>>();
    let again = desk.view.devices.clone().unwrap_or_default();
    ensure!(ids(&again) == ids(&listed), "the list after a stale revoke changed: {again:?}");
    println!("ok   a stale revoke keeps the same list");
    Ok(())
}

fn devices_checks(run: &mut Run, b: &mut Desk, machine: &str) -> anyhow::Result<()> {
    let settings = json!({ "name": NAME, "port": run.serve.port });
    local_studio_core::link::devices::write_private(
        &run.a.data.join("remote/serve.json"),
        format!("{settings}\n").as_bytes(),
    )?;
    b.until("the machine is connected before desk A opens", RECONNECT, |view| connected(view, machine))?;
    let saved: Value = serde_json::from_slice(&std::fs::read(run.b.data.join("remote/links.json"))?)?;
    let device = saved["machines"][0]["device"].as_str().context("links.json names no device id")?.to_string();
    let serve = run.serve.child.as_ref().map(Child::id).context("serve is not running")?;
    ensure!(lock_owner(&run.a.data) == Some(serve), "serve {serve} does not own A's studio.lock");
    let drops = b.view.drops;
    run.a.enter();
    let started = Instant::now();
    let mut desk = Desk::app("desktop A", &run.eukhe, &run.a.data, &run.a.socket)?;
    run.report.push(format!("desk A took studio.lock over from serve: {} ms", started.elapsed().as_millis()));
    ensure!(lock_owner(&run.a.data) == Some(std::process::id()), "desk A opened without owning studio.lock");
    ensure!(run.serve.child.as_mut().context("no serve")?.try_wait()?.is_none(), "serve exited on the hand-over");
    println!("ok   serve {serve} handed studio.lock to desk A and keeps waiting");
    let checked = (|| {
        let host = local_studio_remote::host::start(Arc::clone(&desk.studio), Some(&run.a.data))?
            .context("serve.json exists, so desk A must serve in process")?;
        let served = (|| {
            b.until("B drops serve and reconnects to desk A's in-process server", RECONNECT, |view| {
                view.drops > drops && connected(view, machine)
            })?;
            device_checks(&mut desk, &device)?;
            desk.studio.open_pairing("pair-a".to_string());
            desk.until("open_pairing on desk A answers", QUICK, |view| view.pairing.is_some())?;
            ensure!(desk.view.serving == Some(true), "open_pairing on A, serving in process, reported not serving");
            println!("ok   open_pairing on desk A reports serving == true (in-process server)");
            open_pairing_phone(run, &mut desk, machine, &device)?;
            desk.view.pairing = None;
            desk.studio.open_pairing("pair-a2".to_string());
            desk.until("a second open_pairing on desk A answers", QUICK, |view| view.pairing.is_some())?;
            let minted = run.a.data.join("remote/pairing.json");
            ensure!(minted.exists(), "open_pairing on desk A wrote no pairing.json");
            desk.studio.close_pairing("pair-a2".to_string());
            desk.until("close_pairing on desk A removes its pairing", QUICK, |_| !minted.exists())?;
            anyhow::Ok(())
        })();
        host.stop();
        served
    })();
    let drops = b.view.drops;
    let daemon = desk.close();
    if let Some(slot) = run.daemons.iter_mut().find(|(socket, _)| *socket == run.a.socket) {
        slot.1 = slot.1.or(daemon);
    }
    checked?;
    let closed = Instant::now();
    b.until("B reconnects to serve once desk A closes", RECONNECT, |view| {
        view.drops > drops && connected(view, machine)
    })?;
    ensure!(lock_owner(&run.a.data) == Some(serve), "serve {serve} did not take studio.lock back");
    run.report.push(format!("desk A closed to B reconnected to serve: {} ms", closed.elapsed().as_millis()));
    println!("ok   serve {serve} took studio.lock back and serves B again");
    Ok(())
}

fn files_refused(run: &Run, b: &mut Desk, thread: &str) -> anyhow::Result<()> {
    let replies = b.view.replies(thread);
    let file = run.work.join("notes.txt").to_string_lossy().into_owned();
    let prompt = PromptDraft { text: PING.to_string(), images: Vec::new(), files: vec![file] };
    b.studio.send("files-1".to_string(), thread.to_string(), prompt, Delivery::Default);
    b.until("a remote send with a file attached is refused", QUICK, |view| view.failed.contains_key("files-1"))?;
    let message = b.view.failed.get("files-1").cloned().unwrap_or_default();
    ensure!(message.contains(NAME), "the refusal does not name the machine: {message}");
    b.hold(Duration::from_secs(2));
    ensure!(b.view.replies(thread) == replies, "the refused send still ran a turn on A");
    println!("ok   a remote send with files is refused before it leaves B: {message}");
    Ok(())
}

fn open_pairing_phone(run: &Run, desk: &mut Desk, machine: &str, device: &str) -> anyhow::Result<()> {
    let (_, link, _) = desk.view.pairing.clone().context("open_pairing gave no link")?;
    let parsed = pairing::parse(&link, CHANNEL).map_err(anyhow::Error::msg)?;
    let local = format!("ws://127.0.0.1:{}/v1/ws", run.serve.port);
    let direct = pairing::link(CHANNEL, &local, &parsed.name, &parsed.id, &parsed.secret);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let phone = runtime.block_on(Phone::pair("phone on desk A", &direct, "phone-open-pairing"))?;
    ensure!(phone.machine == machine, "the open_pairing phone was answered by machine {}", phone.machine);
    println!(
        "ok   a phone pairs through desk A's open_pairing secret ({} for {}) with its in-process host",
        parsed.id, parsed.url
    );
    desk.view.devices = None;
    desk.studio.list_paired_devices();
    desk.until("desk A lists the phone it paired", QUICK, |view| {
        view.devices.as_ref().is_some_and(|devices| devices.iter().any(|known| known.id == "phone-open-pairing"))
    })?;
    desk.view.devices = None;
    desk.studio.revoke_paired_device("phone-open-pairing".to_string());
    desk.until("revoking the phone on desk A leaves only B", QUICK, |view| {
        view.devices.as_ref().is_some_and(|devices| devices.len() == 1 && devices[0].id == device)
    })?;
    runtime.block_on(phone.until("the revoked phone's socket closes", QUICK, |frames| {
        frames.iter().any(|frame| frame["op"] == "closed")
    }))?;
    drop(runtime);
    println!("ok   desk A lists the phone, revokes it live, and lists only B again");
    Ok(())
}

fn test_ca(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = root.join("tls");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("leaf.ext"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
    )?;
    let ca: &[&str] = &[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        "-days",
        "2",
        "-subj",
        "/CN=link smoke test CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ];
    let request: &[&str] =
        &["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "leaf.key", "-out", "leaf.csr", "-subj", "/CN=localhost"];
    let sign: &[&str] = &[
        "x509",
        "-req",
        "-in",
        "leaf.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "leaf.pem",
        "-days",
        "2",
        "-extfile",
        "leaf.ext",
    ];
    for args in [ca, request, sign] {
        let output = Command::new("openssl").args(args).current_dir(&dir).output().context("run openssl")?;
        ensure!(output.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(dir)
}

struct Terminator {
    port: u16,
    handshakes: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Terminator {
    fn start(tls: &Path, upstream: u16) -> anyhow::Result<Self> {
        use tokio_rustls::rustls;
        use tokio_rustls::rustls::pki_types::pem::PemObject;
        let certs =
            rustls::pki_types::CertificateDer::pem_file_iter(tls.join("leaf.pem"))?.collect::<Result<Vec<_>, _>>()?;
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(tls.join("leaf.key"))?;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let handshakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&handshakes);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                tokio::pin!(stopped);
                loop {
                    let accepted = tokio::select! {
                        _ = &mut stopped => return,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((tcp, _)) = accepted else { continue };
                    let acceptor = acceptor.clone();
                    let counted = Arc::clone(&counted);
                    tokio::spawn(async move {
                        let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                        counted.fetch_add(1, Ordering::SeqCst);
                        let Ok(mut upstream) = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await else {
                            return;
                        };
                        tokio::io::copy_bidirectional(&mut tls, &mut upstream).await.ok();
                    });
                }
            });
        });
        Ok(Self { port, handshakes, stop: Some(stop), thread: Some(thread) })
    }

    fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn wss_checks(run: &mut Run, b: &mut Desk) -> anyhow::Result<()> {
    let terminator = Terminator::start(&run.tls, run.serve.port)?;
    let url = format!("wss://localhost:{}/v1/ws", terminator.port);
    let checked = (|| {
        run.serve.run(&run.a, &["allow-host", "localhost"])?;
        let printed = run.serve.run(&run.a, &["pair", "--url", &url, "--name", NAME])?;
        let own = format!("{}://pair?", pairing::scheme(CHANNEL));
        let link = printed.lines().find(|line| line.starts_with(&own)).context("pair printed no link")?.to_string();
        let started = Instant::now();
        b.studio.pair_machine(link);
        b.until("B pairs with A over wss:// through a TLS terminator signed by the test CA", SETUP, |view| {
            view.machines.iter().any(|machine| machine.url == url && machine.state == MachineState::Connected)
        })?;
        run.report.push(format!(
            "wss pair_machine to connected (TLS + XXpsk3 + hello): {} ms",
            started.elapsed().as_millis()
        ));
        let machine = b.view.machines.iter().find(|machine| machine.url == url).context("no wss machine")?.id.clone();
        let handshakes = terminator.handshakes.load(Ordering::SeqCst);
        ensure!(handshakes >= 1, "the TLS terminator completed no handshake");
        b.until("A's rows arrive over wss", QUICK, |view| {
            view.threads.iter().any(|row| row.machine.as_deref() == Some(machine.as_str()))
        })?;
        println!(
            "ok   B links to A over {url} ({handshakes} TLS handshake(s) under the test CA) and lists A's threads"
        );
        b.studio.forget_machine(machine);
        b.until("forget_machine removes the wss machine", QUICK, |view| view.machines.is_empty())?;
        anyhow::Ok(())
    })();
    terminator.stop();
    checked
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
        phone_checks(run, &mut b, &machine, &thread)?;
        freeze_checks(run, &mut b, &machine, &thread)?;
        reconnect_checks(run, &mut b, &machine, &thread)?;
        devices_checks(run, &mut b, &machine)?;
        let seen = b.view.items(&thread).to_vec();
        revoke_checks(run, &mut b, &machine)?;
        wss_checks(run, &mut b)?;
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
    let tls = test_ca(&root)?;
    std::env::set_var(TEST_CA, tls.join("ca.pem"));
    let bin = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("local-studio-serve"))
        .filter(|bin| bin.is_file())
        .context("build local-studio-serve first: cargo build -p local-studio-remote --bins --examples")?;
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is unset")?;
    let a = Side::new(&root, "a", Some(home))?;
    let b = Side::new(&root, "b", None)?;
    real_model::install(&b.agent())?;
    let work = a.root.join("work");
    std::fs::create_dir_all(&work)?;
    std::fs::write(work.join("notes.txt"), "link smoke notes\n")?;
    git(&work, &b.home, &["init", "-q", "-b", "main"])?;
    git(&work, &b.home, &["add", "."])?;
    git(&work, &b.home, &["commit", "-q", "-m", "seed"])?;
    let b_work = b.root.join("work");
    std::fs::create_dir_all(&b_work)?;
    std::fs::write(b_work.join("notes.txt"), "link smoke notes on B\n")?;
    git(&b_work, &b.home, &["init", "-q", "-b", "main"])?;
    git(&b_work, &b.home, &["add", "."])?;
    git(&b_work, &b.home, &["commit", "-q", "-m", "seed"])?;
    let serve = Serve { bin, port: free_port()?, child: None };
    let mut run =
        Run { eukhe, a, b, work, b_work, tls, serve, model: String::new(), daemons: Vec::new(), report: Vec::new() };
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
