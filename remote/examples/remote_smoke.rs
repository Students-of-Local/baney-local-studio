use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use base64::Engine;
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use local_studio_core::{
    AccessMode, Approval, Connection, DaemonLaunch, Item, ItemKind, Studio, StudioConfig, ThreadRow, ThreadSettings,
    ThreadStatus, TranscriptOp, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};
use snow::TransportState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as SocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const VERSION: &str = env!("LOCAL_STUDIO_BUILD_VERSION");
const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const XX: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
const KK: &str = "Noise_KK_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"local-studio-remote/1\n";
const QUICK: Duration = Duration::from_secs(20);
const SETUP: Duration = Duration::from_secs(120);
const MODEL_STEP: Duration = Duration::from_mins(40);
const ABORT_BOUND: Duration = Duration::from_secs(20);
const PING_EVERY: Duration = Duration::from_secs(20);
const TUNNEL: &str = "tunnel.smoke.test";
const ONCE_TEXT: &str = "Reply with only the word once.";
const SLEEP_COMMAND: &str = "sleep 60 && echo slept";
const UPDATE_TYPES: [&str; 28] = [
    "connection",
    "projects",
    "threads",
    "thread_started",
    "models",
    "daemon_default",
    "notification",
    "theme",
    "notice",
    "request_done",
    "request_failed",
    "thread",
    "transcript",
    "approvals",
    "questions",
    "questions_settled",
    "queue",
    "subagents",
    "worktree_setup",
    "thread_search",
    "archived_threads",
    "commands",
    "skills",
    "git_status",
    "diff",
    "pull_requests",
    "pull_request_detail",
    "blob",
];

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        write!(text, "{byte:02x}").ok();
        text
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn png(size: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..size {
        raw.push(0u8);
        for x in 0..size {
            let on = (x / 8 + y / 8) % 2 == 0;
            raw.extend_from_slice(if on { &[220, 40, 40] } else { &[250, 250, 250] });
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (at, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(at + 1 == blocks.len()));
        let length = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        file.extend_from_slice(&u32::try_from(data.len()).unwrap_or_default().to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        file.extend_from_slice(&body);
        file.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    file
}

struct Received {
    request: String,
    size: usize,
    mime: String,
    data: Vec<u8>,
    done: bool,
    extra: Value,
}

#[derive(Default)]
struct View {
    hello: Option<Value>,
    incompatible: Option<Value>,
    closed: bool,
    close_code: Option<u16>,
    broken: Option<String>,
    seq: u64,
    seq_gaps: u32,
    updates: u32,
    replies: HashMap<u64, Value>,
    pongs: u32,
    kinds: HashSet<String>,
    connection: Option<Connection>,
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    started: HashMap<String, String>,
    items: HashMap<String, Vec<Item>>,
    earlier: HashMap<String, usize>,
    streamed: HashMap<String, Vec<ItemKind>>,
    transcript_frames: HashMap<String, u32>,
    thread_updates: HashMap<String, u32>,
    first_text_at: HashMap<String, Instant>,
    busy: HashMap<String, bool>,
    access: HashMap<String, AccessMode>,
    approvals: HashMap<String, Vec<Approval>>,
    answers: HashMap<String, String>,
    models: u32,
    git_status: u32,
    notifications: u32,
    blobs: HashMap<String, Received>,
}

impl View {
    fn items(&self, thread: &str) -> &[Item] {
        self.items.get(thread).map_or(&[], Vec::as_slice)
    }

    fn row(&self, thread: &str) -> Option<&ThreadRow> {
        self.threads.iter().find(|row| row.id == thread)
    }

    fn working(&self, thread: &str) -> bool {
        self.row(thread).is_some_and(|row| matches!(row.status, ThreadStatus::Working))
    }

    fn idle(&self, thread: &str) -> bool {
        !self.working(thread) && self.busy.get(thread) == Some(&false)
    }

    fn tools(&self, thread: &str) -> usize {
        self.items(thread).iter().filter(|item| matches!(&item.kind, ItemKind::Tool { .. })).count()
    }

    fn replies_done(&self, thread: &str) -> usize {
        self.items(thread)
            .iter()
            .filter(|item| matches!(&item.kind, ItemKind::Assistant { streaming: false, error: None, .. }))
            .count()
    }

    fn users_saying(&self, thread: &str, text: &str) -> usize {
        self.items(thread)
            .iter()
            .filter(|item| matches!(&item.kind, ItemKind::User { text: said, .. } if said.contains(text)))
            .count()
    }

    fn streamed(&self, thread: &str, kind: fn(&ItemKind) -> bool) -> bool {
        self.streamed.get(thread).is_some_and(|kinds| kinds.iter().any(kind))
    }

    fn apply_frame(&mut self, text: &str) -> anyhow::Result<()> {
        let frame: Value = serde_json::from_str(text).context("a frame is not JSON")?;
        match frame.get("op").and_then(Value::as_str) {
            Some("hello") => {
                self.seq = frame.get("seq").and_then(Value::as_u64).context("hello has no seq")?;
                for update in frame.get("snapshot").and_then(Value::as_array).context("hello has no snapshot")? {
                    self.apply_update(update)?;
                }
                self.hello = Some(frame);
            }
            Some("update") => {
                let seq = frame.get("seq").and_then(Value::as_u64).context("an update has no seq")?;
                if seq != self.seq + 1 {
                    self.seq_gaps += 1;
                }
                self.seq = seq;
                self.updates += 1;
                self.apply_update(frame.get("update").context("an update frame has no update")?)?;
            }
            Some("pong") => self.pongs += 1,
            _ => {
                let id = frame.get("re").and_then(Value::as_u64).with_context(|| format!("unexpected frame {text}"))?;
                self.replies.insert(id, frame);
            }
        }
        Ok(())
    }

    fn apply_update(&mut self, raw: &Value) -> anyhow::Result<()> {
        let kind = raw.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        ensure!(UPDATE_TYPES.contains(&kind.as_str()), "an update type outside the protocol 2 table: {kind}");
        self.kinds.insert(kind.clone());
        if ["thread", "transcript", "approvals", "queue", "subagents", "worktree_setup"].contains(&kind.as_str()) {
            if let Some(thread) = raw.get("thread").and_then(Value::as_str) {
                *self.thread_updates.entry(thread.to_string()).or_default() += 1;
            }
        }
        if kind == "blob" {
            let blob = raw.get("blob").and_then(Value::as_str).context("a blob update has no id")?.to_string();
            self.blobs.insert(
                blob,
                Received {
                    request: raw.get("request").and_then(Value::as_str).unwrap_or_default().to_string(),
                    size: raw
                        .get("size")
                        .and_then(Value::as_u64)
                        .and_then(|size| usize::try_from(size).ok())
                        .unwrap_or_default(),
                    mime: raw.get("mime").and_then(Value::as_str).unwrap_or_default().to_string(),
                    data: Vec::new(),
                    done: false,
                    extra: raw.clone(),
                },
            );
            return Ok(());
        }
        if let Some(request) = raw.get("request").and_then(Value::as_str) {
            self.answers.insert(request.to_string(), kind.clone());
        }
        let update: Update = serde_json::from_value(raw.clone()).with_context(|| format!("decode update {raw}"))?;
        match update {
            Update::Connection { state } => self.connection = Some(state),
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
            Update::Models { .. } => self.models += 1,
            Update::GitStatus { .. } => self.git_status += 1,
            Update::Notification { .. } => self.notifications += 1,
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestFailed { request, message } => println!("     request {request} failed: {message}"),
            _ => {}
        }
        Ok(())
    }

    fn chunk(&mut self, blob: &[u8], last: bool, data: &[u8]) -> anyhow::Result<()> {
        let received =
            self.blobs.get_mut(&hex(blob)).with_context(|| format!("a chunk for unknown blob {}", hex(blob)))?;
        ensure!(!received.done, "a chunk after blob {} was complete", hex(blob));
        received.data.extend_from_slice(data);
        ensure!(received.data.len() <= received.size, "blob {} is longer than announced", hex(blob));
        received.done = last;
        Ok(())
    }

    fn transcript(&mut self, raw: &Value, thread: &str, op: TranscriptOp) {
        match op {
            TranscriptOp::Reset { items } => {
                let earlier = raw.pointer("/op/earlier").and_then(Value::as_u64).unwrap_or_default();
                self.earlier.insert(thread.to_string(), usize::try_from(earlier).unwrap_or_default());
                self.items.insert(thread.to_string(), items);
            }
            TranscriptOp::Insert { index, item } | TranscriptOp::Update { index, item } => {
                let insert = raw.pointer("/op/type").and_then(Value::as_str) == Some("insert");
                if matches!(&item.kind, ItemKind::Assistant { .. }) {
                    self.first_text_at.entry(thread.to_string()).or_insert_with(Instant::now);
                }
                self.streamed.entry(thread.to_string()).or_default().push(item.kind.clone());
                let earlier = self.earlier.get(thread).copied().unwrap_or_default();
                let at = usize::try_from(index).unwrap_or(usize::MAX).saturating_sub(earlier);
                let items = self.items.entry(thread.to_string()).or_default();
                if insert {
                    items.insert(at.min(items.len()), item);
                } else if let Some(slot) = items.get_mut(at) {
                    *slot = item;
                }
            }
        }
    }
}

#[derive(Default, Clone)]
struct Route {
    host: Option<String>,
    origin: Option<String>,
    from: Option<String>,
    forwarded_host: Option<String>,
}

impl Route {
    fn tunnel(address: &str) -> Self {
        Self { host: Some(TUNNEL.to_string()), from: Some(address.to_string()), ..Self::default() }
    }
}

async fn connect(port: u16, route: &Route) -> anyhow::Result<Result<Socket, u16>> {
    let mut request = format!("ws://127.0.0.1:{port}/v1/ws").into_client_request()?;
    for (name, value) in [
        ("host", &route.host),
        ("origin", &route.origin),
        ("cf-connecting-ip", &route.from),
        ("x-forwarded-host", &route.forwarded_host),
    ] {
        if let Some(value) = value {
            request.headers_mut().insert(name, HeaderValue::from_str(value)?);
        }
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(Ok(socket)),
        Err(SocketError::Http(response)) => Ok(Err(response.status().as_u16())),
        Err(error) => Err(error.into()),
    }
}

enum Raw {
    Binary(Vec<u8>),
    Text(String),
    Closed(Option<u16>),
}

async fn raw_next(socket: &mut Socket, bound: Duration) -> anyhow::Result<Raw> {
    let deadline = Instant::now() + bound;
    loop {
        let left = deadline.checked_duration_since(Instant::now()).context("no message from the server in time")?;
        let message = tokio::time::timeout(left, socket.next()).await.context("no message from the server in time")?;
        return Ok(match message {
            Some(Ok(Message::Binary(bytes))) => Raw::Binary(bytes.to_vec()),
            Some(Ok(Message::Text(text))) => Raw::Text(text.to_string()),
            Some(Ok(Message::Close(frame))) => Raw::Closed(frame.map(|frame| u16::from(frame.code))),
            None | Some(Err(_)) => Raw::Closed(None),
            Some(Ok(_)) => continue,
        });
    }
}

fn header_text(pattern: &str, id_key: &str, id: &str, protocol: u64, min_protocol: u64) -> String {
    json!({ "v": 1, "pattern": pattern, id_key: id, "protocol": protocol, "min_protocol": min_protocol }).to_string()
}

fn prologue(header: &str) -> Vec<u8> {
    let mut bytes = PROLOGUE.to_vec();
    bytes.extend_from_slice(header.as_bytes());
    bytes
}

fn noise_write(state: &mut snow::HandshakeState, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut message = vec![0u8; 65_535];
    let length = state.write_message(payload, &mut message)?;
    message.truncate(length);
    Ok(message)
}

struct Phone {
    id: String,
    name: String,
    private: Vec<u8>,
    public: Vec<u8>,
}

impl Phone {
    fn new(id: &str, name: &str) -> anyhow::Result<Self> {
        let pair = snow::Builder::new(KK.parse()?).generate_keypair()?;
        Ok(Self { id: id.to_string(), name: name.to_string(), private: pair.private, public: pair.public })
    }

    fn record(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "platform": "ios", "app": "0.4.0", "channel": CHANNEL })
    }
}

#[allow(clippy::large_enum_variant)]
enum Opened {
    Ready(Socket, TransportState, Vec<u8>),
    Closed(Option<u16>),
}

async fn pair_handshake(
    port: u16,
    route: &Route,
    phone: &Phone,
    pairing: &str,
    psk: &[u8; 32],
) -> anyhow::Result<Opened> {
    let mut socket =
        connect(port, route).await?.map_err(|status| anyhow::anyhow!("the upgrade was refused: {status}"))?;
    let header = header_text("XXpsk3", "pair", pairing, 2, 2);
    socket.send(Message::text(header.clone())).await?;
    let mut state = snow::Builder::new(XX.parse()?)
        .local_private_key(&phone.private)?
        .psk(3, psk)?
        .prologue(&prologue(&header))?
        .build_initiator()?;
    socket.send(Message::binary(noise_write(&mut state, &[])?)).await.ok();
    let second = match raw_next(&mut socket, QUICK).await? {
        Raw::Binary(bytes) => bytes,
        Raw::Closed(code) => return Ok(Opened::Closed(code)),
        Raw::Text(text) => bail!("a text message during the handshake: {text}"),
    };
    let mut scratch = vec![0u8; 65_535];
    state.read_message(&second, &mut scratch)?;
    let server = state.get_remote_static().context("the server sent no static key")?.to_vec();
    socket.send(Message::binary(noise_write(&mut state, phone.record().to_string().as_bytes())?)).await?;
    Ok(Opened::Ready(socket, state.into_transport_mode()?, server))
}

async fn kk_handshake(port: u16, route: &Route, device: &str, private: &[u8], server: &[u8]) -> anyhow::Result<Opened> {
    let mut socket =
        connect(port, route).await?.map_err(|status| anyhow::anyhow!("the upgrade was refused: {status}"))?;
    let header = header_text("KK", "device", device, 2, 2);
    socket.send(Message::text(header.clone())).await?;
    let mut state = snow::Builder::new(KK.parse()?)
        .local_private_key(private)?
        .remote_public_key(server)?
        .prologue(&prologue(&header))?
        .build_initiator()?;
    socket.send(Message::binary(noise_write(&mut state, &[])?)).await.ok();
    match raw_next(&mut socket, QUICK).await? {
        Raw::Binary(bytes) => {
            let mut scratch = vec![0u8; 65_535];
            state.read_message(&bytes, &mut scratch)?;
            Ok(Opened::Ready(socket, state.into_transport_mode()?, server.to_vec()))
        }
        Raw::Closed(code) => Ok(Opened::Closed(code)),
        Raw::Text(text) => bail!("a text message during the handshake: {text}"),
    }
}

fn records(kind: u8, payload: &[u8], room: usize) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = if payload.is_empty() { vec![payload] } else { payload.chunks(room).collect() };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(at, chunk)| {
            let mut record = vec![if kind == 1 && at != last { 2 } else { kind }];
            record.extend_from_slice(chunk);
            record
        })
        .collect()
}

struct Client {
    name: String,
    out: mpsc::UnboundedSender<Vec<u8>>,
    view: Arc<Mutex<View>>,
    changed: Arc<Notify>,
    next: u64,
}

type Sink = SplitSink<Socket, Message>;

fn seal(transport: &Mutex<TransportState>, record: &[u8]) -> Option<Vec<u8>> {
    let mut message = vec![0u8; record.len() + 16];
    let length = transport.lock().unwrap_or_else(PoisonError::into_inner).write_message(record, &mut message).ok()?;
    message.truncate(length);
    Some(message)
}

async fn reader(
    mut stream: SplitStream<Socket>,
    transport: Arc<Mutex<TransportState>>,
    view: Arc<Mutex<View>>,
    changed: Arc<Notify>,
) {
    let mut joined: Vec<u8> = Vec::new();
    while let Some(message) = stream.next().await {
        let mut view_now = view.lock().unwrap_or_else(PoisonError::into_inner);
        match message {
            Ok(Message::Binary(bytes)) => {
                let mut record = vec![0u8; bytes.len()];
                let opened = transport.lock().unwrap_or_else(PoisonError::into_inner).read_message(&bytes, &mut record);
                let Ok(length) = opened else {
                    view_now.broken.get_or_insert_with(|| "a record failed to decrypt".to_string());
                    break;
                };
                record.truncate(length);
                let outcome = match record.first() {
                    Some(1) => {
                        joined.extend_from_slice(&record[1..]);
                        let text = String::from_utf8(std::mem::take(&mut joined)).unwrap_or_default();
                        view_now.apply_frame(&text)
                    }
                    Some(2) => {
                        joined.extend_from_slice(&record[1..]);
                        Ok(())
                    }
                    Some(3) if record.len() >= 18 => {
                        let last = record[17] & 1 == 1;
                        let (head, data) = record.split_at(18);
                        view_now.chunk(&head[1..17], last, data)
                    }
                    other => Err(anyhow::anyhow!("an unknown record {other:?}")),
                };
                if let Err(error) = outcome {
                    view_now.broken.get_or_insert(format!("{error:#}"));
                }
            }
            Ok(Message::Text(text)) => {
                let frame: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if frame.get("op").and_then(Value::as_str) == Some("incompatible") {
                    view_now.incompatible = Some(frame);
                } else {
                    view_now.broken.get_or_insert(format!("a text message inside the channel: {text}"));
                }
            }
            Ok(Message::Close(frame)) => {
                view_now.close_code = frame.map(|frame| u16::from(frame.code));
                break;
            }
            Err(_) => break,
            Ok(_) => {}
        }
        drop(view_now);
        changed.notify_waiters();
    }
    view.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
    changed.notify_waiters();
}

async fn writer(mut sink: Sink, mut rx: mpsc::UnboundedReceiver<Vec<u8>>, transport: Arc<Mutex<TransportState>>) {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        let record = tokio::select! {
            record = rx.recv() => {
                let Some(record) = record else {
                    sink.close().await.ok();
                    return;
                };
                record
            }
            _ = ping.tick() => {
                let mut record = vec![1u8];
                record.extend_from_slice(json!({ "op": "ping" }).to_string().as_bytes());
                record
            }
        };
        let Some(message) = seal(&transport, &record) else { return };
        if sink.send(Message::binary(message)).await.is_err() {
            return;
        }
    }
}

impl Client {
    fn spawn(name: &str, socket: Socket, transport: TransportState) -> Self {
        let (sink, stream) = socket.split();
        let transport = Arc::new(Mutex::new(transport));
        let (out, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let view = Arc::new(Mutex::new(View::default()));
        let changed = Arc::new(Notify::new());
        tokio::spawn(reader(stream, Arc::clone(&transport), Arc::clone(&view), Arc::clone(&changed)));
        tokio::spawn(writer(sink, rx, transport));
        Self { name: name.to_string(), out, view, changed, next: 0 }
    }

    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn frame(&self, frame: &Value) {
        for record in records(1, frame.to_string().as_bytes(), 65_000) {
            self.out.send(record).ok();
        }
    }

    fn blob(&self, id: &[u8; 16], data: &[u8]) {
        let chunks: Vec<&[u8]> = if data.is_empty() { vec![data] } else { data.chunks(60_000).collect() };
        let last = chunks.len() - 1;
        for (at, chunk) in chunks.into_iter().enumerate() {
            let mut record = vec![3u8];
            record.extend_from_slice(id);
            record.push(u8::from(at == last));
            record.extend_from_slice(chunk);
            self.out.send(record).ok();
        }
    }

    async fn hello(&self, resume: Value) -> anyhow::Result<Duration> {
        let started = Instant::now();
        self.frame(&json!({ "op": "hello", "device": { "name": format!("{} (renamed)", self.name), "app": "0.4.1", "channel": CHANNEL }, "resume": resume }));
        self.until("hello answered", QUICK, |view| view.hello.is_some() || view.closed).await?;
        ensure!(
            self.view().hello.is_some(),
            "{}: closed with {:?} instead of hello",
            self.name,
            self.view().close_code
        );
        Ok(started.elapsed())
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

    async fn reply(&mut self, op: &str, args: Value) -> anyhow::Result<Value> {
        self.next += 1;
        let id = self.next;
        self.frame(&json!({ "id": id, "op": op, "args": args }));
        self.reply_to(id, op).await
    }

    async fn reply_to(&self, id: u64, op: &str) -> anyhow::Result<Value> {
        let deadline = Instant::now() + QUICK;
        loop {
            let notified = self.changed.notified();
            if let Some(reply) = self.view().replies.remove(&id) {
                return Ok(reply);
            }
            if self.view().closed {
                bail!("{}: the socket closed before a reply to {op}", self.name);
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("{}: no reply to {op}", self.name);
            };
            tokio::time::timeout(left.min(Duration::from_millis(200)), notified).await.ok();
        }
    }

    async fn call(&mut self, op: &str, args: Value) -> anyhow::Result<Value> {
        let reply = self.reply(op, args).await?;
        if let Some(error) = reply.get("error") {
            bail!("{}: {op} was refused: {error}", self.name);
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn refused(&mut self, op: &str, args: Value, code: &str) -> anyhow::Result<()> {
        let reply = self.reply(op, args).await?;
        let got = reply.pointer("/error/code").and_then(Value::as_str);
        ensure!(got == Some(code), "{}: {op} answered {reply}, expected error {code}", self.name);
        println!(
            "ok   {}: {op} is refused with {code}: {}",
            self.name,
            reply.pointer("/error/message").unwrap_or(&Value::Null)
        );
        Ok(())
    }

    async fn answered(&mut self, op: &str, args: Value, request: &str) -> anyhow::Result<String> {
        self.call(op, args).await?;
        self.until(&format!("{op} answered over the wire"), SETUP, |view| view.answers.contains_key(request)).await?;
        Ok(self.view().answers[request].clone())
    }

    async fn upload(&mut self, data: &[u8], mime: &str) -> anyhow::Result<String> {
        let id = rand_bytes::<16>()?;
        self.next += 1;
        let ask = self.next;
        self.frame(
            &json!({ "id": ask, "op": "blob_put", "args": { "blob": hex(&id), "size": data.len(), "mime": mime } }),
        );
        self.blob(&id, data);
        let reply = self.reply_to(ask, "blob_put").await?;
        ensure!(reply.get("ok") == Some(&json!(true)), "{}: the upload was not acknowledged: {reply}", self.name);
        Ok(hex(&id))
    }

    async fn closed_with(&self, step: &str, code: u16) -> anyhow::Result<()> {
        self.until(step, QUICK, |view| view.closed).await?;
        let got = self.view().close_code;
        ensure!(got == Some(code), "{}: closed with {got:?}, expected {code}", self.name);
        Ok(())
    }
}

fn rand_bytes<const N: usize>() -> anyhow::Result<[u8; N]> {
    let pair = snow::Builder::new(KK.parse()?).generate_keypair()?;
    let mut bytes = [0u8; N];
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = pair.private[at % pair.private.len()] ^ u8::try_from(at).unwrap_or_default();
    }
    Ok(bytes)
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

fn isolate(command: &mut Command, root: &Path, data: &Path) {
    command
        .env("HOME", root.join("home"))
        .env("EUKHE_DAEMON_SOCKET", root.join("d.sock"))
        .env("EUKHE_CODING_AGENT_DIR", root.join("agent"))
        .env("LOCAL_STUDIO_DATA_DIR", data);
}

impl Serve {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        isolate(&mut command, &self.root, &self.data);
        command
    }

    fn start(&mut self, eukhe: &Path, hook: &str) -> anyhow::Result<()> {
        let log = std::fs::File::create(self.root.join("serve.log"))?;
        let child = self
            .command()
            .args(["serve", "--port", &self.port.to_string(), "--name", "smoke-box", "--tunnel-host", TUNNEL])
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

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("serve.log")).unwrap_or_default()
    }

    fn log_tail(&self) -> String {
        let log = self.log();
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(25)..].join("\n")
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

    fn pairing(&self) -> anyhow::Result<(String, [u8; 32])> {
        let url = format!("ws://127.0.0.1:{}/v1/ws", self.port);
        let printed = self.run(&["pair", "--url", &url, "--name", "smoke-box"])?;
        let link = printed
            .lines()
            .find(|line| line.starts_with(&format!("{}://pair?", scheme())))
            .context("pair printed no link")?;
        ensure!(printed.contains('\u{2588}') || printed.contains('\u{2580}'), "pair printed no QR code");
        let (query, secret) = link.split_once('#').context("the pairing link has no secret")?;
        let id = query
            .split(['?', '&'])
            .find_map(|part| part.strip_prefix("id="))
            .context("the pairing link has no id")?
            .to_string();
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(secret)?;
        let secret = <[u8; 32]>::try_from(secret).map_err(|_| anyhow::anyhow!("the pairing secret is not 32 bytes"))?;
        ensure!(id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit()), "pairing id {id}");
        Ok((id, secret))
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

async fn http_get(port: u16, host: &str, path: &str, extra: &str) -> anyhow::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
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

fn desktop(
    eukhe: &Path,
    data: &Path,
    step: &str,
    done: impl FnMut(&Studio, Update) -> anyhow::Result<bool>,
    start: impl FnOnce(&Studio),
) -> anyhow::Result<Option<u32>> {
    tokio::task::block_in_place(|| desktop_run(eukhe, data, step, done, start))
}

fn desktop_run(
    eukhe: &Path,
    data: &Path,
    step: &str,
    mut done: impl FnMut(&Studio, Update) -> anyhow::Result<bool>,
    start: impl FnOnce(&Studio),
) -> anyhow::Result<Option<u32>> {
    let signal = Arc::new(Signal::default());
    let config = StudioConfig {
        socket_path: None,
        session_dir: None,
        data_dir: Some(data.to_string_lossy().into_owned()),
        launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
        controller_main: None,
    };
    let studio =
        Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(|error| anyhow::anyhow!("{error}"))?;
    start(&studio);
    let deadline = Instant::now() + SETUP;
    let mut pid = None;
    let mut finished = false;
    while !finished {
        for update in studio.tick() {
            if let Update::Daemon { pid: Some(known), .. } = &update {
                pid = Some(*known);
            }
            finished = finished || done(&studio, update)?;
        }
        if finished {
            break;
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            studio.shutdown();
            bail!("the desktop side timed out: {step}");
        };
        let pending = signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = signal
            .ready
            .wait_timeout_while(pending, left.min(Duration::from_millis(500)), |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
    println!("ok   desktop: {step}");
    studio.shutdown();
    drop(studio);
    Ok(pid)
}

fn git(work: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(work)
        .env("HOME", work.join("../home"))
        .stdout(Stdio::null())
        .status()?;
    ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

fn seed_work(work: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(work)?;
    let big = (0..6000).fold(String::new(), |mut big, line| {
        writeln!(big, "line {line:05} of a file long enough to need several blob records").ok();
        big
    });
    std::fs::write(work.join("notes.txt"), "remote smoke notes\n")?;
    std::fs::write(work.join("big.txt"), &big)?;
    git(work, &["init", "-q", "-b", "main"])?;
    git(work, &["add", "."])?;
    git(work, &["commit", "-q", "-m", "seed"])?;
    std::fs::write(work.join("notes.txt"), "remote smoke notes\nchanged on the box\n")?;
    Ok(big)
}

struct Target {
    provider: String,
    model: String,
    thinking: String,
}

fn agent_dir(root: &Path) -> anyhow::Result<Option<Target>> {
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent)?;
    let write = |name: &str, data: &[u8]| -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(agent.join(name))?
            .write_all(data)?;
        Ok(())
    };
    write("auth.json", b"{}")?;
    let Some(source) = std::env::var_os("LOCAL_STUDIO_SMOKE_AGENT").map(PathBuf::from) else {
        write("models.json", br#"{"providers":{}}"#)?;
        write("settings.json", br#"{"onboardingShown":true}"#)?;
        return Ok(None);
    };
    let models = std::fs::read(source.join("models.json"))
        .with_context(|| format!("read {}", source.join("models.json").display()))?;
    write("models.json", &models)?;
    write(
        "settings.json",
        &std::fs::read(source.join("settings.json")).unwrap_or_else(|_| br#"{"onboardingShown":true}"#.to_vec()),
    )?;
    let chosen = std::env::var("REMOTE_SMOKE_MODEL").ok();
    let catalog: Value = serde_json::from_slice(&models)?;
    let (provider, model) = if let Some((provider, model)) = chosen.as_deref().and_then(|chosen| chosen.split_once('/'))
    {
        (provider.to_string(), model.to_string())
    } else {
        let (provider, entry) = catalog
            .get("providers")
            .and_then(Value::as_object)
            .and_then(|providers| providers.iter().next())
            .context("models.json names no provider")?;
        let model = entry.pointer("/models/0/id").and_then(Value::as_str).context("the provider names no model")?;
        (provider.clone(), model.to_string())
    };
    let base = catalog.pointer(&format!("/providers/{provider}/baseUrl")).and_then(Value::as_str).unwrap_or("?");
    let thinking = std::env::var("REMOTE_SMOKE_THINKING").unwrap_or_else(|_| "off".to_string());
    println!("     model {provider}/{model} at {base}, thinking {thinking}; auth.json is a scratch {{}} (no sign-in)");
    Ok(Some(Target { provider, model, thinking }))
}

struct Run {
    serve: Serve,
    work: PathBuf,
    project: String,
    big: String,
    hook: String,
    posts: Arc<Mutex<Vec<Value>>>,
    server_key: Vec<u8>,
    epoch: String,
    report: Vec<String>,
}

async fn expect_closed(opened: Opened, step: &str, code: u16) -> anyhow::Result<()> {
    match opened {
        Opened::Closed(got) => {
            ensure!(got == Some(code), "{step}: closed with {got:?}, expected {code}");
        }
        Opened::Ready(socket, transport, _) => {
            let client = Client::spawn(step, socket, transport);
            client.frame(&json!({ "op": "hello", "device": {}, "resume": {} }));
            client.until("the server closes", QUICK, |view| view.closed || view.hello.is_some()).await?;
            let view = client.view();
            ensure!(view.hello.is_none(), "{step}: the server answered hello");
            ensure!(view.close_code == Some(code), "{step}: closed with {:?}, expected {code}", view.close_code);
        }
    }
    println!("ok   {step}: closed with {code}");
    Ok(())
}

async fn transport_checks(run: &mut Run, eukhe: &Path) -> anyhow::Result<()> {
    let port = run.serve.port;
    let host = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + SETUP;
    let health = loop {
        if let Ok(answer) = http_get(port, &host, "/v1/health", "").await {
            break answer;
        }
        if run.serve.exited()? || Instant::now() >= deadline {
            bail!("serve did not come up:\n{}", run.serve.log_tail());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    ensure!(
        health.0 == 200 && health.1.contains("\"protocol\":2") && health.1.contains("\"min_protocol\":2"),
        "health answered {health:?}"
    );
    println!("ok   /v1/health answers {} without auth", health.1);
    for (path, host, extra, why) in [
        ("/v1/health", "evil.example".to_string(), String::new(), "a foreign Host"),
        ("/v1/health", format!("127.0.0.1:{}", port + 1), String::new(), "a loopback Host on another port"),
        (
            "/v1/health",
            "evil.example".to_string(),
            format!("X-Forwarded-Host: 127.0.0.1:{port}\r\n"),
            "a foreign Host with an allowed X-Forwarded-Host",
        ),
        ("/v1/ws", "evil.example".to_string(), String::new(), "a socket for a foreign Host"),
        ("/nope", "evil.example".to_string(), String::new(), "any path on a foreign Host"),
    ] {
        let answer = http_get(port, &host, path, &extra).await?;
        ensure!(answer.0 == 403, "{why} got {}", answer.0);
        println!("ok   {why} gets 403 on {path}");
    }
    let tunnel = http_get(port, TUNNEL, "/v1/health", "").await?;
    ensure!(tunnel.0 == 200, "the tunnel host got {}", tunnel.0);
    println!("ok   the tunnel host {TUNNEL} is allowed");
    let cross = connect(port, &Route { origin: Some("https://evil.example".to_string()), ..Route::default() }).await?;
    ensure!(matches!(cross, Err(403)), "a socket with a mismatched Origin was not refused");
    println!("ok   a socket with a mismatched Origin gets 403");

    let rival_port = free_port()?;
    let mut rival = run.serve.command();
    let mut rival = rival
        .args(["serve", "--port", &rival_port.to_string(), "--eukhe"])
        .arg(eukhe)
        .arg("--data-dir")
        .arg(&run.serve.data)
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
    println!("ok   a second serve on the data dir is refused: {}", said.trim());

    for (protocol, min_protocol) in [(3, 3), (1, 1)] {
        let mut socket =
            connect(port, &Route::default()).await?.map_err(|status| anyhow::anyhow!("refused {status}"))?;
        socket.send(Message::text(header_text("KK", "device", "phone-x", protocol, min_protocol))).await?;
        let Raw::Text(text) = raw_next(&mut socket, QUICK).await? else {
            bail!("no incompatible frame for protocol {protocol}")
        };
        let answer: Value = serde_json::from_str(&text)?;
        ensure!(
            answer == json!({ "op": "incompatible", "protocol": 2, "min_protocol": 2 }),
            "incompatible answered {answer}"
        );
        let Raw::Closed(_) = raw_next(&mut socket, QUICK).await? else { bail!("no close after incompatible") };
        println!(
            "ok   a phone on protocols {min_protocol} to {protocol} gets {text} before any handshake, then a close"
        );
    }
    let mut socket =
        connect(port, &Route::tunnel("198.51.100.10")).await?.map_err(|status| anyhow::anyhow!("refused {status}"))?;
    socket.send(Message::text("{\"v\":1,\"pattern\":\"NN\"}")).await?;
    let Raw::Closed(code) = raw_next(&mut socket, QUICK).await? else { bail!("a bad header was not closed") };
    ensure!(code == Some(1002), "a bad header closed with {code:?}");
    println!("ok   a bad opening header is closed with 1002");
    Ok(())
}

async fn pair(run: &mut Run, phone: &Phone) -> anyhow::Result<Client> {
    let (id, psk) = run.serve.pairing()?;
    let Opened::Ready(socket, transport, server) =
        pair_handshake(run.serve.port, &Route::default(), phone, &id, &psk).await?
    else {
        bail!("pairing {} was refused:\n{}", phone.id, run.serve.log_tail());
    };
    ensure!(run.server_key.is_empty() || run.server_key == server, "the machine's static key changed");
    run.server_key = server;
    let client = Client::spawn(&phone.name, socket, transport);
    client.hello(json!({ "watch": [] })).await?;
    Ok(client)
}

async fn reconnect(run: &Run, phone: &Phone, resume: Value) -> anyhow::Result<(Client, Duration)> {
    let started = Instant::now();
    let opened = kk_handshake(run.serve.port, &Route::default(), &phone.id, &phone.private, &run.server_key).await?;
    let Opened::Ready(socket, transport, _) = opened else {
        bail!("{}: KK was refused:\n{}", phone.id, run.serve.log_tail())
    };
    let client = Client::spawn(&phone.name, socket, transport);
    client.hello(resume).await?;
    Ok((client, started.elapsed()))
}

#[allow(clippy::too_many_lines)]
async fn pairing_checks(run: &mut Run, a: &Phone) -> anyhow::Result<Client> {
    let port = run.serve.port;
    let (stale, stale_psk) = run.serve.pairing()?;
    let (id, psk) = run.serve.pairing()?;
    let opened = pair_handshake(port, &Route::tunnel("198.51.100.11"), a, &stale, &stale_psk).await?;
    expect_closed(opened, "a pairing cancelled by a newer one is refused", 4401).await?;
    let mut wrong = psk;
    wrong[0] ^= 0xff;
    let opened = pair_handshake(port, &Route::tunnel("198.51.100.12"), a, &id, &wrong).await?;
    expect_closed(opened, "a wrong pairing secret fails at the third message", 4401).await?;
    let started = Instant::now();
    let Opened::Ready(socket, transport, server) = pair_handshake(port, &Route::default(), a, &id, &psk).await? else {
        bail!("the right secret was refused:\n{}", run.serve.log_tail());
    };
    run.server_key = server;
    let client = Client::spawn(&a.name, socket, transport);
    client.hello(json!({ "watch": [] })).await?;
    println!("ok   phone A paired through XXpsk3 and got hello in {} ms", started.elapsed().as_millis());
    {
        let view = client.view();
        let hello = view.hello.as_ref().context("no hello")?;
        let kinds: Vec<&str> =
            hello["snapshot"].as_array().into_iter().flatten().filter_map(|update| update["type"].as_str()).collect();
        ensure!(
            hello["protocol"] == json!(2)
                && hello["resume"] == json!("snapshot")
                && hello.pointer("/machine/version") == Some(&json!(VERSION))
                && hello.pointer("/machine/channel") == Some(&json!(CHANNEL))
                && hello.pointer("/machine/urls") == Some(&json!([format!("wss://{TUNNEL}/v1/ws")]))
                && hello["epoch"].as_str().is_some_and(|epoch| !epoch.is_empty()),
            "hello: {hello}"
        );
        ensure!(
            kinds.contains(&"connection") && kinds.contains(&"projects") && kinds.contains(&"threads"),
            "snapshot kinds {kinds:?}"
        );
        run.epoch = hello["epoch"].as_str().unwrap_or_default().to_string();
        println!("ok   hello: protocol 2, epoch {}, machine {}, snapshot {kinds:?}", run.epoch, hello["machine"]);
        let missing: Vec<&str> = ["models", "theme"].into_iter().filter(|kind| !kinds.contains(kind)).collect();
        if !missing.is_empty() {
            println!(
                "     snapshot lacks {missing:?} (models arrive as an update once loaded; theme is not built in core)"
            );
        }
    }
    let remote = run.serve.data.join("remote");
    let mode = |name: &str| -> anyhow::Result<u32> {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::metadata(remote.join(name))?.permissions().mode() & 0o777)
    };
    ensure!(mode("static.key")? == 0o600 && mode("devices.json")? == 0o600, "remote files are not 0600");
    ensure!(!remote.join("pairing.json").exists(), "the used pairing was not burned");
    let ledger: Value = serde_json::from_slice(&std::fs::read(remote.join("devices.json"))?)?;
    let device = ledger["devices"]
        .as_array()
        .and_then(|devices| devices.iter().find(|device| device["id"] == json!(a.id)))
        .context("phone A is not in devices.json")?;
    let fields: HashSet<&str> =
        device.as_object().map(|fields| fields.keys().map(String::as_str).collect()).unwrap_or_default();
    ensure!(device["key"] == json!(hex(&a.public)), "devices.json holds the wrong key for phone A");
    ensure!(
        fields == HashSet::from(["id", "name", "platform", "app", "channel", "key", "firstSeen", "lastSeen"]),
        "devices.json fields {fields:?}"
    );
    ensure!(
        device["name"] == json!(format!("{} (renamed)", a.name)) && device["app"] == json!("0.4.1"),
        "hello did not update the record: {device}"
    );
    println!("ok   static.key and devices.json are 0600; devices.json holds phone A's public key and no secret; pairing.json is gone");
    let opened = pair_handshake(port, &Route::tunnel("198.51.100.13"), a, &id, &psk).await?;
    expect_closed(opened, "a burned pairing secret is refused", 4401).await?;
    let opened =
        kk_handshake(port, &Route::tunnel("198.51.100.14"), "phone-nobody", &a.private, &run.server_key).await?;
    expect_closed(opened, "KK for an unknown device is refused", 4401).await?;
    let stranger = Phone::new(&a.id, "Stranger")?;
    let opened = kk_handshake(port, &Route::tunnel("198.51.100.15"), &a.id, &stranger.private, &run.server_key).await?;
    expect_closed(opened, "KK with another key for a known device id fails the handshake", 4401).await?;
    Ok(client)
}

#[allow(clippy::too_many_lines)]
async fn op_checks(run: &mut Run, a: &mut Client) -> anyhow::Result<()> {
    a.until("daemon connected", SETUP, |view| matches!(view.connection, Some(Connection::Ready))).await?;
    a.until("the desktop's project is in the snapshot", QUICK, |view| view.projects.len() == 1).await?;
    run.project.clone_from(&a.view().projects[0]);
    let project = run.project.clone();
    a.refused("add_project", json!({ "path": "/tmp" }), "not_remote").await?;
    a.refused("load_usage", json!({ "request": "u", "period": "day" }), "not_remote").await?;
    a.refused("delete_thread", json!({ "thread": "x" }), "not_remote").await?;
    a.refused("answer_questions", json!({ "thread": "x", "request": "q", "answers": {} }), "not_built").await?;
    a.refused("rename_thread", json!({ "thread": 7 }), "bad_args").await?;
    a.refused("hello", json!({}), "bad_state").await?;
    a.refused("load_earlier", json!({ "thread": "nope" }), "not_watched").await?;
    a.refused("blob_put", json!({ "blob": hex(&[1u8; 16]), "size": 26 << 20, "mime": "image/png" }), "too_large")
        .await?;
    a.frame(&json!({ "op": "ping" }));
    a.until("ping answered with pong", QUICK, |view| view.pongs >= 1).await?;
    let models = a.view().models;
    a.call("load_models", json!({})).await?;
    a.until("load_models brings a models update", SETUP, |view| view.models > models).await?;
    for (op, args, request) in [
        ("list_commands", json!({ "request": "q-commands", "project": project }), "q-commands"),
        ("list_skills", json!({ "request": "q-skills", "project": project }), "q-skills"),
        ("search_threads", json!({ "request": "q-search", "query": "smoke", "limit": 10 }), "q-search"),
        ("list_archived", json!({ "request": "q-archived" }), "q-archived"),
        (
            "load_diff",
            json!({ "request": "q-diff", "project": project, "scope": { "type": "working_tree" } }),
            "q-diff",
        ),
        ("list_pull_requests", json!({ "request": "q-prs", "project": project }), "q-prs"),
        ("load_pull_request", json!({ "request": "q-pr", "project": project, "number": 1 }), "q-pr"),
    ] {
        let kind = a.answered(op, args, request).await?;
        println!("     {op} answered with {kind}");
    }
    ensure!(a.view().answers["q-diff"] == "diff", "load_diff did not answer with a diff");
    let git = a.view().git_status;
    a.call("git_status", json!({ "project": project })).await?;
    a.until("git_status answers with git_status", SETUP, |view| view.git_status > git).await?;
    a.call("read_file", json!({ "request": "q-file", "project": project, "relative": "big.txt" })).await?;
    a.until("read_file answers with a blob and all its chunks", SETUP, |view| {
        view.blobs.values().any(|blob| blob.request == "q-file" && blob.done)
    })
    .await?;
    {
        let view = a.view();
        let blob = view.blobs.values().find(|blob| blob.request == "q-file").context("no file blob")?;
        ensure!(blob.data == run.big.as_bytes() && blob.size == run.big.len(), "the file blob differs from big.txt");
        ensure!(
            blob.mime.starts_with("text/plain")
                && blob.extra["path"].as_str().is_some_and(|path| path.ends_with("big.txt")),
            "file blob header {}",
            blob.extra
        );
        println!(
            "ok   read_file sent big.txt ({} bytes, {} records) as a blob, byte for byte",
            blob.size,
            blob.size.div_ceil(65_499)
        );
    }
    let image = png(64);
    let id = a.upload(&image, "image/png").await?;
    println!("ok   blob_put of a {} byte PNG is acknowledged after its final chunk ({id})", image.len());
    let short = rand_bytes::<16>()?;
    a.next += 1;
    let ask = a.next;
    a.frame(
        &json!({ "id": ask, "op": "blob_put", "args": { "blob": hex(&short), "size": 1000, "mime": "image/png" } }),
    );
    a.blob(&short, &[0u8; 10]);
    let reply = a.reply_to(ask, "blob_put").await?;
    ensure!(reply.pointer("/error/code") == Some(&json!("bad_blob")), "a short blob answered {reply}");
    println!("ok   a blob that ends short of its declared size is refused with bad_blob");
    a.refused("send", json!({ "request": "r-x", "thread": "nope", "prompt": { "text": "x", "images": [hex(&[9u8; 16])], "files": [] }, "delivery": "queue" }), "bad_blob").await?;
    run.report.push(format!("blob upload id {id}"));
    Ok(())
}

async fn resume_checks(run: &mut Run, a: Client, a_phone: &Phone, c: &mut Client) -> anyhow::Result<Client> {
    let seq = a.view().seq;
    let gaps = a.view().seq_gaps;
    ensure!(gaps == 0, "phone A saw {gaps} seq gaps");
    drop(a);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let models = c.view().models;
    c.call("load_models", json!({})).await?;
    c.until("phone C's load_models brings a models update", SETUP, |view| view.models > models).await?;
    let (a, latency) = reconnect(run, a_phone, json!({ "seq": seq, "epoch": run.epoch, "watch": [] })).await?;
    a.until("the missed models update is replayed after hello", QUICK, |view| view.models >= 1).await?;
    {
        let view = a.view();
        let hello = view.hello.as_ref().context("no hello")?;
        ensure!(
            hello["resume"] == json!("delta") && hello["seq"] == json!(seq) && hello["snapshot"] == json!([]),
            "resume hello {hello}"
        );
        ensure!(view.seq_gaps == 0 && view.seq > seq, "the replay skipped a seq (from {seq} to {})", view.seq);
        println!(
            "ok   KK reconnect resumed by seq: delta from {seq} to {}, hello in {} ms",
            view.seq,
            latency.as_millis()
        );
    }
    run.report.push(format!("KK reconnect to hello latency {} ms", latency.as_millis()));
    drop(a);
    let (a, _) = reconnect(run, a_phone, json!({ "seq": 3, "epoch": "not-this-epoch", "watch": [] })).await?;
    ensure!(
        a.view().hello.as_ref().is_some_and(|hello| hello["resume"] == json!("snapshot")),
        "a foreign epoch did not get a snapshot"
    );
    println!("ok   a resume from another epoch gets a full snapshot");
    Ok(a)
}

async fn limit_checks(run: &Run, a: Client, a_phone: &Phone) -> anyhow::Result<Client> {
    let port = run.serve.port;
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(
            connect(port, &Route::tunnel("192.0.2.50")).await?.map_err(|status| anyhow::anyhow!("refused {status}"))?,
        );
    }
    let mut ninth =
        connect(port, &Route::tunnel("192.0.2.51")).await?.map_err(|status| anyhow::anyhow!("refused {status}"))?;
    let Raw::Closed(code) = raw_next(&mut ninth, QUICK).await? else { bail!("a ninth opening socket was not closed") };
    ensure!(code == Some(1013), "a ninth opening socket closed with {code:?}");
    println!("ok   a ninth socket mid-handshake is closed with 1013 while 8 are open");
    let started = Instant::now();
    let Raw::Closed(code) = raw_next(&mut held[0], QUICK).await? else { bail!("a silent socket was not closed") };
    ensure!(
        code == Some(1002) && started.elapsed() < Duration::from_secs(7),
        "a silent socket closed with {code:?} after {:?}",
        started.elapsed()
    );
    println!("ok   a socket with no opening header is closed after 5 s");
    drop(held);
    for attempt in 1..=5 {
        let mut socket = connect(port, &Route::tunnel("203.0.113.9"))
            .await?
            .map_err(|status| anyhow::anyhow!("refused {status}"))?;
        socket.send(Message::text(format!("not a header {attempt}"))).await?;
        let Raw::Closed(code) = raw_next(&mut socket, QUICK).await? else {
            bail!("bad header {attempt} was not closed")
        };
        ensure!(code == Some(1002), "bad header {attempt} closed with {code:?}");
    }
    let mut socket =
        connect(port, &Route::tunnel("203.0.113.9")).await?.map_err(|status| anyhow::anyhow!("refused {status}"))?;
    let Raw::Closed(code) = raw_next(&mut socket, QUICK).await? else { bail!("a banned address was not closed") };
    ensure!(code == Some(4429), "a banned address closed with {code:?}");
    println!("ok   5 bad headers from 203.0.113.9 within a minute refuse it with 4429");
    let spoof = Route { from: Some("203.0.113.9".to_string()), ..Route::default() };
    let opened = kk_handshake(port, &spoof, &a_phone.id, &a_phone.private, &run.server_key).await?;
    let Opened::Ready(socket, transport, _) = opened else { bail!("CF-Connecting-IP on a loopback Host was honored") };
    let probe = Client::spawn(&a_phone.name, socket, transport);
    probe.hello(json!({ "watch": [] })).await?;
    println!("ok   CF-Connecting-IP counts only for the tunnel host: a loopback socket naming the banned address still connects");
    a.closed_with("the older socket of phone A is replaced with 4000", 4000).await?;
    let log = run.serve.log();
    ensure!(
        log.contains("refused 203.0.113.9 (-)") && log.contains("refused 198.51.100.12 (XXpsk3)"),
        "the failures are not logged with address and pattern"
    );
    println!("ok   each refusal is logged with its address and pattern");
    Ok(probe)
}

async fn revoke_one(run: &mut Run) -> anyhow::Result<()> {
    let b_phone = Phone::new("phone-b", "Smoke Phone B")?;
    let b = pair(run, &b_phone).await?;
    let started = Instant::now();
    run.serve.run(&["revoke", "phone-b"])?;
    b.closed_with("a revoked device's open socket closes with 4401", 4401).await?;
    println!("     revoke to close {} ms", started.elapsed().as_millis());
    let opened =
        kk_handshake(run.serve.port, &Route::tunnel("198.51.100.16"), &b_phone.id, &b_phone.private, &run.server_key)
            .await?;
    expect_closed(opened, "a revoked device's KK is refused", 4401).await
}

#[allow(clippy::too_many_lines)]
async fn model_stage(run: &mut Run, model: &Target, a: &mut Client, c: &Client) -> anyhow::Result<()> {
    let settings = serde_json::to_value(ThreadSettings {
        provider: Some(model.provider.clone()),
        model: Some(model.model.clone()),
        thinking_level: Some(model.thinking.clone()),
        access: AccessMode::FullAccess,
        worktree: WorktreeChoice::Local,
    })?;
    let image = png(64);
    let blob = a.upload(&image, "image/png").await?;
    let text = bash_prompt("printf remote-ok > remote-proof.txt");
    let started = Instant::now();
    a.call("start_thread", json!({ "draft": "d-tool", "project": run.project, "prompt": { "text": text, "images": [blob], "files": [] }, "settings": settings })).await?;
    a.until("thread_started", MODEL_STEP, |view| view.started.contains_key("d-tool")).await?;
    let thread = a.view().started["d-tool"].clone();
    a.call("watch", json!({ "thread": thread })).await?;
    a.until("a streamed turn with a tool call and a reply", MODEL_STEP, |view| {
        view.streamed(&thread, |kind| matches!(kind, ItemKind::Tool { .. }))
            && view.streamed(&thread, |kind| matches!(kind, ItemKind::Assistant { .. }))
            && view.replies_done(&thread) >= 1
            && view.idle(&thread)
    })
    .await?;
    let proof = std::fs::read_to_string(run.work.join("remote-proof.txt"))
        .context("the tool call wrote no remote-proof.txt")?;
    ensure!(proof.contains("remote-ok"), "remote-proof.txt holds {proof:?}");
    println!(
        "PASS model stage: a streamed turn with a real tool call over the wire\nok   the real bash call wrote remote-proof.txt ({:.0} s for the first turn)",
        started.elapsed().as_secs_f64()
    );
    let entry = a.view().items(&thread).iter().find_map(|item| match &item.kind {
        ItemKind::User { images, entry: Some(entry), .. } if *images > 0 => Some(entry.clone()),
        _ => None,
    });
    let entry = entry.context("the user item carries no image")?;
    a.call("load_message_image", json!({ "request": "q-image", "thread": thread, "entry": entry, "index": 0 })).await?;
    a.until("load_message_image answers with a blob", MODEL_STEP, |view| {
        view.blobs.values().any(|blob| blob.request == "q-image" && blob.done)
    })
    .await?;
    {
        let view = a.view();
        let got = view.blobs.values().find(|blob| blob.request == "q-image").context("no image blob")?;
        ensure!(
            got.mime.starts_with("image/") && got.data.len() == got.size && got.size > 0,
            "image blob {} {}",
            got.mime,
            got.size
        );
        println!("ok   the uploaded image went into the turn and came back as a {} byte {} blob", got.size, got.mime);
    }

    let replies = a.view().replies_done(&thread);
    let frames = a.view().transcript_frames.get(&thread).copied().unwrap_or_default();
    let updates = a.view().thread_updates.get(&thread).copied().unwrap_or_default();
    a.view().first_text_at.remove(&thread);
    let sent = Instant::now();
    let args = json!({ "request": "r-once", "thread": thread, "prompt": prompt(ONCE_TEXT), "delivery": "queue" });
    a.call("send", args.clone()).await?;
    let retry = a.call("send", args).await?;
    ensure!(retry.get("duplicate") == Some(&json!(true)), "the retried send was not dropped: {retry}");
    a.until("first update for the thread after send", MODEL_STEP, |view| {
        view.thread_updates.get(&thread).copied().unwrap_or_default() > updates
    })
    .await?;
    let first_thread_update = sent.elapsed();
    a.until("first transcript update after send", MODEL_STEP, |view| {
        view.transcript_frames.get(&thread).copied().unwrap_or_default() > frames
    })
    .await?;
    let first_update = sent.elapsed();
    a.until("the once turn finished", MODEL_STEP, |view| view.replies_done(&thread) > replies && view.idle(&thread))
        .await?;
    let first_text = a.view().first_text_at.get(&thread).map(|at| at.duration_since(sent));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let once = a.view().users_saying(&thread, ONCE_TEXT);
    ensure!(once == 1, "the retried send ran {once} times");
    println!("ok   a retried send with the same request id ran once");
    run.report.push(format!("phone: send to first update for the thread {} ms", first_thread_update.as_millis()));
    run.report.push(format!("phone: send to first transcript update {} ms", first_update.as_millis()));
    for line in run.serve.log().lines().filter(|line| line.contains("send r-once ")) {
        run.report.push(format!("server: {}", line.trim_start_matches("local-studio-serve: ")));
    }
    if let Some(first_text) = first_text {
        run.report
            .push(format!("phone: send to first streamed assistant text {} ms (model time)", first_text.as_millis()));
    }

    a.call("rename_thread", json!({ "thread": thread, "title": "Remote smoke thread" })).await?;
    a.until("rename_thread shows in the row", QUICK, |view| {
        view.row(&thread).is_some_and(|row| row.title == "Remote smoke thread")
    })
    .await?;
    a.call("set_pinned", json!({ "thread": thread, "pinned": true })).await?;
    a.until("set_pinned shows in the row", QUICK, |view| view.row(&thread).is_some_and(|row| row.pinned)).await?;
    a.call("set_pinned", json!({ "thread": thread, "pinned": false })).await?;
    a.call("mark_unread", json!({ "thread": thread, "unread": true })).await?;
    a.until("mark_unread shows in the row", QUICK, |view| view.row(&thread).is_some_and(|row| row.unread)).await?;
    let until =
        i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis())? + 3_600_000;
    a.call("snooze", json!({ "thread": thread, "until": until })).await?;
    a.until("snooze shows in the row", QUICK, |view| view.row(&thread).is_some_and(|row| row.snoozed_until.is_some()))
        .await?;
    a.call("snooze", json!({ "thread": thread, "until": null })).await?;
    a.call("set_settled", json!({ "thread": thread, "settled": true })).await?;
    a.call("set_settled", json!({ "thread": thread, "settled": false })).await?;
    a.call("set_archived", json!({ "thread": thread, "archived": true })).await?;
    a.until("set_archived shows in the row", QUICK, |view| view.row(&thread).is_none_or(|row| row.archived)).await?;
    a.call("set_archived", json!({ "thread": thread, "archived": false })).await?;
    a.until("the thread is back from the archive", QUICK, |view| view.row(&thread).is_some_and(|row| !row.archived))
        .await?;
    a.call("set_model", json!({ "thread": thread, "provider": model.provider, "model": model.model })).await?;
    a.call("set_thinking_level", json!({ "thread": thread, "level": model.thinking })).await?;
    a.call("set_access_mode", json!({ "thread": thread, "mode": "supervised" })).await?;
    a.until("set_access_mode shows on the thread", QUICK, |view| {
        view.access.get(&thread) == Some(&AccessMode::Supervised)
    })
    .await?;
    a.call("set_access_mode", json!({ "thread": thread, "mode": "fullAccess" })).await?;
    a.until("full access again", QUICK, |view| view.access.get(&thread) == Some(&AccessMode::FullAccess)).await?;
    for (op, args) in [
        ("answer_approval", json!({ "thread": thread, "request": "no-such-approval", "answer": "deny" })),
        ("send_queued_now", json!({ "thread": thread, "queued": "no-such-message" })),
        ("remove_queued", json!({ "thread": thread, "queued": "no-such-message" })),
        ("cancel_subagent", json!({ "thread": thread, "child": "no-such-child" })),
    ] {
        a.call(op, args).await?;
        println!("ok   {op} is accepted over the wire");
    }
    let kind = a.answered("load_diff", json!({ "request": "q-turn-diff", "project": run.project, "scope": { "type": "latest_turn", "thread": thread } }), "q-turn-diff").await?;
    println!("     load_diff for the latest turn answered with {kind}");
    let rows = a.view().threads.len();
    a.call("fork_thread", json!({ "thread": thread })).await?;
    a.until("fork_thread adds a row", MODEL_STEP, |view| view.threads.len() > rows).await?;
    a.until("the thread is idle before the abort check", MODEL_STEP, |view| view.idle(&thread)).await?;
    let tools = a.view().tools(&thread);

    a.call("send", json!({ "request": "r-sleep", "thread": thread, "prompt": prompt(&bash_prompt(SLEEP_COMMAND)), "delivery": "queue" })).await?;
    a.until("the sleep turn is running its bash call", MODEL_STEP, |view| {
        view.working(&thread) && view.tools(&thread) > tools
    })
    .await?;
    a.call("abort", json!({ "thread": thread })).await?;
    a.until("abort: the thread leaves Working within 20 s", ABORT_BOUND, |view| view.idle(&thread)).await?;
    a.call("regenerate_title", json!({ "thread": thread })).await?;
    println!("ok   regenerate_title is accepted over the wire");
    a.call("compact", json!({ "thread": thread })).await?;
    println!("ok   compact is accepted over the wire");
    a.call("unwatch", json!({ "thread": thread })).await?;

    let leaked = c.view().transcript_frames.get(&thread).copied().unwrap_or_default();
    ensure!(leaked == 0, "phone C got {leaked} transcript updates for a thread it never watched");
    ensure!(!c.view().answers.contains_key("q-image") && c.view().blobs.is_empty(), "phone C got phone A's answers");
    println!("ok   phone C got none of phone A's transcript updates, answers or blobs");
    ensure!(a.view().seq_gaps == 0 && c.view().seq_gaps == 0, "update seq skipped");
    println!("ok   update seq rose by one per frame on every socket ({} updates on phone A)", a.view().updates);
    let posts = run.posts.lock().unwrap_or_else(PoisonError::into_inner).clone();
    ensure!(!posts.is_empty(), "the notify hook got no notification");
    for post in &posts {
        let keys: HashSet<&str> =
            post.as_object().map(|post| post.keys().map(String::as_str).collect()).unwrap_or_default();
        ensure!(keys == HashSet::from(["machine", "kind", "thread"]), "a notify post carries {keys:?}");
        ensure!(
            !post.to_string().contains("remote-ok") && !post.to_string().contains("once"),
            "a notify post carries message text: {post}"
        );
    }
    println!("ok   the notify webhook got {} posts with only machine, kind and thread: {}", posts.len(), posts[0]);
    Ok(())
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

fn usage_check(eukhe: &Path, data: &Path, model: &Target) -> anyhow::Result<()> {
    let mut report: Option<UsageReport> = None;
    desktop(
        eukhe,
        data,
        "usage report loaded",
        |_, update| {
            if let Update::Usage { report: loaded, .. } = update {
                report = Some(loaded);
                return Ok(true);
            }
            Ok(false)
        },
        |studio| studio.load_usage("usage-1".to_string(), UsagePeriod::Day),
    )?;
    let report = report.context("no usage report")?;
    let entry = report.by_model.iter().find(|entry| entry.provider == model.provider && entry.model == model.model);
    println!(
        "     usage: {} sessions, {} tokens (in {} out {}), ${:.4}; {}/{} {} tokens ${:.4}",
        report.sessions,
        report.total_tokens,
        report.input_tokens,
        report.output_tokens,
        report.cost_usd,
        model.provider,
        model.model,
        entry.map_or(0, |entry| entry.tokens),
        entry.map_or(0.0, |entry| entry.cost_usd),
    );
    let entry = entry.context("the usage report has no entry for the model")?;
    ensure!(
        report.input_tokens > 0 && report.output_tokens > 0 && entry.tokens > 0,
        "the usage report counts no real tokens"
    );
    println!("ok   usage counts real tokens for {}/{} (a local model is priced at $0)", model.provider, model.model);
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn drive(run: &mut Run, eukhe: &Path, model: Option<&Target>) -> anyhow::Result<()> {
    run.serve.start(eukhe, &run.hook.clone())?;
    transport_checks(run, eukhe).await?;
    let a_phone = Phone::new("phone-a", "Smoke Phone A")?;
    let mut a = pairing_checks(run, &a_phone).await?;
    op_checks(run, &mut a).await?;
    let c_phone = Phone::new("phone-c", "Smoke Phone C")?;
    let mut c = pair(run, &c_phone).await?;
    let a = resume_checks(run, a, &a_phone, &mut c).await?;
    let mut a = limit_checks(run, a, &a_phone).await?;
    revoke_one(run).await?;
    println!("PASS remote smoke: non-model stages");
    let model_outcome = match model {
        Some(model) => model_stage(run, model, &mut a, &c).await,
        None => Err(anyhow::anyhow!(
            "model stage: LOCAL_STUDIO_SMOKE_AGENT is unset. Point it at a creds dir whose models.json names a local provider (abbey1: ~/.cache/ls-creds, Ollama qwen3-vl:4b)"
        )),
    };
    let started = Instant::now();
    run.serve.run(&["revoke", "--all"])?;
    a.closed_with("revoke --all closes phone A with 4401", 4401).await?;
    c.closed_with("revoke --all closes phone C with 4401", 4401).await?;
    println!("     revoke --all to close {} ms", started.elapsed().as_millis());
    ensure!(run.serve.run(&["devices"])?.contains("No paired devices"), "devices are left after revoke --all");
    println!("ok   no devices are left after revoke --all");
    model_outcome?;
    run.serve.stop()?;
    if let Some(model) = model {
        usage_check(eukhe, &run.serve.data, model)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let usage = "usage: remote_smoke <eukhe> <scratch dir>";
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
    let data = root.join("data");
    let work = root.join("work");
    std::fs::create_dir_all(root.join("home"))?;
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("EUKHE_DAEMON_SOCKET", root.join("d.sock"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", root.join("agent"));
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    let model = agent_dir(&root)?;
    let big = seed_work(&work)?;
    let project_path = work.to_string_lossy().into_owned();
    let daemon = desktop(
        &eukhe,
        &data,
        "the desktop adds the project, then quits",
        |_, update| {
            Ok(
                matches!(&update, Update::Projects { projects } if projects.iter().any(|project| project.path == project_path)),
            )
        },
        |studio| studio.add_project(work.to_string_lossy().into_owned()),
    )?;
    let (hook, posts) = hook_listener().await?;
    let serve = Serve { bin, root: root.clone(), data, port: free_port()?, child: None };
    let mut run = Run {
        serve,
        work,
        project: String::new(),
        big,
        hook,
        posts,
        server_key: Vec::new(),
        epoch: String::new(),
        report: Vec::new(),
    };
    let outcome = drive(&mut run, &eukhe, model.as_ref()).await;
    if outcome.is_err() {
        println!("     serve log tail:\n{}", run.serve.log_tail());
    }
    let stopped = run.serve.stop();
    let stopped_daemon = stop_daemon(&root.join("d.sock"), daemon).await;
    for line in &run.report {
        println!("     {line}");
    }
    outcome?;
    stopped?;
    stopped_daemon?;
    println!("PASS remote smoke");
    Ok(())
}
