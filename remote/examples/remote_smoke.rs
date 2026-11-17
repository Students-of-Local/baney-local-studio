use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use futures_util::{SinkExt, StreamExt};
use local_studio_core::{
    AccessMode, Approval, Connection, Item, ItemKind, QueuedMessage, ThreadRow, ThreadStatus, ToolState, TranscriptOp,
    Update, UsageReport, WorktreeChoice,
};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as SocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[path = "../../core/examples/support/real_model.rs"]
mod real_model;

const VERSION: &str = env!("LOCAL_STUDIO_BUILD_VERSION");
const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const STEP: Duration = Duration::from_secs(120);
const QUICK: Duration = Duration::from_secs(15);
const ABORT_BOUND: Duration = Duration::from_secs(20);
const PING_EVERY: Duration = Duration::from_secs(20);
const SLOW_TEXT: &str = "sleep 8 && echo slow";
const QUEUED_TEXT: &str = "Reply with only the word queued.";
const ONCE_TEXT: &str = "Reply with only the word once.";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn bash_prompt(command: &str) -> String {
    format!("Use the bash tool to run exactly this command once: `{command}`. Then reply with only the word done.")
}

fn scheme() -> String {
    if CHANNEL == "stable" {
        "localstudio".to_string()
    } else {
        format!("localstudio-{CHANNEL}")
    }
}

fn prompt(text: &str) -> Value {
    json!({ "text": text, "images": [], "files": [] })
}

fn settings() -> anyhow::Result<Value> {
    Ok(serde_json::to_value(real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local))?)
}

#[derive(Default)]
struct View {
    hello: Option<Value>,
    paired: Option<String>,
    unpaired: bool,
    incompatible: Option<Value>,
    closed: bool,
    broken: Option<String>,
    seq: u64,
    seq_gaps: u32,
    replies: HashMap<u64, Value>,
    connection: Option<Connection>,
    daemon_pid: Option<u32>,
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    started: HashMap<String, String>,
    items: HashMap<String, Vec<Item>>,
    earlier: HashMap<String, usize>,
    streamed: HashMap<String, Vec<ItemKind>>,
    transcript_frames: HashMap<String, u32>,
    busy: HashMap<String, bool>,
    access: HashMap<String, AccessMode>,
    approvals: HashMap<String, Vec<Approval>>,
    queues: HashMap<String, Vec<QueuedMessage>>,
    queued_texts: HashMap<String, Vec<String>>,
    usage: Option<UsageReport>,
}

impl View {
    fn items(&self, thread: &str) -> &[Item] {
        self.items.get(thread).map_or(&[], Vec::as_slice)
    }

    fn working(&self, thread: &str) -> bool {
        self.threads.iter().any(|row| row.id == thread && matches!(row.status, ThreadStatus::Working))
    }

    fn idle(&self, thread: &str) -> bool {
        !self.working(thread) && self.busy.get(thread) == Some(&false)
    }

    fn replies_done(&self, thread: &str) -> usize {
        self.items(thread)
            .iter()
            .filter(|item| matches!(&item.kind, ItemKind::Assistant { streaming: false, error: None, .. }))
            .count()
    }

    fn user_index(&self, thread: &str, text: &str) -> Option<usize> {
        self.items(thread)
            .iter()
            .position(|item| matches!(&item.kind, ItemKind::User { text: said, .. } if said.contains(text)))
    }

    fn users_saying(&self, thread: &str, text: &str) -> usize {
        self.items(thread)
            .iter()
            .filter(|item| matches!(&item.kind, ItemKind::User { text: said, .. } if said.contains(text)))
            .count()
    }

    fn tool_running(&self, thread: &str, command: &str) -> bool {
        self.items(thread).iter().any(|item| {
            matches!(&item.kind, ItemKind::Tool { state: ToolState::Running | ToolState::Pending, arguments, .. } if arguments.contains(command))
        })
    }

    fn tool_index(&self, thread: &str, command: &str) -> Option<usize> {
        self.items(thread)
            .iter()
            .position(|item| matches!(&item.kind, ItemKind::Tool { arguments, .. } if arguments.contains(command)))
    }

    fn answered_after(&self, thread: &str, index: usize) -> bool {
        self.items(thread)
            .iter()
            .skip(index)
            .any(|item| matches!(&item.kind, ItemKind::Assistant { streaming: false, error: None, .. }))
    }

    fn streamed(&self, thread: &str, kind: fn(&ItemKind) -> bool) -> bool {
        self.streamed.get(thread).is_some_and(|kinds| kinds.iter().any(kind))
    }

    fn apply_frame(&mut self, text: &str) -> anyhow::Result<()> {
        let frame: Value = serde_json::from_str(text).context("a frame is not JSON")?;
        match frame.get("op").and_then(Value::as_str) {
            Some("hello") => {
                for update in frame.get("snapshot").and_then(Value::as_array).context("hello has no snapshot")? {
                    self.apply_update(update)?;
                }
                self.hello = Some(frame);
            }
            Some("paired") => {
                self.paired =
                    Some(frame.get("credential").and_then(Value::as_str).context("no credential")?.to_string());
            }
            Some("unpaired") => self.unpaired = true,
            Some("incompatible") => self.incompatible = Some(frame),
            Some("update") => {
                let seq = frame.get("seq").and_then(Value::as_u64).context("an update has no seq")?;
                if seq != self.seq + 1 {
                    self.seq_gaps += 1;
                }
                self.seq = seq;
                self.apply_update(frame.get("update").context("an update frame has no update")?)?;
            }
            Some("pong") => {}
            _ => {
                let id = frame.get("re").and_then(Value::as_u64).with_context(|| format!("unexpected frame {text}"))?;
                self.replies.insert(id, frame);
            }
        }
        Ok(())
    }

    fn apply_update(&mut self, raw: &Value) -> anyhow::Result<()> {
        let update: Update = serde_json::from_value(raw.clone()).with_context(|| format!("decode update {raw}"))?;
        match update {
            Update::Connection { state } => self.connection = Some(state),
            Update::Daemon { pid, .. } => self.daemon_pid = pid,
            Update::Projects { projects } => self.projects = projects.into_iter().map(|project| project.id).collect(),
            Update::Threads { threads } => self.threads = threads,
            Update::ThreadStarted { draft, thread } => {
                self.started.insert(draft, thread);
            }
            Update::Thread { thread, info } => {
                self.busy.insert(thread.clone(), info.busy);
                self.access.insert(thread, info.access);
            }
            Update::Transcript { thread, op } => {
                *self.transcript_frames.entry(thread.clone()).or_default() += 1;
                self.transcript(raw, &thread, op);
            }
            Update::Approvals { thread, approvals } => {
                self.approvals.insert(thread, approvals);
            }
            Update::Queue { thread, queued } => {
                let seen = self.queued_texts.entry(thread.clone()).or_default();
                seen.extend(queued.iter().map(|message| message.text.clone()));
                self.queues.insert(thread, queued);
            }
            Update::Usage { report, .. } => self.usage = Some(report),
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestFailed { request, message } => println!("     request {request} failed: {message}"),
            _ => {}
        }
        Ok(())
    }

    fn transcript(&mut self, raw: &Value, thread: &str, op: TranscriptOp) {
        match op {
            TranscriptOp::Reset { items } => {
                let earlier = raw.pointer("/op/earlier").and_then(Value::as_u64).unwrap_or_default();
                self.earlier.insert(thread.to_string(), usize::try_from(earlier).unwrap_or_default());
                self.items.insert(thread.to_string(), items);
            }
            TranscriptOp::Insert { index, item } => {
                self.streamed.entry(thread.to_string()).or_default().push(item.kind.clone());
                let at = self.local_index(thread, index);
                let items = self.items.entry(thread.to_string()).or_default();
                items.insert(at.min(items.len()), item);
            }
            TranscriptOp::Update { index, item } => {
                self.streamed.entry(thread.to_string()).or_default().push(item.kind.clone());
                let at = self.local_index(thread, index);
                if let Some(slot) = self.items.entry(thread.to_string()).or_default().get_mut(at) {
                    *slot = item;
                }
            }
        }
    }

    fn local_index(&self, thread: &str, index: u64) -> usize {
        let earlier = self.earlier.get(thread).copied().unwrap_or_default();
        usize::try_from(index).unwrap_or(usize::MAX).saturating_sub(earlier)
    }
}

struct Client {
    name: String,
    out: mpsc::UnboundedSender<Message>,
    view: Arc<Mutex<View>>,
    changed: Arc<Notify>,
    next: u64,
}

async fn connect(port: u16, host: Option<&str>, origin: Option<&str>) -> anyhow::Result<Result<Socket, u16>> {
    let mut request = format!("ws://127.0.0.1:{port}/v1/ws").into_client_request()?;
    if let Some(host) = host {
        request.headers_mut().insert("host", HeaderValue::from_str(host)?);
    }
    if let Some(origin) = origin {
        request.headers_mut().insert("origin", HeaderValue::from_str(origin)?);
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(Ok(socket)),
        Err(SocketError::Http(response)) => Ok(Err(response.status().as_u16())),
        Err(error) => Err(error.into()),
    }
}

impl Client {
    fn spawn(name: &str, socket: Socket) -> Self {
        let (mut sink, mut stream) = socket.split();
        let (out, mut rx) = mpsc::unbounded_channel::<Message>();
        let view = Arc::new(Mutex::new(View::default()));
        let changed = Arc::new(Notify::new());
        let reader_view = Arc::clone(&view);
        let reader_changed = Arc::clone(&changed);
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let mut view = reader_view.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Err(error) = view.apply_frame(&text) {
                            view.broken.get_or_insert(format!("{error:#}"));
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
                reader_changed.notify_waiters();
            }
            reader_view.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
            reader_changed.notify_waiters();
        });
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_EVERY);
            ping.tick().await;
            loop {
                tokio::select! {
                    message = rx.recv() => {
                        let Some(message) = message else {
                            sink.close().await.ok();
                            break;
                        };
                        if sink.send(message).await.is_err() {
                            break;
                        }
                    }
                    _ = ping.tick() => {
                        if sink.send(Message::text(json!({ "op": "ping" }).to_string())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { name: name.to_string(), out, view, changed, next: 0 }
    }

    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn frame(&self, frame: &Value) {
        self.out.send(Message::text(frame.to_string())).ok();
    }

    async fn until(&self, step: &str, bound: Duration, done: impl Fn(&View) -> bool) -> anyhow::Result<Duration> {
        let started = Instant::now();
        let deadline = started + bound;
        loop {
            let notified = self.changed.notified();
            {
                let view = self.view();
                if let Some(broken) = &view.broken {
                    bail!("{}: the wire broke: {broken}", self.name);
                }
                if done(&view) {
                    let took = started.elapsed();
                    println!("ok   {}: {step} ({:.1} s)", self.name, took.as_secs_f64());
                    return Ok(took);
                }
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("{}: timed out after {} s waiting for: {step}", self.name, bound.as_secs());
            };
            tokio::time::timeout(left.min(Duration::from_millis(200)), notified).await.ok();
        }
    }

    async fn call(&mut self, op: &str, args: Value) -> anyhow::Result<Value> {
        self.next += 1;
        let id = self.next;
        self.frame(&json!({ "id": id, "op": op, "args": args }));
        let deadline = Instant::now() + QUICK;
        loop {
            let notified = self.changed.notified();
            if let Some(reply) = self.view().replies.remove(&id) {
                if let Some(error) = reply.get("error") {
                    bail!("{}: {op} was refused: {error}", self.name);
                }
                return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("{}: no reply to {op}", self.name);
            };
            tokio::time::timeout(left.min(Duration::from_millis(200)), notified).await.ok();
        }
    }
}

struct Serve {
    bin: PathBuf,
    root: PathBuf,
    data: PathBuf,
    port: u16,
    child: Option<Child>,
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl Serve {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .env("EUKHE_DAEMON_SOCKET", self.root.join("d.sock"))
            .env("EUKHE_CODING_AGENT_DIR", self.root.join("agent"))
            .env("LOCAL_STUDIO_DATA_DIR", &self.data);
        command
    }

    fn start(&mut self, eukhe: &Path, hook: &str) -> anyhow::Result<()> {
        let log = std::fs::File::create(self.root.join("serve.log"))?;
        let child = self
            .command()
            .args(["serve", "--port", &self.port.to_string(), "--name", "smoke-mac"])
            .arg("--eukhe")
            .arg(eukhe)
            .args(["--notify", hook])
            .arg("--data-dir")
            .arg(&self.data)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .context("start local-studio-serve")?;
        println!("     serve pid {} on 127.0.0.1:{}", child.id(), self.port);
        self.child = Some(child);
        Ok(())
    }

    fn log_tail(&self) -> String {
        let log = std::fs::read_to_string(self.root.join("serve.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }

    fn exited(&mut self) -> anyhow::Result<bool> {
        Ok(match self.child.as_mut() {
            Some(child) => child.try_wait()?.is_some(),
            None => true,
        })
    }

    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = self.command().args(args).arg("--data-dir").arg(&self.data).output()?;
        ensure!(output.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn pairing_secret(&self) -> anyhow::Result<String> {
        let url = format!("ws://127.0.0.1:{}/v1/ws", self.port);
        let printed = self.run(&["pair", "--url", &url, "--name", "smoke-mac"])?;
        let link = printed
            .lines()
            .find(|line| line.starts_with(&format!("{}://pair?", scheme())))
            .context("pair printed no link")?;
        ensure!(printed.contains('\u{2588}') || printed.contains('\u{2580}'), "pair printed no QR code");
        let (_, secret) = link.split_once('#').context("the pairing link has no secret")?;
        Ok(secret.to_string())
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

async fn http_get(port: u16, host: &str, path: &str) -> anyhow::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await?;
    let status = answer.split_whitespace().nth(1).and_then(|code| code.parse().ok()).context("no status")?;
    let body = answer.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    Ok((status, body))
}

async fn hook_listener() -> anyhow::Result<(String, Arc<Mutex<Vec<Value>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://127.0.0.1:{}/hook", listener.local_addr()?.port());
    let posts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&posts);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(read) = stream.read(&mut chunk).await else { return };
                    if read == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&buffer).into_owned();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
                    let length = head
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(str::to_string))
                        .and_then(|length| length.trim().parse::<usize>().ok())
                        .unwrap_or_default();
                    if body.len() < length {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(body) {
                        seen.lock().unwrap_or_else(PoisonError::into_inner).push(value);
                    }
                    stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.ok();
                    return;
                }
            });
        }
    });
    Ok((url, posts))
}

async fn pair_device(serve: &Serve, device: &str, name: &str) -> anyhow::Result<String> {
    let secret = serve.pairing_secret()?;
    let socket = connect(serve.port, None, None).await?.map_err(|status| anyhow::anyhow!("refused: {status}"))?;
    let client = Client::spawn(&format!("pair {device}"), socket);
    let info = json!({ "id": device, "name": name, "platform": "ios", "app": "0.1.0", "channel": "dev" });
    client.frame(&json!({ "op": "pair", "secret": secret, "device": info }));
    client.until("paired with a one-time secret", QUICK, |view| view.paired.is_some() || view.unpaired).await?;
    let credential = client.view().paired.clone().context("the pairing was refused")?;
    ensure!(credential.len() == 64 && credential.bytes().all(|byte| byte.is_ascii_hexdigit()), "bad credential");
    let reuse = connect(serve.port, None, None).await?.map_err(|status| anyhow::anyhow!("refused: {status}"))?;
    let reuse = Client::spawn(&format!("pair {device} again"), reuse);
    reuse.frame(&json!({ "op": "pair", "secret": secret, "device": info }));
    reuse.until("a burned secret is refused with unpaired", QUICK, |view| view.unpaired).await?;
    Ok(credential)
}

async fn attach(port: u16, name: &str, device: &str, credential: &str) -> anyhow::Result<(Client, Duration)> {
    let started = Instant::now();
    let socket = connect(port, None, None).await?.map_err(|status| anyhow::anyhow!("refused: {status}"))?;
    let client = Client::spawn(name, socket);
    client
        .frame(&json!({ "op": "hello", "device": device, "credential": credential, "protocol": 1, "min_protocol": 1 }));
    client.until("hello answered", QUICK, |view| view.hello.is_some() || view.unpaired).await?;
    Ok((client, started.elapsed()))
}

fn file_holds(work: &Path, name: &str, text: &str) -> anyhow::Result<()> {
    let found = std::fs::read_to_string(work.join(name)).with_context(|| format!("{name} was not written"))?;
    ensure!(found.contains(text), "{name} holds {found:?}, expected {text:?}");
    println!("ok   the real bash call wrote {name}");
    Ok(())
}

struct Run {
    serve: Serve,
    work: PathBuf,
    hook: String,
    posts: Arc<Mutex<Vec<Value>>>,
    daemon_pid: Option<u32>,
}

async fn transport_checks(run: &mut Run, eukhe: &Path) -> anyhow::Result<()> {
    run.serve.start(eukhe, &run.hook)?;
    let port = run.serve.port;
    let host = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + QUICK;
    let health = loop {
        if let Ok(answer) = http_get(port, &host, "/v1/health").await {
            break answer;
        }
        if run.serve.exited()? || Instant::now() >= deadline {
            bail!("serve did not come up:\n{}", run.serve.log_tail());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    ensure!(
        health.0 == 200 && health.1.contains("\"protocol\":1") && health.1.contains("\"min_protocol\":1"),
        "health answered {health:?}"
    );
    println!("ok   /v1/health answers {} without auth", health.1);
    let foreign = http_get(port, "evil.example", "/v1/health").await?;
    ensure!(foreign.0 == 403, "a foreign Host got {}", foreign.0);
    println!("ok   a foreign Host is refused with 403");
    let second = Serve {
        bin: run.serve.bin.clone(),
        root: run.serve.root.clone(),
        data: run.serve.data.clone(),
        port: free_port()?,
        child: None,
    };
    let mut rival = second
        .command()
        .args(["serve", "--port", &second.port.to_string(), "--eukhe"])
        .arg(eukhe)
        .arg("--data-dir")
        .arg(&second.data)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + QUICK;
    while rival.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            Command::new("kill").args(["-KILL", &rival.id().to_string()]).status()?;
            rival.wait()?;
            bail!("a second serve on the same data dir kept running");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let output = rival.wait_with_output()?;
    let said = String::from_utf8_lossy(&output.stderr);
    ensure!(!output.status.success() && said.contains("already owns"), "a second serve said: {said}");
    println!("ok   a second Studio on the data dir is refused: {}", said.trim());
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn drive(run: &mut Run, eukhe: &Path) -> anyhow::Result<()> {
    transport_checks(run, eukhe).await?;
    let port = run.serve.port;
    let credential_a = pair_device(&run.serve, "phone-a", "Smoke Phone A").await?;
    let (mut a, hello_latency) = attach(port, "phone A", "phone-a", &credential_a).await?;
    {
        let view = a.view();
        let hello = view.hello.as_ref().context("phone A got no hello")?;
        let kinds: Vec<&str> = hello
            .get("snapshot")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|update| update.get("type").and_then(Value::as_str))
            .collect();
        ensure!(
            hello.pointer("/machine/version") == Some(&json!(VERSION))
                && hello.pointer("/machine/channel") == Some(&json!(CHANNEL))
                && hello.get("protocol") == Some(&json!(1))
                && hello.get("min_protocol") == Some(&json!(1)),
            "hello: {hello}"
        );
        ensure!(
            std::env::var_os("LOCAL_STUDIO_VERSION").is_some() || VERSION.contains("-dev.0+local."),
            "a local build reports version {VERSION}"
        );
        ensure!(kinds.contains(&"connection") && kinds.contains(&"projects"), "snapshot kinds {kinds:?}");
        println!("ok   reconnect with hello returns machine {} and a snapshot of {kinds:?}", hello["machine"]);
    }
    let socket = connect(port, None, None).await?.map_err(|status| anyhow::anyhow!("refused: {status}"))?;
    let future = Client::spawn("phone on protocol 2", socket);
    future.frame(
        &json!({ "op": "hello", "device": "phone-a", "credential": credential_a, "protocol": 2, "min_protocol": 2 }),
    );
    future
        .until("a protocol 2 only hello gets incompatible and a close", QUICK, |view| {
            view.incompatible.is_some() && view.closed
        })
        .await?;
    {
        let view = future.view();
        let answer = view.incompatible.as_ref().context("no incompatible frame")?;
        ensure!(
            answer.get("protocol") == Some(&json!(1))
                && answer.get("min_protocol") == Some(&json!(1))
                && view.hello.is_none(),
            "incompatible answered {answer}"
        );
        println!("ok   incompatible names this server's window: {answer}");
    }
    a.until("daemon connected", STEP, |view| matches!(view.connection, Some(Connection::Ready))).await?;
    run.daemon_pid = a.view().daemon_pid;
    a.call("add_project", json!({ "path": run.work })).await?;
    a.until("project added over the wire", STEP, |view| view.projects.len() == 1).await?;
    let project = a.view().projects[0].clone();

    let credential_b = pair_device(&run.serve, "phone-b", "Smoke Phone B").await?;
    let (mut b, _) = attach(port, "phone B", "phone-b", &credential_b).await?;
    b.call("start_thread", json!({ "draft": "d-bee", "project": project, "prompt": prompt("Reply with only the word bee."), "settings": settings()? })).await?;
    b.until("its own thread started", STEP, |view| view.started.contains_key("d-bee")).await?;
    let bee = b.view().started["d-bee"].clone();
    b.call("watch", json!({ "thread": bee })).await?;
    b.until("its thread's reply streamed", STEP, |view| view.replies_done(&bee) >= 1 && view.idle(&bee)).await?;

    let file_prompt = bash_prompt("printf remote-ok > remote-proof.txt");
    a.call(
        "start_thread",
        json!({ "draft": "d-file", "project": project, "prompt": prompt(&file_prompt), "settings": settings()? }),
    )
    .await?;
    a.until("thread started", STEP, |view| view.started.contains_key("d-file")).await?;
    let thread = a.view().started["d-file"].clone();
    a.call("watch", json!({ "thread": thread })).await?;
    a.until("transcript inserts for the tool call and the reply", STEP, |view| {
        view.streamed(&thread, |kind| matches!(kind, ItemKind::Tool { .. }))
            && view.streamed(&thread, |kind| matches!(kind, ItemKind::Assistant { .. }))
            && view.replies_done(&thread) >= 1
            && view.idle(&thread)
    })
    .await?;
    file_holds(&run.work, "remote-proof.txt", "remote-ok")?;

    let before = a.view().transcript_frames.get(&thread).copied().unwrap_or_default();
    let replies = a.view().replies_done(&thread);
    let sent = Instant::now();
    a.call("send", json!({ "request": "r-latency", "thread": thread, "prompt": prompt("Reply with only the word ok."), "delivery": "queue" })).await?;
    a.until("first transcript update after send", STEP, |view| {
        view.transcript_frames.get(&thread).copied().unwrap_or_default() > before
    })
    .await?;
    let first_update = sent.elapsed();
    a.until("the sent turn finished", STEP, |view| view.replies_done(&thread) > replies && view.idle(&thread)).await?;

    let replies = a.view().replies_done(&thread);
    let args = json!({ "request": "r-once", "thread": thread, "prompt": prompt(ONCE_TEXT), "delivery": "queue" });
    a.call("send", args.clone()).await?;
    let retry = a.call("send", args).await?;
    ensure!(retry.get("duplicate") == Some(&json!(true)), "the retried send was not dropped: {retry}");
    a.until("the once turn finished", STEP, |view| view.replies_done(&thread) > replies && view.idle(&thread)).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let once = a.view().users_saying(&thread, ONCE_TEXT);
    ensure!(once == 1 && a.view().idle(&thread), "the retried send ran {once} times");
    println!("ok   a retried send with the same request id ran once");

    a.call("send", json!({ "request": "r-slow", "thread": thread, "prompt": prompt(&bash_prompt(SLOW_TEXT)), "delivery": "queue" })).await?;
    a.until("the slow bash step runs", STEP, |view| view.tool_running(&thread, SLOW_TEXT) && view.working(&thread))
        .await?;
    a.call(
        "send",
        json!({ "request": "r-queued", "thread": thread, "prompt": prompt(QUEUED_TEXT), "delivery": "queue" }),
    )
    .await?;
    a.until("a queue update shows the follow-up", QUICK, |view| {
        view.queued_texts.get(&thread).is_some_and(|texts| texts.iter().any(|text| text.contains(QUEUED_TEXT)))
    })
    .await?;
    a.until("the follow-up is delivered after the slow turn", STEP, |view| {
        let slow = view.tool_index(&thread, SLOW_TEXT);
        let queued = view.user_index(&thread, QUEUED_TEXT);
        matches!((slow, queued), (Some(slow), Some(queued)) if queued > slow)
            && view.queues.get(&thread).is_some_and(Vec::is_empty)
            && queued.is_some_and(|queued| view.answered_after(&thread, queued))
            && view.idle(&thread)
    })
    .await?;

    a.call("set_access_mode", json!({ "thread": thread, "mode": "supervised" })).await?;
    a.until("the thread is supervised", QUICK, |view| view.access.get(&thread) == Some(&AccessMode::Supervised))
        .await?;
    let replies = a.view().replies_done(&thread);
    a.call("send", json!({ "request": "r-approve", "thread": thread, "prompt": prompt(&bash_prompt("printf approved > approved.txt")), "delivery": "queue" })).await?;
    a.until("an approvals update asks", STEP, |view| view.approvals.get(&thread).is_some_and(|asks| !asks.is_empty()))
        .await?;
    let ask = a.view().approvals[&thread][0].clone();
    println!("     approval asks for {:?}", ask.command);
    a.call("answer_approval", json!({ "thread": thread, "request": ask.request_id, "answer": "approve" })).await?;
    a.until("the approved turn finished", STEP, |view| {
        view.approvals.get(&thread).is_some_and(Vec::is_empty)
            && view.replies_done(&thread) > replies
            && view.idle(&thread)
    })
    .await?;
    file_holds(&run.work, "approved.txt", "approved")?;

    a.call("set_access_mode", json!({ "thread": thread, "mode": "fullAccess" })).await?;
    a.until("the thread has full access", QUICK, |view| view.access.get(&thread) == Some(&AccessMode::FullAccess))
        .await?;
    a.call("send", json!({ "request": "r-sleep", "thread": thread, "prompt": prompt(&bash_prompt("sleep 60 && echo slept")), "delivery": "queue" })).await?;
    a.until("the sleep 60 step runs", STEP, |view| view.tool_running(&thread, "sleep 60") && view.working(&thread))
        .await?;
    a.call("abort", json!({ "thread": thread })).await?;
    a.until("abort: the thread leaves Working within 20 s", ABORT_BOUND, |view| view.idle(&thread)).await?;

    {
        let view = b.view();
        let leaked = view.transcript_frames.get(&thread).copied().unwrap_or_default();
        let own = view.transcript_frames.get(&bee).copied().unwrap_or_default();
        ensure!(leaked == 0, "phone B got {leaked} transcript updates for phone A's thread");
        ensure!(own > 0, "phone B got no transcript updates for its own thread");
        println!("ok   phone B got {own} transcript updates for its thread and none of phone A's");
    }
    ensure!(a.view().seq_gaps == 0 && b.view().seq_gaps == 0, "update seq skipped");
    println!("ok   update seq rose by one per frame on both sockets");
    let posts = run.posts.lock().unwrap_or_else(PoisonError::into_inner).clone();
    ensure!(
        posts.iter().any(|post| post.pointer("/notification/type") == Some(&json!("notification"))
            && post.pointer("/machine/name") == Some(&json!("smoke-mac"))),
        "the notify hook got no notification: {posts:?}"
    );
    println!("ok   the notify webhook got {} notifications", posts.len());

    run.serve.run(&["revoke", "phone-b"])?;
    b.frame(&json!({ "op": "ping" }));
    b.until("a revoked socket gets unpaired and closes", QUICK, |view| view.unpaired && view.closed).await?;
    let (refused, _) = attach(port, "phone B again", "phone-b", &credential_b).await?;
    ensure!(refused.view().unpaired && refused.view().hello.is_none(), "a revoked credential got a hello");
    println!("ok   a revoked credential is refused");
    let foreign = connect(port, Some("evil.example"), None).await?;
    ensure!(matches!(foreign, Err(403)), "a socket for a foreign Host was not refused");
    println!("ok   a socket for a foreign Host is refused with 403");
    let cross = connect(port, None, Some("https://evil.example")).await?;
    ensure!(matches!(cross, Err(403)), "a socket with a mismatched Origin was not refused");
    println!("ok   a socket with a mismatched Origin is refused with 403");
    let same = connect(port, None, Some(&format!("http://127.0.0.1:{port}"))).await?;
    ensure!(same.is_ok(), "a socket with a matching Origin was refused");
    println!("ok   a socket with a matching Origin is accepted");

    a.call("load_usage", json!({ "request": "usage-1", "period": "day" })).await?;
    a.until("usage report", STEP, |view| view.usage.is_some()).await?;
    println!("     hello latency {} ms", hello_latency.as_millis());
    println!("     send to first transcript update latency {} ms", first_update.as_millis());
    let report = a.view().usage.clone().context("usage")?;
    real_model::check_usage(&report)
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
        pid = pid.or_else(|| {
            client.hello().get("supervisorPid").and_then(Value::as_u64).and_then(|pid| u32::try_from(pid).ok())
        });
        let command = DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() };
        client.request_ok(command).await.ok();
        client.close();
    }
    let Some(pid) = pid else {
        println!("ok   no daemon was running");
        return Ok(());
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        if Instant::now() >= deadline {
            bail!("daemon {pid} is still running after Shutdown");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("ok   daemon {pid} stopped");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let usage = "usage: remote_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let eukhe = PathBuf::from(args.next().context(usage)?);
    let root = PathBuf::from(args.next().context(usage)?);
    let bin = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("local-studio-serve"))
        .filter(|bin| bin.is_file())
        .context("build local-studio-serve first: cargo build -p local-studio-remote --bins --examples")?;
    let work = root.join("work");
    std::fs::create_dir_all(&work)?;
    let _credentials = real_model::install(&root.join("agent"))?;
    let (hook, posts) = hook_listener().await?;
    let serve = Serve { bin, root: root.clone(), data: root.join("data"), port: free_port()?, child: None };
    let mut run = Run { serve, work, hook, posts, daemon_pid: None };
    let outcome = drive(&mut run, &eukhe).await;
    if outcome.is_err() {
        println!("     serve log tail:\n{}", run.serve.log_tail());
    }
    let stopped = run.serve.stop();
    let daemon = stop_daemon(&root.join("d.sock"), run.daemon_pid).await;
    outcome?;
    stopped?;
    daemon?;
    println!("PASS remote smoke");
    Ok(())
}
