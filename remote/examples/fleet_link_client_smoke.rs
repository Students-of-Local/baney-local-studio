use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::proxy::{self, FLEET_BODY, PORT};
use local_studio_core::link::snow::{self, TransportState};
use local_studio_core::{Studio, StudioConfig, Update, Waker};
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
const MODELS: &str = r#"{"object":"list","data":[{"id":"a-model","object":"model"}]}"#;
const EVENTS: [&str; 6] = [
    "data: {\"n\":1,\"text\":\"alpha\"}\n\n",
    "data: {\"n\":2,\"text\":\"beta\"}\n\n",
    "data: {\"n\":3,\"text\":\"gamma\"}\n\n",
    "data: {\"n\":4,\"text\":\"delta\"}\n\n",
    "data: {\"n\":5,\"text\":\"epsilon\"}\n\n",
    "data: [DONE]\n\n",
];
const SSE_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Message>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

fn listening() -> Vec<u16> {
    let output = Command::new("ss").args(["-H", "-ltn"]).output().map(|output| output.stdout).unwrap_or_default();
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(3)?.rsplit(':').next()?.parse().ok())
        .collect()
}

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    length: usize,
}

struct Stub {
    label: &'static str,
    key: String,
    node: String,
    seen: Mutex<Vec<Seen>>,
    closed: Mutex<usize>,
    changed: Notify,
    gate: Notify,
}

impl Stub {
    fn new(label: &'static str, key: &str, node: &Value) -> Arc<Self> {
        Arc::new(Self {
            label,
            key: key.to_string(),
            node: node.to_string(),
            seen: Mutex::new(Vec::new()),
            closed: Mutex::new(0),
            changed: Notify::new(),
            gate: Notify::new(),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    fn count(&self) -> usize {
        lock(&self.seen).len()
    }

    fn served(&self) -> Vec<Seen> {
        self.seen().into_iter().filter(|seen| seen.target != "/api/node").collect()
    }

    fn closed(&self) -> usize {
        *lock(&self.closed)
    }

    async fn until(&self, step: &str, done: impl Fn(&Self) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + QUICK;
        loop {
            let notified = self.changed.notified();
            if done(self) {
                println!("ok   {}: {step}", self.label);
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("{}: timed out waiting for: {step}", self.label);
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
            return Ok(Some((Seen { method, target, headers, length }, body)));
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

async fn held_until_closed(socket: &mut TcpStream, stub: &Stub) {
    let mut scratch = [0u8; 1024];
    while matches!(socket.read(&mut scratch).await, Ok(read) if read > 0) {}
    *lock(&stub.closed) += 1;
    stub.changed.notify_waiters();
}

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
            "/api/node" => respond(&mut socket, 200, "application/json", stub.node.as_bytes()).await?,
            "/v1/models" => respond(&mut socket, 200, "application/json", MODELS.as_bytes()).await?,
            "/v1/chat/completions" if text.contains("hang") => {
                socket.write_all(SSE_HEAD).await?;
                socket.write_all(EVENTS[0].as_bytes()).await?;
                socket.flush().await?;
                held_until_closed(&mut socket, &stub).await;
                return Ok(());
            }
            "/v1/chat/completions" if text.contains("gate") => {
                socket.write_all(SSE_HEAD).await?;
                socket.write_all(EVENTS[0].as_bytes()).await?;
                socket.flush().await?;
                tokio::time::timeout(QUICK, stub.gate.notified()).await.ok();
                for event in &EVENTS[1..] {
                    socket.write_all(event.as_bytes()).await?;
                    socket.flush().await?;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
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
            _ => respond(&mut socket, 404, "application/json", b"{\"error\":\"not here\"}").await?,
        }
    }
    Ok(())
}

async fn stub_serve(listener: TcpListener, stub: Arc<Stub>) {
    loop {
        let Ok((socket, _)) = listener.accept().await else { continue };
        tokio::spawn(stub_connection(socket, Arc::clone(&stub)));
    }
}

fn endpoint(controller: &str, port: u16, models: &[&str], running: u32) -> Value {
    json!({
        "id": format!("{controller}:{port}"),
        "controllerId": controller,
        "port": port,
        "pid": null,
        "models": models,
        "live": true,
        "lastSeenAt": null,
        "jobId": null,
        "latencyMs": 3,
        "load": { "inFlight": 0, "running": running, "waiting": 0, "kvPercent": 0, "decodeTps": 0 },
    })
}

fn node(id: &str, name: &str, url: &str, endpoints: &Value) -> Value {
    json!({
        "controller": { "id": id, "name": name, "url": url, "version": "0.1.0" },
        "endpoints": endpoints,
        "usage": [],
        "peers": [],
    })
}

#[derive(Default)]
struct View {
    hello: Option<Value>,
    replies: HashMap<u64, Value>,
    fleet: usize,
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
            Some("fleet") => self.fleet += 1,
            _ => {}
        }
        Ok(())
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
    while let Some(record) = rx.recv().await {
        let Some(message) = seal(&transport, &record) else { return };
        if sink.send(Message::binary(message)).await.is_err() {
            return;
        }
    }
    sink.close().await.ok();
}

struct Phone {
    name: String,
    out: mpsc::UnboundedSender<Vec<u8>>,
    view: Arc<Mutex<View>>,
    changed: Arc<Notify>,
    next: u64,
}

impl Phone {
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
        let mut record = vec![1u8];
        record.extend_from_slice(frame.to_string().as_bytes());
        self.out.send(record).ok();
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

    async fn refused(&mut self, op: &str, args: &Value, code: &str) -> anyhow::Result<()> {
        self.next += 1;
        let id = self.next;
        self.frame(&json!({ "id": id, "op": op, "args": args }));
        self.until(&format!("a reply to {op}"), |view| view.replies.contains_key(&id) || view.closed).await?;
        let reply = self.view().replies.remove(&id).context("the socket closed before a reply")?;
        let got = reply.pointer("/error/code").and_then(Value::as_str);
        ensure!(got == Some(code), "{}: {op} {args} answered {reply}, expected error {code}", self.name);
        Ok(())
    }
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

async fn next_binary(socket: &mut Socket) -> anyhow::Result<Vec<u8>> {
    loop {
        let message = tokio::time::timeout(QUICK, socket.next()).await.context("no message from serve in time")?;
        match message {
            Some(Ok(Message::Binary(bytes))) => return Ok(bytes.to_vec()),
            Some(Ok(Message::Close(frame))) => bail!("serve closed with {:?} during pairing", frame.map(|f| f.code)),
            None | Some(Err(_)) => bail!("serve dropped the socket during pairing"),
            Some(Ok(_)) => {}
        }
    }
}

async fn pair_phone(name: &str, port: u16, link: &str) -> anyhow::Result<Phone> {
    let (query, secret) = link.split_once('#').context("the link has no secret")?;
    let pairing = query.split(['?', '&']).find_map(|part| part.strip_prefix("id=")).context("the link has no id")?;
    let psk = <[u8; 32]>::try_from(URL_SAFE_NO_PAD.decode(secret)?)
        .map_err(|_| anyhow::anyhow!("the pairing secret is not 32 bytes"))?;
    let keys = snow::Builder::new(KK.parse()?).generate_keypair()?;
    let id = format!("{name}-{}", hex(&keys.public[..6]));
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/ws")).await?;
    let header = json!({ "v": 1, "pattern": "XXpsk3", "pair": pairing, "protocol": 2, "min_protocol": 2 }).to_string();
    socket.send(Message::text(header.clone())).await?;
    let mut state = snow::Builder::new(XX.parse()?)
        .local_private_key(&keys.private)?
        .psk(3, &psk)?
        .prologue(&prologue(&header))?
        .build_initiator()?;
    socket.send(Message::binary(noise_write(&mut state, &[])?)).await?;
    let second = next_binary(&mut socket).await?;
    let mut scratch = vec![0u8; 65_535];
    state.read_message(&second, &mut scratch)?;
    let record = json!({ "id": id, "name": name, "platform": "linux", "app": "0.4.0", "channel": CHANNEL });
    socket.send(Message::binary(noise_write(&mut state, record.to_string().as_bytes())?)).await?;
    let phone = Phone::spawn(name, socket, state.into_transport_mode()?);
    phone.hello().await?;
    Ok(phone)
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
        "/CN=fleet link client smoke test CA",
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

async fn terminator(tls: &Path, upstream: u16) -> anyhow::Result<(u16, tokio::task::JoinHandle<()>)> {
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
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { continue };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", upstream)).await else { return };
                tokio::io::copy_bidirectional(&mut tls, &mut upstream).await.ok();
            });
        }
    });
    Ok((port, task))
}

#[derive(Clone)]
struct Side {
    label: &'static str,
    root: PathBuf,
    data: PathBuf,
    controller: PathBuf,
    env: Vec<(&'static str, OsString)>,
}

impl Side {
    fn new(root: &Path, label: &'static str) -> anyhow::Result<Self> {
        let root = root.join(label);
        for dir in ["home", "agent", "controller"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        let env = vec![
            ("HOME", root.join("home").into_os_string()),
            ("EUKHE_DAEMON_SOCKET", root.join("d.sock").into_os_string()),
            ("EUKHE_CODING_AGENT_DIR", root.join("agent").into_os_string()),
            ("LOCAL_STUDIO_DATA_DIR", root.join("data").into_os_string()),
            ("LOCAL_STUDIO_CONTROLLER_HOME", root.join("controller").into_os_string()),
        ];
        Ok(Self { label, data: root.join("data"), controller: root.join("controller"), root, env })
    }

    fn set(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.env.retain(|(held, _)| *held != name);
        self.env.push((name, value.into()));
    }

    fn isolate(&self, command: &mut Command) {
        for name in ["LOCAL_STUDIO_CONTROLLER_URL", "LOCAL_STUDIO_CONTROLLER_PORT", "EUKHE_BIN", proxy_ca()] {
            command.env_remove(name);
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
    }

    fn adopt(&self) {
        for name in ["LOCAL_STUDIO_CONTROLLER_URL", "LOCAL_STUDIO_CONTROLLER_PORT", "EUKHE_BIN", proxy_ca()] {
            std::env::remove_var(name);
        }
        for (name, value) in &self.env {
            std::env::set_var(name, value);
        }
    }

    fn run(&self, bin: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
        let mut command = Command::new(bin);
        self.isolate(&mut command);
        Ok(command.args(args).output()?)
    }

    fn ok(&self, bin: &Path, args: &[&str]) -> anyhow::Result<String> {
        let output = self.run(bin, args)?;
        ensure!(
            output.status.success(),
            "{}: {} {args:?} failed: {}{}",
            self.label,
            bin.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn data_arg(&self) -> String {
        self.data.to_string_lossy().into_owned()
    }
}

fn proxy_ca() -> &'static str {
    "LOCAL_STUDIO_LINK_TEST_CA"
}

struct Proc {
    label: String,
    child: Option<Child>,
    log: PathBuf,
}

impl Proc {
    fn spawn(label: &str, mut command: Command, log: PathBuf) -> anyhow::Result<Self> {
        let file = std::fs::File::create(&log)?;
        let child = command.stdout(file.try_clone()?).stderr(file).spawn().with_context(|| format!("start {label}"))?;
        println!("     {label} pid {}", child.id());
        Ok(Self { label: label.to_string(), child: Some(child), log })
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
                bail!("{} {pid} ignored SIGTERM for 15 s and was killed", self.label);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("ok   {} {pid} stopped by PID", self.label);
        Ok(())
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn tail(&self) {
        let log = self.log();
        let lines: Vec<&str> = log.lines().collect();
        eprintln!("{} log tail:\n{}", self.label, lines[lines.len().saturating_sub(25)..].join("\n"));
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            Command::new("kill").args(["-TERM", &child.id().to_string()]).status().ok();
            child.wait().ok();
        }
    }
}

async fn healthy(port: u16, path: &str, what: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
            let ask = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
            stream.write_all(ask.as_bytes()).await?;
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.ok();
            if answer.starts_with("HTTP/1.1 200") {
                return Ok(());
            }
        }
        ensure!(Instant::now() < deadline, "{what} did not answer {path} within 60 s");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct Answer {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn code(&self) -> Option<&str> {
        self.body.pointer("/error/code").and_then(Value::as_str)
    }
}

async fn answer(response: reqwest::Response) -> anyhow::Result<Answer> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response.bytes().await?;
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    Ok(Answer { status, headers, body })
}

fn chat(model: &str, mode: &str) -> String {
    json!({ "model": model, "stream": true, "messages": [{ "role": "user", "content": mode }] }).to_string()
}

async fn read_until(response: &mut reqwest::Response, got: &mut Vec<u8>, want: &str) -> anyhow::Result<()> {
    while find(got, want.as_bytes()).is_none() {
        match tokio::time::timeout(QUICK, response.chunk()).await {
            Ok(Ok(Some(chunk))) => got.extend_from_slice(&chunk),
            Ok(Ok(None)) => bail!("the stream ended before {want:?}: {}", String::from_utf8_lossy(got)),
            Ok(Err(error)) => bail!("the stream broke before {want:?}: {error}"),
            Err(_) => bail!("no {want:?} within {} s: the answer was held back", QUICK.as_secs()),
        }
    }
    Ok(())
}

async fn read_all(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let mut got = Vec::new();
    while let Some(chunk) = tokio::time::timeout(QUICK, response.chunk()).await.context("the stream stalled")?? {
        got.extend_from_slice(&chunk);
    }
    Ok(got)
}

fn linked_peers(controller: &Path) -> anyhow::Result<Value> {
    let config: Value = serde_json::from_slice(&std::fs::read(controller.join("config.json"))?)?;
    Ok(config.get("linkPeers").cloned().unwrap_or(Value::Null))
}

async fn squat(listener: TcpListener, heard: Arc<Mutex<Vec<(String, String)>>>) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else { continue };
        let heard = Arc::clone(&heard);
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            if let Ok(Some((seen, _))) = read_request(&mut socket, &mut buffer).await {
                let authorization = seen.headers.get("authorization").cloned().unwrap_or_default();
                lock(&heard).push((seen.target, authorization));
                respond(&mut socket, 503, "application/json", b"{}").await.ok();
            }
        });
    }
}

struct Signal {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Waker for Signal {
    fn wake(&self) {
        *lock(&self.pending) = true;
        self.ready.notify_one();
    }
}

fn forget_in_app(b: &Side, machine: &str) -> anyhow::Result<()> {
    let signal = Arc::new(Signal { pending: Mutex::new(false), ready: Condvar::new() });
    let config = StudioConfig {
        socket_path: Some(b.root.join("d.sock").to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: Some(b.data_arg()),
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let studio =
        Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(|error| anyhow::anyhow!("{error}"))?;
    let checked = (|| {
        let deadline = Instant::now() + QUICK;
        let mut listed = false;
        while !listed {
            for update in studio.tick() {
                if let Update::Machines { machines } = update {
                    listed = machines.iter().any(|row| row.id == machine);
                }
            }
            ensure!(Instant::now() < deadline, "the app on B never listed {machine}");
            std::thread::sleep(Duration::from_millis(100));
        }
        studio.forget_machine(machine.to_string());
        let deadline = Instant::now() + QUICK;
        loop {
            let peers = linked_peers(&b.controller)?;
            if peers == json!([]) || peers.is_null() {
                break;
            }
            ensure!(Instant::now() < deadline, "forget_machine left linkPeers {peers}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let links: Value = serde_json::from_slice(&std::fs::read(b.data.join("remote").join("links.json"))?)?;
        let kept = links["machines"].as_array().map(|rows| rows.iter().any(|row| row["id"] == machine));
        ensure!(kept == Some(false), "links.json still holds {machine} after forget_machine");
        ensure!(
            !b.data.join("remote").join("fleet-proxy-token").exists(),
            "the proxy token outlived the last fleet link"
        );
        let config: Value = serde_json::from_slice(&std::fs::read(b.controller.join("config.json"))?)?;
        ensure!(config.get("linkToken").is_none(), "B's controller kept the proxy token after the last link peer left");
        let deadline = Instant::now() + QUICK;
        while std::net::TcpListener::bind(("127.0.0.1", PORT)).is_err() {
            ensure!(
                Instant::now() < deadline,
                "the proxy kept 127.0.0.1:{PORT} after its last fleet link was forgotten"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::Ok(())
    })();
    studio.shutdown();
    checked
}

struct World {
    a: Side,
    b: Side,
    serve_bin: PathBuf,
    studio_bin: PathBuf,
    a_port: u16,
    b_port: u16,
    b_url: String,
    key_a: String,
    key_b: String,
    stub_a: Arc<Stub>,
    stub_c: Arc<Stub>,
    tls: PathBuf,
}

#[allow(clippy::too_many_lines)]
async fn stages(world: &World, a_serve: &Proc, b_serve: &mut Option<Proc>) -> anyhow::Result<()> {
    let World { a, b, serve_bin, studio_bin, a_port, b_port, b_url, key_a, key_b, stub_a, stub_c, tls } = world;
    let http = reqwest::Client::builder().no_proxy().build()?;
    let (term_port, term) = terminator(tls, *a_port).await?;
    let wss = format!("wss://localhost:{term_port}/v1/ws");
    a.ok(serve_bin, &["allow-host", "localhost", "--data-dir", &a.data_arg()])?;

    let invited: Value = serde_json::from_str(a.ok(studio_bin, &["fleet", "invite", "--url", &wss, "--json"])?.trim())?;
    ensure!(invited["state"] == "minted" && invited["scopes"] == json!(["fleet"]), "invite printed {invited}");
    let link = invited["link"].as_str().context("the invite has no link")?.to_string();
    println!("ok   A: local-studio fleet invite --url {wss} minted a fleet pairing");

    let early = b.run(studio_bin, &["fleet", "join", &link, "--json"])?;
    let printed = String::from_utf8_lossy(&early.stdout);
    ensure!(
        !early.status.success() && printed.contains("Nothing owns"),
        "fleet join with no owner of B's data dir did not refuse: {printed}"
    );
    println!("ok   B: fleet join refuses while nothing owns the data dir, and the invite is left unused");

    let log = b.root.join("serve.log");
    let mut command = Command::new(serve_bin);
    b.isolate(&mut command);
    command.args(["serve", "--port", &b_port.to_string(), "--name", "fleet-b", "--data-dir", &b.data_arg()]);
    *b_serve = Some(Proc::spawn("B serve", command, log)?);
    healthy(*b_port, "/v1/health", "B serve").await?;

    let started = Instant::now();
    let joined: Value = serde_json::from_str(b.ok(studio_bin, &["fleet", "join", &link, "--json"])?.trim())?;
    let ledger: Value = serde_json::from_slice(&std::fs::read(a.data.join("remote").join("devices.json"))?)?;
    let machine = ledger["machineId"].as_str().context("A's devices.json has no machine id")?.to_string();
    let peer = proxy::peer_url(&machine);
    ensure!(joined["state"] == "joined", "fleet join printed {joined}");
    ensure!(joined["machine"]["id"] == machine.as_str(), "fleet join named {} not A's {machine}", joined["machine"]);
    ensure!(joined["peer"] == peer.as_str(), "fleet join added {} not {peer}", joined["peer"]);
    ensure!(
        linked_peers(&b.controller)? == json!([peer]),
        "B's controller holds linkPeers {}",
        linked_peers(&b.controller)?
    );
    let config: Value = serde_json::from_slice(&std::fs::read(b.controller.join("config.json"))?)?;
    ensure!(!config["peers"].to_string().contains(&machine), "the link peer leaked into B's tailnet peers");
    let links: Value = serde_json::from_slice(&std::fs::read(b.data.join("remote").join("links.json"))?)?;
    let row = links["machines"].as_array().and_then(|rows| rows.iter().find(|row| row["id"] == machine.as_str()));
    ensure!(row.is_some_and(|row| row["fleet"] == true), "B's links.json does not mark A as a fleet link");
    let device = ledger["devices"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["scopes"] == json!(["fleet"])))
        .and_then(|row| row["id"].as_str())
        .context("A's devices.json has no fleet-scoped device")?
        .to_string();
    let token_file = b.data.join("remote").join("fleet-proxy-token");
    let token = std::fs::read_to_string(&token_file).context("B has no fleet proxy token")?.trim().to_string();
    ensure!(token.len() == 64 && token != *key_b, "B's proxy token is not 32 random bytes in hex");
    ensure!(config["linkToken"] == token.as_str(), "B's controller does not hold the proxy token next to linkPeers");
    for file in [&token_file, &b.controller.join("config.json")] {
        let mode = std::fs::metadata(file)?.permissions().mode() & 0o777;
        ensure!(mode == 0o600, "{} has mode {mode:o}", file.display());
    }
    println!(
        "ok   B: fleet join over {wss} paired in {} ms, marked A as a fleet link and added {peer} as a link peer only",
        started.elapsed().as_millis()
    );

    let models = answer(http.get(format!("{b_url}/v1/models")).bearer_auth(key_b).timeout(QUICK).send().await?).await?;
    let listed = models.body["data"].as_array().cloned().unwrap_or_default();
    let owner = listed.iter().find(|row| row["id"] == "a-model").map(|row| row["owned_by"].clone());
    ensure!(owner == Some(json!("stub-a")), "B's gateway lists {}", models.body);
    let reads = stub_a.seen();
    let read = reads.iter().find(|seen| seen.target == "/api/node").context("A's controller saw no node read")?;
    ensure!(read.headers.get("authorization") == Some(&format!("Bearer {key_a}")), "A's node read had the wrong key");
    println!("ok   B's gateway lists A's a-model, read through the proxy and A's serve with A's own key");

    let before = stub_a.count();
    let mut response = http
        .post(format!("{b_url}/v1/chat/completions"))
        .bearer_auth(key_b)
        .header("content-type", "application/json")
        .body(chat("a-model", "gate"))
        .timeout(QUICK)
        .send()
        .await?;
    ensure!(response.status() == 200, "the chat through B answered {}", response.status());
    let served = response.headers().get("x-local-studio-endpoint").and_then(|value| value.to_str().ok());
    ensure!(served == Some("stub-a:9000"), "the chat was served by {served:?}");
    ensure!(response.headers().get("x-local-studio-failover").is_none(), "the chat failed over");
    let mut got = Vec::new();
    read_until(&mut response, &mut got, "alpha").await?;
    ensure!(find(&got, b"beta").is_none(), "the gate leaked: {}", String::from_utf8_lossy(&got));
    stub_a.gate.notify_one();
    got.extend(read_all(response).await?);
    ensure!(got == EVENTS.concat().as_bytes(), "the stream arrived changed: {}", String::from_utf8_lossy(&got));
    let chats: Vec<Seen> = stub_a.served().into_iter().filter(|seen| seen.target == "/v1/chat/completions").collect();
    let last = chats.last().context("A's controller saw no chat")?;
    ensure!(stub_a.count() > before && last.method == "POST", "A's controller saw no new chat: {last:?}");
    ensure!(last.headers.get("x-local-studio-hop").map(String::as_str) == Some("1"), "no hop header at A");
    ensure!(last.headers.get("authorization") == Some(&format!("Bearer {key_a}")), "A's chat had the wrong key");
    ensure!(
        last.headers.get("x-local-studio-endpoint").map(String::as_str) == Some("stub-a:9000"),
        "the pin header did not pass: {:?}",
        last.headers
    );
    ensure!(!format!("{:?}", stub_a.seen()).contains(key_b.as_str()), "B's fleet key reached A's controller");
    println!("ok   a streaming chat to B is served by A through the link: the first event arrived before A sent the rest, hop 1 and the pin header reached A, B's key did not");

    let closed = stub_a.closed();
    let mut response = http
        .post(format!("{b_url}/v1/chat/completions"))
        .bearer_auth(key_b)
        .header("content-type", "application/json")
        .body(chat("a-model", "hang"))
        .timeout(QUICK)
        .send()
        .await?;
    let mut got = Vec::new();
    read_until(&mut response, &mut got, "alpha").await?;
    ensure!(stub_a.closed() == closed, "A's controller connection closed before the client left");
    let left = Instant::now();
    drop(response);
    stub_a
        .until("closing the client mid-stream closes A's controller connection", |stub| stub.closed() > closed)
        .await?;
    println!("ok   closing B's client mid-stream cancelled A's request in {} ms", left.elapsed().as_millis());

    let count = stub_a.count();
    let proxied = |method: reqwest::Method, path: &str| http.request(method, format!("{peer}{path}")).timeout(QUICK);
    let cluster =
        answer(proxied(reqwest::Method::POST, "/api/cluster").bearer_auth(&token).body("{}").send().await?).await?;
    ensure!(cluster.status == 403 && cluster.code() == Some("forbidden"), "POST /api/cluster gave {}", cluster.body);
    let snapshot = answer(proxied(reqwest::Method::GET, "/api/snapshot").bearer_auth(&token).send().await?).await?;
    ensure!(snapshot.status == 403 && snapshot.code() == Some("forbidden"), "GET /api/snapshot gave {}", snapshot.body);
    ensure!(stub_a.count() == count, "a refused path reached A's controller: {:?}", &stub_a.seen()[count..]);
    println!("ok   POST /api/cluster and GET /api/snapshot through B's proxy are refused by A's serve with forbidden");

    let shapes: [(&str, Option<String>); 7] = [
        ("no authorization", None),
        ("a wrong bearer", Some("Bearer wrong".to_string())),
        ("A's own key", Some(format!("Bearer {key_a}"))),
        ("B's own fleet key", Some(format!("Bearer {key_b}"))),
        ("the token with a suffix", Some(format!("Bearer {token}x"))),
        ("the token cut short", Some(format!("Bearer {}", &token[..token.len() - 1]))),
        ("the token as Basic", Some(format!("Basic {token}"))),
    ];
    for (shape, header) in shapes {
        let mut asked = proxied(reqwest::Method::GET, "/v1/models");
        if let Some(header) = header {
            asked = asked.header("authorization", header);
        }
        let refused = answer(asked.send().await?).await?;
        ensure!(refused.status == 401, "{shape} got {} {}", refused.status, refused.body);
    }
    ensure!(stub_a.count() == count, "an unauthorized request reached A's controller");
    let unknown = answer(
        http.get(format!("http://127.0.0.1:{PORT}/m/nobody/v1/models"))
            .bearer_auth(&token)
            .timeout(QUICK)
            .send()
            .await?,
    )
    .await?;
    ensure!(
        unknown.status == 404 && unknown.code() == Some("unknown_machine"),
        "an unknown machine gave {}",
        unknown.body
    );
    let passed = answer(
        proxied(reqwest::Method::GET, "/v1/models")
            .bearer_auth(&token)
            .header("cookie", "session=nope")
            .header("x-forwarded-for", "10.0.0.1")
            .header("accept", "application/json")
            .send()
            .await?,
    )
    .await?;
    ensure!(
        passed.status == 200 && passed.body == serde_json::from_str::<Value>(MODELS)?,
        "the keyed request gave {}",
        passed.body
    );
    let seen = stub_a.seen();
    let last = seen.last().context("A's controller saw nothing")?;
    ensure!(stub_a.count() == count + 1 && last.target == "/v1/models", "the keyed request did not reach A");
    ensure!(last.headers.get("accept").map(String::as_str) == Some("application/json"), "accept did not pass");
    for dropped in ["cookie", "x-forwarded-for"] {
        ensure!(!last.headers.contains_key(dropped), "{dropped} passed the proxy");
    }
    println!("ok   a missing, wrong or foreign bearer, B's own fleet key included, gets 401 from B's proxy and never reaches A; the proxy token passes with the allowed headers only");

    let count = stub_a.count();
    let over = answer(
        proxied(reqwest::Method::POST, "/v1/completions")
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(vec![b'x'; FLEET_BODY + 1])
            .send()
            .await?,
    )
    .await?;
    ensure!(
        over.status == 413 && over.code() == Some("too_large"),
        "a body one byte over the limit gave {}",
        over.status
    );
    ensure!(stub_a.count() == count, "the oversized body reached A");
    let full = answer(
        proxied(reqwest::Method::POST, "/v1/completions")
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(vec![b'x'; FLEET_BODY])
            .send()
            .await?,
    )
    .await?;
    let seen = stub_a.seen();
    let last = seen.last().context("A's controller saw nothing")?;
    ensure!(
        full.status == 404 && stub_a.count() == count + 1,
        "a body at the limit gave {} {}",
        full.status,
        full.body
    );
    ensure!(last.target == "/v1/completions" && last.length == FLEET_BODY, "A saw a body of {} bytes", last.length);
    println!(
        "ok   a body of {} bytes is answered 413 by B's proxy; one of {FLEET_BODY} bytes reaches A whole",
        FLEET_BODY + 1
    );

    let closed = stub_a.closed();
    let mut held = Vec::new();
    for _ in 0..8 {
        let opened = proxied(reqwest::Method::POST, "/v1/chat/completions")
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(chat("a-model", "hang"))
            .send()
            .await?;
        ensure!(opened.status() == 200, "a held stream through the proxy opened with {}", opened.status());
        held.push(opened);
    }
    let ninth = answer(
        proxied(reqwest::Method::POST, "/v1/chat/completions")
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(chat("a-model", "hang"))
            .send()
            .await?,
    )
    .await?;
    ensure!(ninth.status == 429, "a ninth stream through the proxy gave {} {}", ninth.status, ninth.body);
    drop(held);
    stub_a.until("eight held streams closed when their clients left", |stub| stub.closed() >= closed + 8).await?;
    println!("ok   eight streams through the proxy are served and serve's 429 for a ninth passes through");

    let minted: Value = serde_json::from_str(
        a.ok(
            serve_bin,
            &["pair", "--json", "--url", &format!("ws://127.0.0.1:{a_port}/v1/ws"), "--data-dir", &a.data_arg()],
        )?
        .trim(),
    )?;
    ensure!(minted.get("scopes").is_none(), "an ordinary pairing carries scopes: {minted}");
    let mut phone = pair_phone("plain-phone", *a_port, minted["link"].as_str().context("no link")?).await?;
    let count = stub_a.count();
    phone.refused("fleet_http", &json!({ "method": "GET", "path": "/v1/models" }), "forbidden").await?;
    phone
        .refused(
            "fleet_http",
            &json!({ "method": "POST", "path": "/v1/chat/completions", "body": "e30=", "stream": true }),
            "forbidden",
        )
        .await?;
    ensure!(stub_a.count() == count, "the plain phone reached A's controller");
    ensure!(phone.view().fleet == 0, "the plain phone received fleet updates");
    println!("ok   an ordinarily paired phone gets forbidden from fleet_http on A");

    let readded = http
        .post(format!("{b_url}/api/peers"))
        .bearer_auth(key_b)
        .json(&json!({ "url": peer, "via": "link" }))
        .timeout(QUICK)
        .send()
        .await?;
    ensure!(readded.status() == 200, "re-adding the link peer gave {}", readded.status());
    let walked = Instant::now();
    let models = answer(http.get(format!("{b_url}/v1/models")).bearer_auth(key_b).timeout(QUICK).send().await?).await?;
    ensure!(models.body.to_string().contains("a-model"), "B lost A before the revoke: {}", models.body);
    let count = stub_a.count();
    let served_c = stub_c.served().len();
    a.ok(serve_bin, &["revoke", &device, "--data-dir", &a.data_arg()])?;
    let closing = format!("device {device} was revoked; closing its socket");
    let deadline = Instant::now() + QUICK;
    while !a_serve.log().contains(&closing) {
        ensure!(Instant::now() < deadline, "A's serve never closed B's socket after the revoke");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    ensure!(
        walked.elapsed() < Duration::from_millis(4500),
        "the revoke took too long to test against B's cached graph"
    );
    let response = http
        .post(format!("{b_url}/v1/chat/completions"))
        .bearer_auth(key_b)
        .header("content-type", "application/json")
        .body(chat("shared-model", "after revoke"))
        .timeout(QUICK)
        .send()
        .await?;
    let status = response.status();
    let failover = response.headers().get("x-local-studio-failover").and_then(|v| v.to_str().ok()).map(str::to_string);
    let served = response.headers().get("x-local-studio-endpoint").and_then(|v| v.to_str().ok()).map(str::to_string);
    let got = read_all(response).await?;
    ensure!(status == 200, "the first chat after the revoke gave {status}: {}", String::from_utf8_lossy(&got));
    ensure!(failover.as_deref() == Some("1"), "the first chat after the revoke did not fail over once: {failover:?}");
    ensure!(served.as_deref() == Some("stub-c:9001"), "the first chat after the revoke was served by {served:?}");
    ensure!(got == EVENTS.concat().as_bytes(), "C's stream arrived changed");
    ensure!(stub_c.served().len() == served_c + 1, "C did not serve the chat");
    ensure!(stub_a.count() == count, "a request reached A after the revoke");
    let started = Instant::now();
    let down = answer(proxied(reqwest::Method::GET, "/v1/models").bearer_auth(&token).send().await?).await?;
    let took = started.elapsed();
    ensure!(down.status == 503 && down.code() == Some("link_down"), "the proxy after the revoke gave {}", down.body);
    ensure!(down.header("x-local-studio-retry") == Some("1"), "the 503 does not say it is safe to retry");
    ensure!(took < Duration::from_secs(1), "the proxy took {} ms to answer 503", took.as_millis());
    println!(
        "ok   after revoking B on A, B's next chat failed over to C in one try (503 from the proxy) and the proxy answers 503 in {} ms",
        took.as_millis()
    );

    if let Some(mut serve) = b_serve.take() {
        serve.stop()?;
    }
    let squatter = Arc::new(Mutex::new(Vec::new()));
    let squatting = tokio::spawn(squat(TcpListener::bind(("127.0.0.1", PORT)).await?, Arc::clone(&squatter)));
    let readded = http
        .post(format!("{b_url}/api/peers"))
        .bearer_auth(key_b)
        .json(&json!({ "url": peer, "via": "link" }))
        .timeout(QUICK)
        .send()
        .await?;
    ensure!(readded.status() == 200, "re-adding the link peer without a token gave {}", readded.status());
    http.get(format!("{b_url}/v1/models")).bearer_auth(key_b).timeout(QUICK).send().await?.bytes().await?;
    http.post(format!("{b_url}/v1/chat/completions"))
        .bearer_auth(key_b)
        .header("content-type", "application/json")
        .body(chat("a-model", "squatted"))
        .timeout(QUICK)
        .send()
        .await?
        .bytes()
        .await?;
    squatting.abort();
    squatting.await.ok();
    let heard = lock(&squatter).clone();
    ensure!(!heard.is_empty(), "B's controller sent nothing to 127.0.0.1:{PORT} while the proxy was off");
    for (target, authorization) in &heard {
        ensure!(!authorization.contains(key_b.as_str()), "B's controller sent its fleet key to a squatter on {target}");
        ensure!(*authorization == format!("Bearer {token}"), "a squatter saw {target} with another credential");
    }
    println!(
        "ok   with the proxy off, a squatter on port {PORT} saw {} requests from B's controller, all with the proxy token and none with the fleet key",
        heard.len()
    );
    let side = b.clone();
    let id = machine.clone();
    std::thread::spawn(move || forget_in_app(&side, &id))
        .join()
        .map_err(|_| anyhow::anyhow!("the forget check panicked"))??;
    println!("ok   forget_machine on B removes A's link peer, rotates the proxy token away on both sides and frees port {PORT}");

    term.abort();
    let log = a_serve.log();
    ensure!(!log.contains(key_a.as_str()) && !log.contains(key_b.as_str()), "A's serve log holds a fleet key");
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    let usage = "usage: fleet_link_client_smoke <scratch dir>";
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
    ensure!(
        std::net::TcpListener::bind(("127.0.0.1", PORT)).is_ok(),
        "127.0.0.1:{PORT} is taken; the fleet link proxy needs it, so stop whatever holds it and run again"
    );
    let mut a = Side::new(&root, "a")?;
    let mut b = Side::new(&root, "b")?;
    let tls = test_ca(&root)?;
    let busy = listening();
    let linked_port = free_port()?;
    let tailnet_port = free_port()?;
    let b_controller_port = free_port()?;
    let a_port = free_port()?;
    let b_port = free_port()?;
    let b_url = format!("http://127.0.0.1:{b_controller_port}");
    let token =
        || -> anyhow::Result<String> { Ok(hex(&snow::Builder::new(KK.parse()?).generate_keypair()?.public[..16])) };
    let key_a = format!("key-a-{}", token()?);
    let key_b = format!("key-b-{}", token()?);
    std::fs::write(a.controller.join("config.json"), json!({ "fleetKey": key_a }).to_string())?;
    a.set("LOCAL_STUDIO_CONTROLLER_PORT", linked_port.to_string());
    b.set("LOCAL_STUDIO_CONTROLLER_URL", b_url.clone());
    b.set("LOCAL_STUDIO_CONTROLLER_PORT", b_controller_port.to_string());
    b.set(proxy_ca(), tls.join("ca.pem"));
    b.adopt();
    let c_url = format!("http://127.0.0.1:{tailnet_port}");
    let mut exclude = busy;
    exclude.extend([linked_port, tailnet_port, a_port, b_port]);
    std::fs::write(
        b.controller.join("config.json"),
        json!({
            "id": "ctrl-b",
            "name": "Controller B",
            "url": b_url,
            "fleetKey": key_b,
            "peers": [c_url],
            "excludePorts": exclude,
            "engineKeys": {},
            "registry": { "url": "", "dir": b.root.join("registry"), "ref": "none" },
            "modelsDir": b.root.join("models"),
        })
        .to_string(),
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let stub_a = Stub::new(
            "A's controller",
            &key_a,
            &node(
                "stub-a",
                "Stub A",
                "http://10.9.9.9:18091",
                &json!([endpoint("stub-a", 9000, &["a-model", "shared-model"], 0)]),
            ),
        );
        let stub_c = Stub::new(
            "C's controller",
            &key_b,
            &node("stub-c", "Stub C", &c_url, &json!([endpoint("stub-c", 9001, &["shared-model"], 3)])),
        );
        tokio::spawn(stub_serve(TcpListener::bind(("127.0.0.1", linked_port)).await?, Arc::clone(&stub_a)));
        tokio::spawn(stub_serve(TcpListener::bind(("127.0.0.1", tailnet_port)).await?, Arc::clone(&stub_c)));

        let mut command = Command::new(&serve_bin);
        a.isolate(&mut command);
        command.args(["serve", "--port", &a_port.to_string(), "--name", "fleet-a", "--data-dir", &a.data_arg()]);
        let mut a_serve = Proc::spawn("A serve", command, a.root.join("serve.log"))?;
        let mut command = Command::new("bun");
        let main = Path::new(env!("CARGO_MANIFEST_DIR")).join("../controller/src/main.ts");
        b.isolate(&mut command);
        command
            .arg(main)
            .env("LOCAL_STUDIO_CONTROLLER_HOST", "127.0.0.1")
            .env("LOCAL_STUDIO_CONTROLLER_PORT", b_controller_port.to_string());
        let mut b_controller = Proc::spawn("B controller", command, b.root.join("controller.log"))?;
        let mut b_serve = None;
        let world = World { a, b, serve_bin, studio_bin, a_port, b_port, b_url, key_a, key_b, stub_a, stub_c, tls };
        let outcome = async {
            healthy(a_port, "/v1/health", "A serve").await?;
            healthy(b_controller_port, "/api/health", "B controller").await?;
            Box::pin(stages(&world, &a_serve, &mut b_serve)).await
        }
        .await;
        if outcome.is_err() {
            a_serve.tail();
            b_controller.tail();
            if let Some(serve) = &b_serve {
                serve.tail();
            }
        }
        if let Some(mut serve) = b_serve.take() {
            serve.stop()?;
        }
        b_controller.stop()?;
        a_serve.stop()?;
        outcome
    })?;
    println!("fleet_link_client_smoke: every check passed");
    Ok(())
}
