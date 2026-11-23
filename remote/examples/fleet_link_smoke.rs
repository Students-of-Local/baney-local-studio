use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::snow::{self, TransportState};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
const XX: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
const KK: &str = "Noise_KK_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"local-studio-remote/1\n";
const QUICK: Duration = Duration::from_secs(20);
const PING_EVERY: Duration = Duration::from_secs(20);
const MAX_JSON: usize = 8 << 20;
const MODELS: &str = r#"{"object":"list","data":[{"id":"stub-model","object":"model"}]}"#;
const NODE: &str = r#"{"id":"stub-node","name":"stub","engines":[]}"#;
const EVENTS: [&str; 6] = [
    "data: {\"n\":1,\"text\":\"alpha\"}\n\n",
    "data: {\"n\":2,\"text\":\"beta\"}\n\n",
    "data: {\"n\":3,\"text\":\"gamma\"}\n\n",
    "data: {\"n\":4,\"text\":\"delta\"}\n\n",
    "data: {\"n\":5,\"text\":\"epsilon\"}\n\n",
    "data: [DONE]\n\n",
];
const BIG: usize = 7 << 20;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Message>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    target: String,
    headers: HashMap<String, String>,
}

#[derive(Default)]
struct Stub {
    // Serve always sends this key. A request without it is another program probing the port (a Local
    // controller on this machine scans every listener for /v1/models), and is answered 404 unrecorded.
    key: String,
    seen: Mutex<Vec<Seen>>,
    closed: Mutex<usize>,
    changed: Notify,
    connections: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl Stub {
    fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    fn count(&self) -> usize {
        lock(&self.seen).len()
    }

    fn closed(&self) -> usize {
        *lock(&self.closed)
    }

    async fn until(&self, step: &str, done: impl Fn(&Self) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + QUICK;
        loop {
            let notified = self.changed.notified();
            if done(self) {
                println!("ok   stub: {step}");
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("stub: timed out waiting for: {step}");
            };
            tokio::time::timeout(left.min(Duration::from_millis(200)), notified).await.ok();
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

async fn read_request(socket: &mut TcpStream, buffer: &mut Vec<u8>) -> anyhow::Result<Option<(Seen, Vec<u8>)>> {
    let mut scratch = vec![0u8; 64 << 10];
    loop {
        if let Some(end) = find(buffer, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let mut lines = head.split("\r\n");
            let mut first = lines.next().unwrap_or_default().split(' ');
            let method = first.next().unwrap_or_default().to_string();
            let target = first.next().unwrap_or_default().to_string();
            let headers: HashMap<String, String> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
                .collect();
            let length = headers.get("content-length").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
            while buffer.len() < end + 4 + length {
                let read = socket.read(&mut scratch).await?;
                ensure!(read > 0, "the request body was cut short");
                buffer.extend_from_slice(&scratch[..read]);
            }
            let body = buffer[end + 4..end + 4 + length].to_vec();
            buffer.drain(..end + 4 + length);
            return Ok(Some((Seen { method, target, headers }, body)));
        }
        let read = socket.read(&mut scratch).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&scratch[..read]);
    }
}

async fn respond(socket: &mut TcpStream, status: u16, kind: &str, body: &[u8]) -> anyhow::Result<()> {
    let head = format!("HTTP/1.1 {status} Stub\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\r\n", body.len());
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

const SSE_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";

async fn stub_connection(mut socket: TcpStream, stub: Arc<Stub>) -> anyhow::Result<()> {
    let mut buffer = Vec::new();
    while let Some((seen, body)) = read_request(&mut socket, &mut buffer).await? {
        let text = String::from_utf8_lossy(&body).into_owned();
        let path = seen.target.split('?').next().unwrap_or_default().to_string();
        if seen.headers.get("authorization") != Some(&format!("Bearer {}", stub.key)) {
            respond(&mut socket, 404, "text/plain", b"not found").await?;
            continue;
        }
        lock(&stub.seen).push(seen);
        stub.changed.notify_waiters();
        match path.as_str() {
            "/v1/models" => respond(&mut socket, 200, "application/json", MODELS.as_bytes()).await?,
            "/api/node" => respond(&mut socket, 200, "application/json", NODE.as_bytes()).await?,
            "/v1/messages" | "/v1/responses" => respond(&mut socket, 200, "application/json", b"{}").await?,
            "/v1/chat/completions" if text.contains("silent") => {
                let mut scratch = [0u8; 1024];
                while matches!(socket.read(&mut scratch).await, Ok(read) if read > 0) {}
                *lock(&stub.closed) += 1;
                stub.changed.notify_waiters();
                return Ok(());
            }
            "/v1/chat/completions" if text.contains("hang") => {
                socket.write_all(SSE_HEAD).await?;
                socket.write_all(EVENTS[0].as_bytes()).await?;
                socket.flush().await?;
                let mut scratch = [0u8; 1024];
                while matches!(socket.read(&mut scratch).await, Ok(read) if read > 0) {}
                *lock(&stub.closed) += 1;
                stub.changed.notify_waiters();
                return Ok(());
            }
            "/v1/chat/completions" => {
                socket.write_all(SSE_HEAD).await?;
                for event in EVENTS {
                    socket.write_all(event.as_bytes()).await?;
                    socket.flush().await?;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
                return Ok(());
            }
            "/v1/completions" => respond(&mut socket, 200, "application/json", &vec![b'x'; BIG]).await?,
            _ => respond(&mut socket, 404, "text/plain", b"not found").await?,
        }
    }
    Ok(())
}

async fn stub_serve(listener: TcpListener, stub: Arc<Stub>) {
    loop {
        let Ok((socket, _)) = listener.accept().await else { continue };
        let task = tokio::spawn(stub_connection(socket, Arc::clone(&stub)));
        lock(&stub.connections).push(task.abort_handle());
    }
}

async fn stub_down(stub: &Stub, listener: tokio::task::JoinHandle<()>) {
    listener.abort();
    listener.await.ok();
    for connection in lock(&stub.connections).drain(..) {
        connection.abort();
    }
}

#[derive(Default)]
struct View {
    hello: Option<Value>,
    replies: HashMap<u64, Value>,
    fleet: Vec<Value>,
    closed: bool,
    close_code: Option<u16>,
    broken: Option<String>,
}

impl View {
    fn apply(&mut self, text: &str) -> anyhow::Result<()> {
        let frame: Value = serde_json::from_str(text).context("a frame that is not JSON")?;
        if let Some(id) = frame.get("re").and_then(Value::as_u64) {
            self.replies.insert(id, frame);
            return Ok(());
        }
        match frame.get("op").and_then(Value::as_str) {
            Some("hello") => self.hello = Some(frame),
            Some("fleet") => self.fleet.push(frame.get("update").cloned().unwrap_or(Value::Null)),
            Some("update") => {
                let update = frame.get("update").cloned().unwrap_or(Value::Null);
                if update.get("type").and_then(Value::as_str).is_some_and(|kind| kind.starts_with("fleet_")) {
                    bail!("a fleet update arrived in the resumable update sequence: {update}");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn stream(&self, stream: &str) -> (Vec<Vec<u8>>, Option<Value>) {
        let mut chunks = Vec::new();
        let mut end = None;
        for update in self.fleet.iter().filter(|update| update["stream"] == stream) {
            match update["type"].as_str() {
                Some("fleet_chunk") => {
                    chunks.push(STANDARD.decode(update["data"].as_str().unwrap_or_default()).unwrap_or_default());
                }
                Some("fleet_end") => end = Some(update.clone()),
                _ => {}
            }
        }
        (chunks, end)
    }
}

fn seal(transport: &Mutex<TransportState>, record: &[u8]) -> Option<Vec<u8>> {
    let mut message = vec![0u8; record.len() + 16];
    let length = lock(transport).write_message(record, &mut message).ok()?;
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
        let mut now = lock(&view);
        match message {
            Ok(Message::Binary(bytes)) => {
                let mut record = vec![0u8; bytes.len()];
                let Ok(length) = lock(&transport).read_message(&bytes, &mut record) else {
                    now.broken.get_or_insert_with(|| "a record failed to decrypt".to_string());
                    break;
                };
                record.truncate(length);
                if matches!(record.first(), Some(1 | 2)) && joined.len() + record.len() - 1 > MAX_JSON {
                    now.broken.get_or_insert_with(|| format!("a JSON frame over {MAX_JSON} bytes"));
                    break;
                }
                match record.first() {
                    Some(1) => {
                        joined.extend_from_slice(&record[1..]);
                        let text = String::from_utf8(std::mem::take(&mut joined)).unwrap_or_default();
                        if let Err(error) = now.apply(&text) {
                            now.broken.get_or_insert(format!("{error:#}"));
                        }
                    }
                    Some(2) => joined.extend_from_slice(&record[1..]),
                    _ => {}
                }
            }
            Ok(Message::Close(frame)) => {
                now.close_code = frame.map(|frame| u16::from(frame.code));
                break;
            }
            Ok(Message::Text(text)) => {
                now.broken.get_or_insert(format!("a text message inside the channel: {text}"));
            }
            Err(_) => break,
            Ok(_) => {}
        }
        drop(now);
        changed.notify_waiters();
    }
    lock(&view).closed = true;
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

struct Client {
    name: String,
    out: mpsc::UnboundedSender<Vec<u8>>,
    view: Arc<Mutex<View>>,
    changed: Arc<Notify>,
    next: u64,
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
        lock(&self.view)
    }

    fn frame(&self, frame: &Value) {
        let payload = frame.to_string().into_bytes();
        let chunks: Vec<&[u8]> = payload.chunks(65_000).collect();
        let last = chunks.len().saturating_sub(1);
        for (at, chunk) in chunks.into_iter().enumerate() {
            let mut record = vec![if at == last { 1u8 } else { 2u8 }];
            record.extend_from_slice(chunk);
            self.out.send(record).ok();
        }
    }

    async fn until(&self, step: &str, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + QUICK;
        loop {
            let notified = self.changed.notified();
            {
                let view = self.view();
                if let Some(broken) = &view.broken {
                    bail!("{}: the wire broke: {broken}", self.name);
                }
                if done(&view) {
                    println!("ok   {}: {step}", self.name);
                    return Ok(());
                }
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("{}: timed out waiting for: {step}", self.name);
            };
            tokio::time::timeout(left.min(Duration::from_millis(200)), notified).await.ok();
        }
    }

    async fn hello(&self) -> anyhow::Result<()> {
        self.frame(&json!({ "op": "hello", "device": { "name": self.name, "app": "0.4.0", "channel": CHANNEL } }));
        self.until("hello answered", |view| view.hello.is_some() || view.closed).await?;
        ensure!(
            self.view().hello.is_some(),
            "{}: closed with {:?} instead of hello",
            self.name,
            self.view().close_code
        );
        Ok(())
    }

    fn ask(&mut self, op: &str, args: &Value) -> u64 {
        self.next += 1;
        self.frame(&json!({ "id": self.next, "op": op, "args": args }));
        self.next
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

    async fn reply(&mut self, op: &str, args: &Value) -> anyhow::Result<Value> {
        let id = self.ask(op, args);
        self.reply_to(id, op).await
    }

    async fn result(&mut self, op: &str, args: &Value) -> anyhow::Result<Value> {
        let reply = self.reply(op, args).await?;
        if let Some(error) = reply.get("error") {
            bail!("{}: {op} was refused: {error}", self.name);
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn refused(&mut self, op: &str, args: &Value, code: &str) -> anyhow::Result<()> {
        let reply = self.reply(op, args).await?;
        let got = reply.pointer("/error/code").and_then(Value::as_str);
        ensure!(got == Some(code), "{}: {op} {args} answered {reply}, expected error {code}", self.name);
        Ok(())
    }
}

fn http(method: &str, path: &str) -> Value {
    json!({ "method": method, "path": path })
}

fn body_of(result: &Value) -> Vec<u8> {
    STANDARD.decode(result["body"].as_str().unwrap_or_default()).unwrap_or_default()
}

fn status_of(result: &Value) -> u64 {
    result["status"].as_u64().unwrap_or_default()
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

async fn next_binary(socket: &mut Socket) -> anyhow::Result<Result<Vec<u8>, Option<u16>>> {
    loop {
        let message = tokio::time::timeout(QUICK, socket.next()).await.context("no message from serve in time")?;
        return Ok(match message {
            Some(Ok(Message::Binary(bytes))) => Ok(bytes.to_vec()),
            Some(Ok(Message::Close(frame))) => Err(frame.map(|frame| u16::from(frame.code))),
            None | Some(Err(_)) => Err(None),
            Some(Ok(Message::Text(text))) => bail!("a text message during the handshake: {text}"),
            Some(Ok(_)) => continue,
        });
    }
}

struct Device {
    id: String,
    name: String,
    private: Vec<u8>,
}

impl Device {
    fn new(name: &str) -> anyhow::Result<Self> {
        let pair = snow::Builder::new(KK.parse()?).generate_keypair()?;
        let id = format!("{name}-{}", hex(&pair.public[..6]));
        Ok(Self { id, name: name.to_string(), private: pair.private })
    }

    async fn pair(&self, port: u16, pairing: &str, psk: &[u8; 32]) -> anyhow::Result<(Client, Vec<u8>)> {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/ws")).await?;
        let header = json!({ "v": 1, "pattern": "XXpsk3", "pair": pairing, "protocol": 2, "min_protocol": 2 });
        let header = header.to_string();
        socket.send(Message::text(header.clone())).await?;
        let mut state = snow::Builder::new(XX.parse()?)
            .local_private_key(&self.private)?
            .psk(3, psk)?
            .prologue(&prologue(&header))?
            .build_initiator()?;
        socket.send(Message::binary(noise_write(&mut state, &[])?)).await?;
        let second = next_binary(&mut socket)
            .await?
            .map_err(|code| anyhow::anyhow!("{}: closed with {code:?} during pairing", self.name))?;
        let mut scratch = vec![0u8; 65_535];
        state.read_message(&second, &mut scratch)?;
        let server = state.get_remote_static().context("serve sent no static key")?.to_vec();
        let record =
            json!({ "id": self.id, "name": self.name, "platform": "linux", "app": "0.4.0", "channel": CHANNEL });
        socket.send(Message::binary(noise_write(&mut state, record.to_string().as_bytes())?)).await?;
        let client = Client::spawn(&self.name, socket, state.into_transport_mode()?);
        client.hello().await?;
        Ok((client, server))
    }

    async fn reconnect_refused(&self, port: u16, server: &[u8]) -> anyhow::Result<Option<u16>> {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/ws")).await?;
        let header = json!({ "v": 1, "pattern": "KK", "device": self.id, "protocol": 2, "min_protocol": 2 });
        let header = header.to_string();
        socket.send(Message::text(header.clone())).await?;
        let mut state = snow::Builder::new(KK.parse()?)
            .local_private_key(&self.private)?
            .remote_public_key(server)?
            .prologue(&prologue(&header))?
            .build_initiator()?;
        socket.send(Message::binary(noise_write(&mut state, &[])?)).await.ok();
        match next_binary(&mut socket).await? {
            Ok(_) => bail!("{}: serve answered the KK handshake of a revoked device", self.name),
            Err(code) => Ok(code),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        write!(text, "{byte:02x}").ok();
        text
    })
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

struct Scratch {
    root: PathBuf,
    data: PathBuf,
    controller: PathBuf,
    controller_port: u16,
}

impl Scratch {
    fn isolate(&self, command: &mut Command) {
        command
            .env("HOME", self.root.join("home"))
            .env("EUKHE_DAEMON_SOCKET", self.root.join("d.sock"))
            .env("EUKHE_CODING_AGENT_DIR", self.root.join("agent"))
            .env("LOCAL_STUDIO_DATA_DIR", &self.data)
            .env("LOCAL_STUDIO_CONTROLLER_HOME", &self.controller)
            .env("LOCAL_STUDIO_CONTROLLER_PORT", self.controller_port.to_string())
            .env_remove("LOCAL_STUDIO_CONTROLLER_URL")
            .env_remove("EUKHE_BIN");
    }
}

struct Serve {
    child: Option<Child>,
    log: PathBuf,
}

impl Serve {
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

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            Command::new("kill").args(["-TERM", &child.id().to_string()]).status().ok();
            child.wait().ok();
        }
    }
}

fn run(scratch: &Scratch, bin: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut command = Command::new(bin);
    scratch.isolate(&mut command);
    let output = command.args(args).output()?;
    ensure!(
        output.status.success(),
        "{} {args:?} failed: {}{}",
        bin.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn pairing_of(minted: &Value) -> anyhow::Result<(String, [u8; 32])> {
    ensure!(minted["state"] == "minted", "the pairing was not minted: {minted}");
    ensure!(minted["scopes"] == json!(["fleet"]), "the minted pairing does not carry the fleet scope: {minted}");
    let link = minted["link"].as_str().context("no link")?;
    ensure!(!link.contains("scope") && !link.contains("fleet"), "the link carries the scope: {link}");
    let (query, secret) = link.split_once('#').context("the link has no secret")?;
    let id = query.split(['?', '&']).find_map(|part| part.strip_prefix("id=")).context("the link has no id")?;
    let secret = <[u8; 32]>::try_from(URL_SAFE_NO_PAD.decode(secret)?)
        .map_err(|_| anyhow::anyhow!("the pairing secret is not 32 bytes"))?;
    Ok((id.to_string(), secret))
}

fn pending_scopes(data: &Path) -> anyhow::Result<Value> {
    let pending: Value = serde_json::from_slice(&std::fs::read(data.join("remote").join("pairing.json"))?)?;
    Ok(pending.get("scopes").cloned().unwrap_or(Value::Null))
}

async fn healthy(port: u16) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
            let ask = format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
            stream.write_all(ask.as_bytes()).await?;
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.ok();
            if answer.starts_with("HTTP/1.1 200") {
                return Ok(());
            }
        }
        ensure!(Instant::now() < deadline, "serve did not answer /v1/health within 60 s");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct Run<'a> {
    scratch: &'a Scratch,
    serve_bin: &'a Path,
    studio_bin: &'a Path,
    serve: &'a Serve,
    port: u16,
    stub: &'a Arc<Stub>,
    key: &'a str,
}

#[allow(clippy::too_many_lines)]
async fn stages(run_: Run<'_>, stub_task: tokio::task::JoinHandle<()>) -> anyhow::Result<()> {
    let Run { scratch, serve_bin, studio_bin, serve, port, stub, key } = run_;
    let data = scratch.data.to_string_lossy().into_owned();
    let url = format!("ws://127.0.0.1:{port}/v1/ws");

    let minted: Value = serde_json::from_str(
        run(scratch, serve_bin, &["pair", "--fleet", "--json", "--url", &url, "--data-dir", &data])?.trim(),
    )?;
    let (pairing, secret) = pairing_of(&minted)?;
    ensure!(pending_scopes(&scratch.data)? == json!(["fleet"]), "the pending pairing does not hold the fleet scope");
    println!(
        "ok   pair --fleet --json minted a pairing with scopes [\"fleet\"], kept in pairing.json and not in the link"
    );
    let fleet = Device::new("fleet-desk")?;
    let (mut a, server_key) = fleet.pair(port, &pairing, &secret).await?;

    let minted: Value =
        serde_json::from_str(run(scratch, serve_bin, &["pair", "--json", "--url", &url, "--data-dir", &data])?.trim())?;
    ensure!(minted.get("scopes").is_none(), "an ordinary pairing printed scopes: {minted}");
    ensure!(pending_scopes(&scratch.data)?.is_null(), "an ordinary pending pairing holds scopes");
    let link = minted["link"].as_str().context("no link")?;
    let (query, secret) = link.split_once('#').context("the link has no secret")?;
    let id = query.split(['?', '&']).find_map(|part| part.strip_prefix("id=")).context("no id")?.to_string();
    let secret = <[u8; 32]>::try_from(URL_SAFE_NO_PAD.decode(secret)?).map_err(|_| anyhow::anyhow!("bad secret"))?;
    let phone = Device::new("plain-phone")?;
    let (mut p, _) = phone.pair(port, &id, &secret).await?;

    let invited: Value = serde_json::from_str(run(scratch, studio_bin, &["fleet", "invite", "--json"])?.trim())?;
    let (pairing, secret) = pairing_of(&invited)?;
    println!("ok   local-studio fleet invite --json minted a fleet pairing: {}", invited["state"]);
    let other = Device::new("invited-desk")?;
    let (mut c, _) = other.pair(port, &pairing, &secret).await?;

    let ledger: Value = serde_json::from_slice(&std::fs::read(scratch.data.join("remote").join("devices.json"))?)?;
    let scopes: HashMap<String, Value> = ledger["devices"]
        .as_array()
        .context("devices.json lists no devices")?
        .iter()
        .map(|device| {
            (
                device["id"].as_str().unwrap_or_default().to_string(),
                device.get("scopes").cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    ensure!(
        scopes.get(&fleet.id) == Some(&json!(["fleet"])),
        "devices.json: {} has scopes {:?}",
        fleet.id,
        scopes.get(&fleet.id)
    );
    ensure!(
        scopes.get(&other.id) == Some(&json!(["fleet"])),
        "devices.json: {} has scopes {:?}",
        other.id,
        scopes.get(&other.id)
    );
    ensure!(
        scopes.get(&phone.id) == Some(&Value::Null),
        "devices.json: the plain phone has scopes {:?}",
        scopes.get(&phone.id)
    );
    let listed = run(scratch, serve_bin, &["devices", "--data-dir", &data])?;
    for (device, fleet_scoped) in [(&fleet.id, true), (&other.id, true), (&phone.id, false)] {
        let row = listed.lines().find(|line| line.starts_with(device.as_str())).context("devices omits a device")?;
        ensure!(row.contains("scopes fleet") == fleet_scoped, "devices shows {row:?}");
    }
    println!("ok   devices.json and `devices` show the fleet scope on the two fleet devices only");

    let asked = json!({
        "method": "GET",
        "path": "/v1/models",
        "query": { "q": "a b&c=d", "n": 2 },
        "headers": {
            "Accept": "application/json",
            "x-local-studio-endpoint": "e-1",
            "authorization": "Bearer not-the-key",
            "x-local-studio-hop": "0",
            "cookie": "nope",
            "x-forwarded-for": "10.0.0.1"
        },
    });
    let models = a.result("fleet_http", &asked).await?;
    ensure!(status_of(&models) == 200, "GET /v1/models answered {models}");
    ensure!(body_of(&models) == MODELS.as_bytes(), "GET /v1/models body differs");
    ensure!(models["headers"] == json!({ "content-type": "application/json" }), "headers {}", models["headers"]);
    let seen = stub.seen();
    ensure!(seen.len() == 1, "the stub saw {} requests", seen.len());
    let first = &seen[0];
    ensure!(first.method == "GET" && first.target == "/v1/models?q=a+b%26c%3Dd&n=2", "the stub saw {first:?}");
    ensure!(
        first.headers.get("authorization") == Some(&format!("Bearer {key}")),
        "the stub saw the wrong authorization"
    );
    ensure!(
        first.headers.get("x-local-studio-hop").map(String::as_str) == Some("1"),
        "no hop header: {:?}",
        first.headers
    );
    ensure!(first.headers.get("accept").map(String::as_str) == Some("application/json"), "accept was not passed");
    ensure!(
        first.headers.get("x-local-studio-endpoint").map(String::as_str) == Some("e-1"),
        "endpoint header not passed"
    );
    for dropped in ["cookie", "x-forwarded-for"] {
        ensure!(!first.headers.contains_key(dropped), "the stub saw {dropped}");
    }
    println!("ok   fleet device: GET /v1/models 200; the stub saw the fleet key, x-local-studio-hop: 1, the allowed headers only and an encoded query");

    let node = a.result("fleet_http", &http("GET", "/api/node")).await?;
    ensure!(status_of(&node) == 200 && body_of(&node) == NODE.as_bytes(), "GET /api/node answered {node}");
    let seen = stub.seen();
    let last = seen.last().context("the stub saw nothing")?;
    ensure!(last.target == "/api/node", "the stub saw {last:?}");
    ensure!(!last.headers.contains_key("x-local-studio-hop"), "GET /api/node carried the hop header");
    ensure!(last.headers.get("authorization") == Some(&format!("Bearer {key}")), "GET /api/node had no fleet key");
    println!("ok   fleet device: GET /api/node 200 with the fleet key and no hop header");
    for path in ["/v1/messages", "/v1/responses"] {
        let asked = json!({ "method": "POST", "path": path, "body": STANDARD.encode(b"{}") });
        let answer = a.result("fleet_http", &asked).await?;
        ensure!(status_of(&answer) == 200, "POST {path} answered {answer}");
        let seen = stub.seen();
        let last = seen.last().context("the stub saw nothing")?;
        ensure!(last.method == "POST" && last.target == path, "the stub saw {last:?}");
        ensure!(
            last.headers.get("x-local-studio-hop").map(String::as_str) == Some("1"),
            "POST {path} had no hop header"
        );
        ensure!(last.headers.get("authorization") == Some(&format!("Bearer {key}")), "POST {path} had no fleet key");
    }
    println!("ok   fleet device: POST /v1/messages and /v1/responses reach the controller with the hop header and key");

    let chat = json!({
        "method": "POST",
        "path": "/v1/chat/completions",
        "headers": { "content-type": "application/json", "accept": "text/event-stream" },
        "body": STANDARD.encode(br#"{"model":"stub-model","stream":true,"messages":[{"role":"user","content":"events"}]}"#),
        "stream": true,
    });
    let opened = a.result("fleet_http", &chat).await?;
    ensure!(status_of(&opened) == 200, "the stream opened with {opened}");
    ensure!(opened["headers"]["content-type"] == "text/event-stream", "stream headers {}", opened["headers"]);
    let stream = opened["stream"].as_str().context("no stream id")?.to_string();
    a.until("fleet_end for the SSE stream", |view| view.stream(&stream).1.is_some()).await?;
    let (chunks, end) = a.view().stream(&stream);
    let end = end.context("no fleet_end")?;
    ensure!(end.get("error").is_none(), "the stream ended with an error: {end}");
    ensure!(chunks.len() >= 2, "the stream arrived as {} chunk", chunks.len());
    ensure!(chunks.iter().all(|chunk| chunk.len() <= 64 << 10), "a chunk passed 64 KiB");
    let joined: Vec<u8> = chunks.concat();
    ensure!(
        joined == EVENTS.concat().as_bytes(),
        "the stream arrived out of order or changed: {}",
        String::from_utf8_lossy(&joined)
    );
    let after_end = a.view().fleet.iter().rposition(|update| update["stream"] == stream.as_str()).unwrap_or_default();
    ensure!(a.view().fleet[after_end]["type"] == "fleet_end", "an update for the stream came after fleet_end");
    let seen = stub.seen();
    let last = seen.last().context("the stub saw nothing")?;
    ensure!(last.method == "POST" && last.target == "/v1/chat/completions", "the stub saw {last:?}");
    ensure!(
        last.headers.get("x-local-studio-hop").map(String::as_str) == Some("1"),
        "the chat request had no hop header"
    );
    println!(
        "ok   fleet device: a streamed chat arrived as {} fleet_chunk updates in order, then one fleet_end",
        chunks.len()
    );

    let hang = json!({
        "method": "POST",
        "path": "/v1/chat/completions",
        "headers": { "content-type": "application/json" },
        "body": STANDARD.encode(br#"{"stream":true,"messages":[{"role":"user","content":"hang"}]}"#),
        "stream": true,
    });
    let opened = a.result("fleet_http", &hang).await?;
    let held = opened["stream"].as_str().context("no stream id")?.to_string();
    a.until("the first chunk of the held stream", |view| !view.stream(&held).0.is_empty()).await?;
    let before = stub.closed();
    a.result("fleet_cancel", &json!({ "stream": held })).await?;
    stub.until("the cancelled stream's connection closed", |stub| stub.closed() > before).await?;
    a.until("fleet_end after fleet_cancel", |view| view.stream(&held).1.is_some()).await?;
    let end = a.view().stream(&held).1.context("no fleet_end")?;
    ensure!(end["error"] == "cancelled", "the cancelled stream ended with {end}");
    a.refused("fleet_cancel", &json!({ "stream": held }), "unknown_stream").await?;

    let silent = json!({
        "method": "POST",
        "path": "/v1/chat/completions",
        "headers": { "content-type": "application/json" },
        "body": STANDARD.encode(br#"{"stream":true,"messages":[{"role":"user","content":"silent"}]}"#),
        "stream": "early-1",
    });
    let seen = lock(&stub.seen).len();
    let before = stub.closed();
    let asked = a.ask("fleet_http", &silent);
    stub.until("the silent request reached the controller", |stub| lock(&stub.seen).len() > seen).await?;
    a.refused("fleet_http", &silent, "bad_args").await?;
    a.result("fleet_cancel", &json!({ "stream": "early-1" })).await?;
    stub.until("the unanswered stream's connection closed", |stub| stub.closed() > before).await?;
    let answered = a.reply_to(asked, "fleet_http").await?;
    ensure!(
        status_of(answered.get("result").unwrap_or(&Value::Null)) == 499,
        "a cancel before the answer gave {answered}"
    );
    println!("ok   fleet device: a client-named stream cancelled before the controller answered closed its connection");
    println!(
        "ok   fleet device: fleet_cancel mid-stream closed the controller connection and ended the stream as cancelled"
    );

    let mut held = Vec::new();
    for _ in 0..8 {
        let opened = a.result("fleet_http", &hang).await?;
        ensure!(status_of(&opened) == 200, "a held stream opened with {opened}");
        held.push(opened["stream"].as_str().context("no stream id")?.to_string());
    }
    let ninth = a.result("fleet_http", &hang).await?;
    ensure!(status_of(&ninth) == 429 && ninth.get("stream").is_none(), "a ninth stream answered {ninth}");
    let before = stub.closed();
    for stream in &held {
        a.result("fleet_cancel", &json!({ "stream": stream })).await?;
    }
    stub.until("eight held connections closed", |stub| stub.closed() >= before + 8).await?;
    println!("ok   fleet device: eight streams at once are served and a ninth is answered 429");

    let count = stub.count();
    for (method, path) in [
        ("POST", "/api/cluster"),
        ("GET", "/api/snapshot"),
        ("GET", "/v1/../api/snapshot"),
        ("GET", "/v1/models/../api/snapshot"),
        ("GET", "/v1/models/"),
        ("GET", "/v1/models?x=1"),
        ("GET", "/v1//models"),
        ("GET", "/V1/MODELS"),
        ("get", "/v1/models"),
        ("POST", "/v1/models"),
        ("GET", "/v1/chat/completions"),
        ("POST", "/api/peers"),
        ("GET", "http://127.0.0.1:1/v1/models"),
    ] {
        a.refused("fleet_http", &http(method, path), "forbidden").await?;
    }
    ensure!(stub.count() == count, "the stub saw a refused path: {:?}", &stub.seen()[count..]);
    println!("ok   fleet device: /api/cluster, /api/snapshot, .. paths and every other method and path are refused with forbidden, and none reached the stub");

    p.refused("fleet_http", &http("GET", "/v1/models"), "forbidden").await?;
    p.refused("fleet_http", &chat, "forbidden").await?;
    p.refused("fleet_cancel", &json!({ "stream": stream }), "forbidden").await?;
    ensure!(stub.count() == count, "the stub saw a request from the plain phone");
    ensure!(p.view().fleet.is_empty(), "the plain phone received {} fleet updates", p.view().fleet.len());
    ensure!(c.view().fleet.is_empty(), "the other fleet device received {} fleet updates", c.view().fleet.len());
    println!(
        "ok   plain phone: fleet_http and fleet_cancel are forbidden, and neither other device saw a fleet update"
    );

    let big = json!({ "method": "POST", "path": "/v1/completions", "body": STANDARD.encode(b"{\"prompt\":\"big\"}") });
    let answered = a.result("fleet_http", &big).await?;
    ensure!(
        status_of(&answered) == 413 && answered.get("stream").is_none(),
        "a 7 MiB answer gave {}",
        status_of(&answered)
    );
    println!("ok   fleet device: a 7 MiB non-streaming answer from the controller is answered 413");

    let node = c.result("fleet_http", &http("GET", "/api/node")).await?;
    ensure!(status_of(&node) == 200, "the invited device got {node}");
    let opened = c.result("fleet_http", &hang).await?;
    let open = opened["stream"].as_str().context("no stream id")?.to_string();
    c.until("the first chunk of a stream held over revoke", |view| !view.stream(&open).0.is_empty()).await?;
    let before = stub.closed();
    run(scratch, serve_bin, &["revoke", &other.id, "--data-dir", &data])?;
    let id = c.ask("fleet_http", &http("GET", "/api/node"));
    c.until("closed after revoke", |view| view.closed || view.replies.contains_key(&id)).await?;
    if let Some(reply) = c.view().replies.remove(&id) {
        ensure!(
            reply.pointer("/error/code") == Some(&json!("forbidden")),
            "a revoked device's fleet_http answered {reply}"
        );
    }
    c.until("socket closed after revoke", |view| view.closed).await?;
    ensure!(c.view().close_code == Some(4401), "the revoked device closed with {:?}", c.view().close_code);
    stub.until("the revoked device's open stream closed at the controller", |stub| stub.closed() > before).await?;
    let code = other.reconnect_refused(port, &server_key).await?;
    ensure!(code == Some(4401), "the revoked device's reconnect closed with {code:?}");
    println!(
        "ok   invited device: after `revoke` its socket closes with 4401, its open stream's controller connection closes, and it cannot reconnect to call fleet_http"
    );

    stub_down(stub, stub_task).await;
    let down = a.result("fleet_http", &http("GET", "/v1/models")).await?;
    ensure!(status_of(&down) == 503, "the stub down gave {down}");
    ensure!(!body_of(&down).is_empty(), "the 503 has no body");
    println!(
        "ok   fleet device: with the controller down fleet_http answers 503: {}",
        String::from_utf8_lossy(&body_of(&down))
    );

    let mut statuses = Vec::new();
    for _ in 0..33 {
        statuses.push(status_of(&a.result("fleet_http", &http("GET", "/v1/models")).await?));
    }
    let first = statuses.iter().position(|status| *status == 429).context("33 quick requests got no 429")?;
    ensure!(statuses[first..].iter().all(|status| *status == 429), "requests passed after a 429: {statuses:?}");
    ensure!(statuses[..first].iter().all(|status| *status == 503), "unexpected statuses {statuses:?}");
    println!("ok   fleet device: past 32 requests a minute fleet_http answers 429 (after {first} more in this burst)");

    ensure!(p.view().fleet.is_empty(), "the plain phone received fleet updates");
    let log = serve.log();
    ensure!(!log.contains(key), "serve's log holds the fleet key");
    ensure!(!log.contains("epsilon") && !log.contains("stub-model"), "serve's log holds a body");
    println!("ok   serve's log holds neither the fleet key nor a body");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let usage = "usage: fleet_link_smoke <scratch dir>";
    let root = PathBuf::from(std::env::args_os().nth(1).context(usage)?);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let bins = std::env::current_exe()?.parent().and_then(Path::parent).context("no target dir")?.to_path_buf();
    let serve_bin = bins.join("local-studio-serve");
    let studio_bin = bins.join("local-studio");
    ensure!(
        serve_bin.is_file() && studio_bin.is_file(),
        "build both binaries first: cargo build --workspace --bins --examples"
    );
    let data = root.join("data");
    let controller = root.join("controller");
    for dir in [root.join("home"), root.join("agent"), controller.clone()] {
        std::fs::create_dir_all(dir)?;
    }
    let key = format!("stub-fleet-key-{}", hex(&snow::Builder::new(KK.parse()?).generate_keypair()?.public[..12]));
    std::fs::write(controller.join("config.json"), json!({ "fleetKey": key }).to_string())?;
    let stub_listener = TcpListener::bind("127.0.0.1:0").await?;
    let controller_port = stub_listener.local_addr()?.port();
    let stub = Arc::new(Stub { key: key.clone(), ..Stub::default() });
    let stub_task = tokio::spawn(stub_serve(stub_listener, Arc::clone(&stub)));
    let scratch = Scratch { root: root.clone(), data: data.clone(), controller, controller_port };
    let port = free_port()?;
    let log = root.join("serve.log");
    let mut command = Command::new(&serve_bin);
    scratch.isolate(&mut command);
    let child = command
        .args(["serve", "--port", &port.to_string(), "--name", "fleet-smoke", "--data-dir"])
        .arg(&data)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log)?)
        .spawn()
        .context("start local-studio-serve")?;
    println!("     serve pid {} on 127.0.0.1:{port}, stub controller on 127.0.0.1:{controller_port}", child.id());
    let mut serve = Serve { child: Some(child), log };
    healthy(port).await?;
    let plan = Run {
        scratch: &scratch,
        serve_bin: &serve_bin,
        studio_bin: &studio_bin,
        serve: &serve,
        port,
        stub: &stub,
        key: &key,
    };
    let outcome = stages(plan, stub_task).await;
    if outcome.is_err() {
        let log = serve.log();
        let lines: Vec<&str> = log.lines().collect();
        eprintln!("serve log tail:\n{}", lines[lines.len().saturating_sub(25)..].join("\n"));
    }
    serve.stop()?;
    outcome?;
    println!("fleet_link_smoke: every check passed");
    Ok(())
}
