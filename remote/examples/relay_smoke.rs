use std::collections::HashMap;
use std::convert::Infallible;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context as Poller, Poll};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming};
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use local_studio_core::link::devices::random;
use local_studio_core::tunnel::relay::{self, Config, Ended, Event};
use local_studio_core::tunnel::Ports;
use local_studio_core::VERSION;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::{accept_async_with_config, WebSocketStream};

#[path = "support/tls.rs"]
mod tls;

const PING: &str = r#"{"v":1,"t":"ping"}"#;
const QUICK: Duration = Duration::from_secs(10);
const TEST_CA: &str = "LOCAL_STUDIO_LINK_TEST_CA";
const LONG_PING: Duration = Duration::from_secs(30);
const NO_REDIAL: Duration = Duration::from_secs(3);
const PUBLIC_HOST: &str = "relay.example.com";
const MARK: &str = "x-relay-smoke";
const BIG: usize = 3_201 * 65_521;
const BLOCK: u32 = 65_521;
const RELAY_TAKES: usize = 32 << 20;
const FRAME_HEAD: usize = 5;

fn limits(most: usize) -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(most)).max_frame_size(Some(most))
}

fn noise(bytes: u32) -> Vec<u8> {
    (0..bytes).map(|at| at.wrapping_mul(2_654_435_761).to_le_bytes()[3]).collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_ping(message: &Message) -> bool {
    matches!(message, Message::Text(text) if text.as_str() == PING)
}

struct Dial {
    stream: TcpStream,
    head: String,
    at: Instant,
}

struct Client {
    events: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<Ended>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Client {
    fn start(url: &str, key: &str, ports: Ports, ping: Duration) -> Self {
        let (sender, events) = mpsc::unbounded_channel();
        let config = Config { url: url.to_string(), key: key.to_string(), ports, ping };
        let run = relay::run(config, move |event| {
            sender.send(event).ok();
        });
        Self { events, task: tokio::spawn(run) }
    }

    async fn connected(&mut self) -> anyhow::Result<()> {
        let event = timeout(QUICK, self.events.recv()).await.context("no Connected event")?;
        ensure!(event == Some(Event::Connected), "expected Connected, got {event:?}");
        Ok(())
    }

    async fn disconnected(&mut self, key: &str) -> anyhow::Result<String> {
        match timeout(QUICK, self.events.recv()).await.context("no Disconnected event")? {
            Some(Event::Disconnected(why)) if !why.contains(key) => Ok(why),
            other => bail!("expected Disconnected without the key, got {other:?}"),
        }
    }

    async fn ended(&mut self, within: Duration) -> anyhow::Result<Ended> {
        Ok(timeout(within, &mut self.task).await.context("run did not return")??)
    }
}

type Sender = SplitSink<WebSocketStream<TcpStream>, Message>;

#[derive(Clone)]
struct Link {
    client: u16,
    frames: Arc<Mutex<Vec<Message>>>,
    changed: Arc<Notify>,
    paused: Arc<AtomicBool>,
    sink: Arc<tokio::sync::Mutex<Option<Sender>>>,
    reader: Arc<Mutex<Option<AbortHandle>>>,
}

#[derive(Default)]
struct Answer {
    status: u64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    data: usize,
    ended: bool,
    errors: usize,
}

impl Answer {
    fn read(frames: &[Message], id: u32) -> Self {
        let mut answer = Self::default();
        for got in frames {
            match got {
                Message::Binary(bytes) if bytes.len() >= 5 && bytes[1..5] == id.to_be_bytes() => {
                    answer.data += 1;
                    answer.body.extend_from_slice(&bytes[5..]);
                }
                Message::Text(text) => answer.control(&serde_json::from_str(text).unwrap_or_default(), id),
                _ => {}
            }
        }
        answer
    }

    fn control(&mut self, frame: &Value, id: u32) {
        match frame["t"].as_str() {
            _ if frame["id"] != id || frame["v"] != 1 => {}
            Some("res") => {
                self.status = frame["status"].as_u64().unwrap_or_default();
                self.headers = serde_json::from_value(frame["headers"].clone()).unwrap_or_default();
            }
            Some("end") => self.ended = true,
            Some("error") => self.errors += 1,
            _ => {}
        }
    }

    fn is(&self, status: u64, body: &str) -> anyhow::Result<()> {
        let ok = self.ended && self.errors == 0 && self.status == status && self.body == body.as_bytes();
        ensure!(ok, "expected {status} {body:?}, got {} {:?}", self.status, String::from_utf8_lossy(&self.body));
        Ok(())
    }
}

impl Link {
    async fn open(smoke: &Smoke, stream: TcpStream, pongs: bool) -> anyhow::Result<Self> {
        let client = stream.peer_addr()?.port();
        let socket = accept_async_with_config(stream, Some(limits(RELAY_TAKES))).await?;
        let (sink, mut stream) = socket.split();
        let link = Self {
            client,
            frames: Arc::default(),
            changed: Arc::default(),
            paused: Arc::default(),
            sink: Arc::new(Some(sink).into()),
            reader: Arc::default(),
        };
        let (seen, log) = (link.clone(), Arc::clone(&smoke.log));
        let reader = tokio::spawn(async move {
            loop {
                while seen.paused.load(Ordering::Acquire) {
                    sleep(Duration::from_millis(5)).await;
                }
                let Some(Ok(got)) = stream.next().await else { return };
                writeln!(lock(&log), "{} bytes {}", got.len(), got.to_text().unwrap_or_default()).ok();
                let (ping, closed) = (is_ping(&got), got.is_close());
                lock(&seen.frames).push(got);
                seen.changed.notify_waiters();
                if ping && pongs {
                    seen.send(&json!({ "v": 1, "t": "pong" })).await.ok();
                }
                if closed {
                    return;
                }
            }
        });
        *lock(&link.reader) = Some(reader.abort_handle());
        Ok(link)
    }

    async fn cut(&self) {
        self.sink.lock().await.take();
        if let Some(reader) = lock(&self.reader).take() {
            reader.abort();
        }
    }

    async fn raw(&self, message: Message) -> anyhow::Result<()> {
        Ok(self.sink.lock().await.as_mut().context("the link is cut")?.send(message).await?)
    }

    async fn open_stream(&self, mut frame: Value, more: &[(&str, &str)]) -> anyhow::Result<()> {
        frame["v"] = json!(1);
        frame["t"] = json!("req");
        frame["headers"] = json!([&[("host", PUBLIC_HOST), (MARK, "1")], more].concat());
        self.send(&frame).await
    }

    async fn req(&self, id: u32, method: &str, path: &str, body: bool) -> anyhow::Result<()> {
        self.open_stream(json!({ "id": id, "method": method, "path": path, "body": body, "ws": false }), &[]).await
    }

    async fn ws(&self, id: u32, path: &str, more: &[(&str, &str)]) -> anyhow::Result<Answer> {
        let frame = json!({ "id": id, "method": "GET", "path": path, "body": false, "ws": true });
        self.open_stream(frame, more).await?;
        self.until(&format!("the answer to {id}"), |frames| {
            Some(Answer::read(frames, id)).filter(|answer| answer.status == 101 || answer.ended || answer.errors > 0)
        })
        .await
    }

    async fn socket(&self, id: u32, path: &str) -> anyhow::Result<()> {
        let answer = self.ws(id, path, &[]).await?;
        ensure!(answer.status == 101 && answer.headers.is_empty(), "{path}: {} {:?}", answer.status, answer.headers);
        Ok(())
    }

    async fn message(&self, id: u32, kind: u8, payload: &[u8]) -> anyhow::Result<()> {
        let mut frame = vec![kind];
        frame.extend(id.to_be_bytes());
        frame.extend(payload);
        self.raw(Message::binary(frame)).await
    }

    async fn echo(&self, id: u32, kind: u8, payload: &[u8]) -> anyhow::Result<()> {
        self.message(id, kind, payload).await?;
        self.echoed(id, kind, payload).await
    }

    async fn echoed(&self, id: u32, kind: u8, payload: &[u8]) -> anyhow::Result<()> {
        let mut wanted = vec![kind];
        wanted.extend(id.to_be_bytes());
        wanted.extend(payload);
        self.until("the echo", |frames| frames.contains(&Message::binary(wanted.clone())).then_some(())).await
    }

    async fn ws_close(&self, id: u32, code: u16, reason: &str) -> anyhow::Result<()> {
        self.until(&format!("ws_close {code}"), |frames| {
            let close = json!({ "v": 1, "t": "ws_close", "id": id, "code": code, "reason": reason });
            let same =
                |got: &Message| got.to_text().is_ok_and(|text| serde_json::from_str(text).ok() == Some(close.clone()));
            frames.iter().any(same).then_some(())
        })
        .await
    }

    async fn quiet(&self, id: u32) -> anyhow::Result<()> {
        let before = self.count(|_| true);
        sleep(Duration::from_millis(300)).await;
        let late = Answer::read(&lock(&self.frames)[before..], id);
        ensure!(late.data == 0 && !late.ended && late.errors == 0 && late.status == 0, "a stopped stream sent more");
        Ok(())
    }

    async fn data(&self, id: u32, payload: &[u8]) -> anyhow::Result<()> {
        self.message(id, 1, payload).await
    }

    async fn end(&self, id: u32) -> anyhow::Result<()> {
        self.send(&json!({ "v": 1, "t": "end", "id": id })).await
    }

    async fn cancel(&self, id: u32) -> anyhow::Result<()> {
        self.send(&json!({ "v": 1, "t": "cancel", "id": id, "reason": "client" })).await
    }

    fn peek(&self, id: u32) -> Answer {
        Answer::read(&lock(&self.frames), id)
    }

    async fn answer(&self, id: u32) -> anyhow::Result<Answer> {
        self.until(&format!("the answer to {id}"), |frames| {
            Some(Answer::read(frames, id)).filter(|answer| answer.ended || answer.errors > 0)
        })
        .await
    }

    async fn get(&self, id: u32, method: &str, path: &str) -> anyhow::Result<Answer> {
        self.req(id, method, path, false).await?;
        self.answer(id).await
    }

    async fn send(&self, frame: &Value) -> anyhow::Result<()> {
        self.raw(Message::text(frame.to_string())).await
    }

    fn count(&self, wanted: impl Fn(&Message) -> bool) -> usize {
        lock(&self.frames).iter().filter(|got| wanted(got)).count()
    }

    fn pings(&self) -> usize {
        self.count(is_ping)
    }

    async fn until<T>(&self, what: &str, find: impl Fn(&[Message]) -> Option<T>) -> anyhow::Result<T> {
        let deadline = Instant::now() + QUICK;
        loop {
            let changed = self.changed.notified();
            if let Some(found) = find(&lock(&self.frames)) {
                return Ok(found);
            }
            let left = deadline.checked_duration_since(Instant::now()).with_context(|| format!("never saw {what}"))?;
            timeout(left.min(Duration::from_millis(200)), changed).await.ok();
        }
    }

    async fn closed(&self) -> anyhow::Result<()> {
        self.until("the close", |frames| frames.iter().any(Message::is_close).then_some(())).await
    }
}

struct Pipe(mpsc::Receiver<Bytes>);

impl Body for Pipe {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Poller<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0.poll_recv(cx).map(|bytes| bytes.map(|bytes| Ok(Frame::data(bytes))))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Upload {
    Whole(Vec<u8>),
    Broken,
}

#[derive(Default)]
struct Origin {
    dials: AtomicUsize,
    hosts: Mutex<Vec<(String, Vec<u8>)>>,
    uploads: Mutex<HashMap<String, Upload>>,
    written: AtomicUsize,
    closes: Mutex<Vec<(u16, String)>>,
    sockets: AtomicUsize,
    left: AtomicBool,
    go: Notify,
}

impl Origin {
    async fn listen(self: &Arc<Self>, name: &'static str) -> anyhow::Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let origin = Arc::clone(self);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                origin.dials.fetch_add(1, Ordering::AcqRel);
                let origin = Arc::clone(&origin);
                let service = service_fn(move |request| Arc::clone(&origin).answer(name, request));
                let serve =
                    http1::Builder::new().title_case_headers(true).serve_connection(TokioIo::new(stream), service);
                tokio::spawn(serve.with_upgrades());
            }
        });
        Ok(port)
    }

    async fn answer(self: Arc<Self>, name: &str, request: Request<Incoming>) -> Result<Response<Pipe>, Infallible> {
        let (sender, pipe) = mpsc::channel(1);
        let mut response = Response::new(Pipe(pipe));
        if !request.headers().contains_key(MARK) {
            self.dials.fetch_sub(1, Ordering::AcqRel);
            return Ok(response);
        }
        let value = |name: &str| request.headers().get(name).map(|value| value.as_bytes().to_vec()).unwrap_or_default();
        lock(&self.hosts).push((String::from_utf8_lossy(&value("host")).into(), value("x-latin")));
        let query = request.uri().query().unwrap_or_default().to_string();
        let (kind, tag) = query.split_once('=').unwrap_or((&query, ""));
        if let Some(key) = request.headers().get("sec-websocket-key").filter(|_| kind != "refuse") {
            let accept =
                HeaderValue::from_str(&derive_accept_key(key.as_bytes())).unwrap_or(HeaderValue::from_static(""));
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let mut set = |name, value| response.headers_mut().insert(name, value);
            set("sec-websocket-accept", accept);
            set("connection", HeaderValue::from_static("upgrade"));
            set("upgrade", HeaderValue::from_static("websocket"));
            if value("sec-websocket-protocol") == b"a, b" && value("sec-websocket-extensions").is_empty() {
                set("sec-websocket-protocol", HeaderValue::from_static("b"));
            }
            let (kind, tag) = (kind.to_string(), tag.parse().unwrap_or_default());
            tokio::spawn(async move { self.socket(request, &kind, tag).await });
            return Ok(response);
        }
        let framing = ["content-length", "transfer-encoding", "expect"]
            .map(|name| String::from_utf8_lossy(&value(name)).into_owned());
        let mut body = request.into_body();
        match kind {
            "cookies" => {
                for cookie in ["a=1", "b=2"] {
                    response.headers_mut().append("set-cookie", HeaderValue::from_static(cookie));
                }
                response
                    .headers_mut()
                    .insert("x-latin", HeaderValue::from_bytes(b"caf\xe9").unwrap_or(HeaderValue::from_static("")));
            }
            "status" => *response.status_mut() = StatusCode::from_bytes(tag.as_bytes()).unwrap_or_default(),
            "refuse" => {
                *response.status_mut() = StatusCode::FORBIDDEN;
                sender.send("no entry".into()).await.ok();
            }
            "upload" | "stall" => {
                let (origin, tag, stall) = (Arc::clone(&self), tag.to_string(), kind == "stall");
                tokio::spawn(async move {
                    if stall {
                        origin.go.notified().await;
                    }
                    let whole = read(&mut body).await;
                    let length = whole.as_ref().map_or(0, Vec::len);
                    lock(&origin.uploads).insert(tag, whole.map_or(Upload::Broken, Upload::Whole));
                    sender.send(length.to_string().into()).await.ok();
                });
            }
            "echo" => {
                let text = read(&mut body).await.unwrap_or_default();
                sender.send(format!("{tag}:{}", String::from_utf8_lossy(&text)).into()).await.ok();
            }
            "framing" => {
                let text = read(&mut body).await.unwrap_or_default();
                let seen = format!("{}|{}", framing.join("|"), String::from_utf8_lossy(&text));
                sender.send(seen.into()).await.ok();
            }
            "events" => {
                tokio::spawn(async move {
                    sender.send("data: 1\n\n".into()).await.ok();
                    self.go.notified().await;
                    sender.send("data: 2\n\n".into()).await.ok();
                });
            }
            "forever" | "big" => {
                let wanted = if kind == "big" { BIG } else { usize::MAX };
                let block = Bytes::from(noise(BLOCK));
                tokio::spawn(async move {
                    let mut sent = 0;
                    while sent < wanted && sender.send(block.clone()).await.is_ok() {
                        sent += block.len();
                        self.written.store(sent, Ordering::Release);
                    }
                    self.left.store(sent < wanted, Ordering::Release);
                });
            }
            "block" => {
                sender.send(noise(BLOCK).into()).await.ok();
            }
            _ => {
                sender.send(format!("ok {name}").into()).await.ok();
            }
        }
        Ok(response)
    }

    async fn socket(&self, request: Request<Incoming>, kind: &str, size: usize) {
        let Ok(upgraded) = hyper::upgrade::on(request).await else { return };
        self.sockets.fetch_add(1, Ordering::AcqRel);
        let limits = Some(limits(64 << 20));
        let mut socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, limits).await;
        if kind == "close" {
            let code = CloseCode::from(u16::try_from(size).unwrap_or_default());
            socket.close(Some(CloseFrame { code, reason: "origin".into() })).await.ok();
        } else if kind == "bare" {
            socket.close(None).await.ok();
        } else if kind == "lost" {
            self.sockets.fetch_sub(1, Ordering::AcqRel);
            return;
        } else if kind == "send" {
            socket.send(Message::binary(vec![7; size])).await.ok();
        } else if kind == "stall" {
            self.go.notified().await;
        }
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Close(Some(frame)) => lock(&self.closes).push((frame.code.into(), frame.reason.to_string())),
                message => socket.send(message).await.unwrap_or_default(),
            }
        }
        self.sockets.fetch_sub(1, Ordering::AcqRel);
    }

    async fn until(&self, what: &str, within: Duration, done: impl Fn(&Self) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + within;
        while !done(self) {
            ensure!(Instant::now() < deadline, "the origin never saw {what}");
            sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }

    async fn uploaded(&self, tag: &str, wanted: &Upload) -> anyhow::Result<()> {
        self.until(&format!("the body of {tag} end"), QUICK, |origin| lock(&origin.uploads).contains_key(tag)).await?;
        ensure!(lock(&self.uploads).get(tag) == Some(wanted), "the origin saw another body for {tag}");
        Ok(())
    }
}

async fn read(body: &mut Incoming) -> Option<Vec<u8>> {
    let mut whole = Vec::new();
    while let Some(frame) = body.frame().await {
        whole.extend_from_slice(&frame.ok()?.into_data().unwrap_or_default());
    }
    Some(whole)
}

struct Smoke {
    key: String,
    log: Arc<Mutex<File>>,
    listener: TcpListener,
    url: String,
    ports: Ports,
    origin: Arc<Origin>,
    tls: PathBuf,
}

impl Smoke {
    async fn new(scratch: &Path, tls: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(scratch)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = Arc::new(Origin::default());
        Ok(Self {
            key: URL_SAFE_NO_PAD.encode(random::<32>()?),
            log: Arc::new(Mutex::new(File::create(scratch.join("frames.log"))?)),
            url: format!("http://{}", listener.local_addr()?),
            listener,
            ports: Ports { serve: origin.listen("serve").await?, mcp: origin.listen("mcp").await? },
            origin,
            tls,
        })
    }

    async fn filled(&self, link: &Link) -> anyhow::Result<usize> {
        let mut kernel = 0;
        let deadline = Instant::now() + QUICK;
        loop {
            sleep(Duration::from_millis(300)).await;
            let now = self.kernel_bytes(link)?;
            if now > 0 && now == kernel {
                return Ok(kernel);
            }
            ensure!(Instant::now() < deadline, "the link never filled: {kernel} then {now} bytes");
            kernel = now;
        }
    }

    fn kernel_bytes(&self, link: &Link) -> anyhow::Result<usize> {
        let (relay, client) = (self.listener.local_addr()?.port(), link.client);
        let table = std::fs::read_to_string("/proc/net/tcp")?;
        let queue = |from: u16, to: u16, at: usize| {
            let row = table.lines().find(|row| {
                let cells: Vec<&str> = row.split_whitespace().collect();
                let ends = |cell: &str, port: u16| cell.ends_with(&format!(":{port:04X}"));
                cells.len() > 4 && ends(cells[1], from) && ends(cells[2], to)
            });
            let bytes = row?.split_whitespace().nth(4)?.split(':').nth(at)?;
            usize::from_str_radix(bytes, 16).ok()
        };
        let sent = queue(client, relay, 0).context("no row for the client's socket")?;
        Ok(sent + queue(relay, client, 1).context("no row for the relay's socket")?)
    }

    async fn terminate(&self) -> anyhow::Result<(u16, JoinHandle<()>)> {
        let acceptor = tls::acceptor(&self.tls)?;
        let (listener, upstream) = (TcpListener::bind("127.0.0.1:0").await?, self.listener.local_addr()?);
        let port = listener.local_addr()?.port();
        let terminator = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let mut secure = acceptor.accept(stream).await.ok()?;
                    let mut plain = TcpStream::connect(upstream).await.ok()?;
                    tokio::io::copy_bidirectional(&mut secure, &mut plain).await.ok()
                });
            }
        });
        Ok((port, terminator))
    }

    fn client(&self, ping: Duration) -> Client {
        Client::start(&self.url, &self.key, self.ports, ping)
    }

    async fn dial(&self, within: Duration) -> anyhow::Result<Dial> {
        let dialed = async {
            loop {
                let (stream, _) = self.listener.accept().await?;
                let at = Instant::now();
                let mut seen = vec![0u8; 8 << 10];
                let head = loop {
                    let read = stream.peek(&mut seen).await?;
                    let head = String::from_utf8_lossy(&seen[..read]).into_owned();
                    if read == 0 || head.contains("\r\n\r\n") {
                        break head;
                    }
                    sleep(Duration::from_millis(5)).await;
                };
                if head.starts_with("GET /_relay/connect HTTP/1.1\r\n") {
                    return anyhow::Ok(Dial { stream, head, at });
                }
            }
        };
        timeout(within, dialed).await.context("the client did not dial")?
    }

    async fn refuse(&self, status: &str) -> anyhow::Result<Instant> {
        let Dial { mut stream, head, at } = self.dial(QUICK).await?;
        let mut request = vec![0u8; head.len()];
        stream.read_exact(&mut request).await?;
        let key = &self.key;
        let answer = format!("HTTP/1.1 {status}\r\nx-echo: {key}\r\ncontent-length: {}\r\n\r\n{key}", key.len());
        stream.write_all(answer.as_bytes()).await?;
        stream.shutdown().await?;
        Ok(at)
    }

    async fn quiet(&self, within: Duration) -> anyhow::Result<()> {
        ensure!(self.dial(within).await.is_err(), "the client dialed again");
        Ok(())
    }

    async fn connect(&self, ping: Duration) -> anyhow::Result<(Client, Link)> {
        let mut client = self.client(ping);
        let link = Link::open(self, self.dial(QUICK).await?.stream, true).await?;
        link.send(&hello(1)).await?;
        client.connected().await?;
        Ok((client, link))
    }
}

fn hello(version: u64) -> Value {
    json!({ "v": version, "t": "hello", "maxBody": 10 << 20, "maxInFlight": 64, "headTimeoutMs": 60_000 })
}

async fn step(name: &str, work: impl Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
    let started = Instant::now();
    work.await.with_context(|| format!("step {name}"))?;
    println!("ok   {name} ({:.1} s)", started.elapsed().as_secs_f32());
    Ok(())
}

async fn c1_hello(smoke: &Smoke, url: &str) -> anyhow::Result<()> {
    let mut client = Client::start(url, &smoke.key, smoke.ports, LONG_PING);
    let dial = smoke.dial(QUICK).await?;
    for wanted in [format!("authorization: Bearer {}", smoke.key), format!("user-agent: LocalStudio/{VERSION} (relay)")]
    {
        ensure!(dial.head.lines().filter(|line| *line == wanted).count() == 1, "the dial lacked one of its headers");
    }
    let link = Link::open(smoke, dial.stream, true).await?;
    link.send(&hello(1)).await?;
    client.connected().await
}

async fn c2_unauthorized(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    smoke.refuse("401 Unauthorized").await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Unauthorized, "ended {ended:?}");
    smoke.quiet(NO_REDIAL).await
}

async fn c3_backoff(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    let mut dials = Vec::new();
    for _ in 0..3 {
        dials.push(smoke.refuse("503 Service Unavailable").await?);
        let why = client.disconnected(&smoke.key).await?;
        ensure!(why.contains("503"), "Disconnected said {why}");
    }
    let dial = smoke.dial(QUICK).await?;
    dials.push(dial.at);
    let link = Link::open(smoke, dial.stream, true).await?;
    link.send(&hello(1)).await?;
    client.connected().await?;
    let gaps: Vec<f32> = dials.windows(2).map(|pair| (pair[1] - pair[0]).as_secs_f32()).collect();
    let floors = [0.5, 1.0, 2.0];
    let grew = gaps.iter().zip(floors).all(|(gap, floor)| *gap >= floor && *gap < floor * 2.0 + 1.0);
    ensure!(grew && gaps[0] < gaps[1] && gaps[1] < gaps[2], "gaps between dials were {gaps:?}");
    Ok(())
}

async fn replaced(smoke: &Smoke, message: Message) -> anyhow::Result<()> {
    let (mut client, link) = smoke.connect(LONG_PING).await?;
    link.raw(message).await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Replaced, "ended {ended:?}");
    smoke.quiet(NO_REDIAL).await
}

async fn c6_outdated(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    let link = Link::open(smoke, smoke.dial(QUICK).await?.stream, true).await?;
    link.send(&hello(2)).await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Outdated, "ended {ended:?}");
    link.closed().await?;
    smoke.quiet(NO_REDIAL).await
}

async fn c7_no_pong(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(Duration::from_millis(200));
    let link = Link::open(smoke, smoke.dial(QUICK).await?.stream, false).await?;
    let greeted = Instant::now();
    link.send(&hello(1)).await?;
    client.connected().await?;
    client.disconnected(&smoke.key).await?;
    let again = smoke.dial(QUICK).await?.at - greeted;
    ensure!(again < Duration::from_secs(2), "the redial took {again:?}");
    ensure!(link.pings() > 0, "no exact ping arrived");
    link.closed().await
}

async fn c8_no_hello(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    let dial = smoke.dial(QUICK).await?;
    let _link = Link::open(smoke, dial.stream, true).await?;
    let again = smoke.dial(Duration::from_secs(13)).await?.at - dial.at;
    ensure!(again >= Duration::from_secs(10), "the redial came after {again:?}");
    client.disconnected(&smoke.key).await?;
    Ok(())
}

async fn c9_ignored(smoke: &Smoke) -> anyhow::Result<()> {
    let (mut client, link) = smoke.connect(Duration::from_millis(500)).await?;
    let mut sent = vec!["", "{", "[]", "null", "7", r#"{"v":1}"#, r#"{"v":1,"t":7}"#, r#"{"v":1,"t":"later"}"#];
    sent.extend([r#"{"t":"replaced"}"#, r#"{"v":2,"t":"replaced"}"#, r#"{"v":2,"t":"cancel","id":5}"#]);
    sent.extend([r#"{"v":1,"t":"end","id":5}"#, r#"{"v":1,"t":"cancel","id":5}"#, r#"{"v":1,"t":"end"}"#]);
    sent.extend([r#"{"v":1,"t":"ws_close","id":5,"code":1000,"reason":""}"#, r#"{"v":1,"t":"cancel","id":"5"}"#]);
    for text in sent {
        link.raw(Message::text(text)).await?;
    }
    for bytes in [&[][..], &[1], &[1, 0, 0, 5], &[9, 0, 0, 0, 5, 1], &[1, 0, 0, 0, 5, 1], &[2, 0, 0, 0, 5, 1]] {
        link.raw(Message::binary(bytes.to_vec())).await?;
    }
    let before = link.pings();
    let more = |frames: &[Message]| (frames.iter().filter(|got| is_ping(got)).count() >= before + 5).then_some(());
    link.until("five more pings", more).await?;
    ensure!(link.count(|got| !is_ping(got)) == 0, "the client answered: {:?}", lock(&link.frames));
    ensure!(client.events.try_recv().is_err() && !client.task.is_finished(), "the client dropped the connection");
    Ok(())
}

async fn c10_bad_config(smoke: &Smoke) -> anyhow::Result<()> {
    let (url, key) = (smoke.url.as_str(), smoke.key.as_str());
    let port = smoke.listener.local_addr()?.port();
    let mut bad = vec![(url.to_string(), key[..42].to_string()), (url.to_string(), format!("{key}A"))];
    bad.push((url.to_string(), format!("{}+", &key[..42])));
    for odd in ["localhost", "192.0.2.1", "relay.example.com", "me@127.0.0.1", "me:x@127.0.0.1"] {
        bad.push((format!("http://{odd}:{port}"), key.to_string()));
    }
    for odd in [format!("{url}/"), format!("{url}/x"), format!("{url}?x"), format!("{url}#x"), format!(" {url}")] {
        bad.push((odd, key.to_string()));
    }
    for odd in ["https://relay.example.com/x", "https://", "ws://127.0.0.1", "relay.example.com", ""] {
        bad.push((odd.to_string(), key.to_string()));
    }
    for (url, key) in bad {
        let ended = Client::start(&url, &key, smoke.ports, LONG_PING).ended(Duration::from_secs(1)).await?;
        ensure!(matches!(&ended, Ended::BadConfig(why) if !why.contains(&key)), "{url} ended {ended:?}");
    }
    smoke.quiet(Duration::from_secs(1)).await
}

async fn h1_headers(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let frame = json!({ "id": 1, "method": "GET", "path": "/mcp?cookies", "body": false, "ws": false });
    link.open_stream(frame, &[("x-latin", "caf\u{e9}")]).await?;
    let answer = link.answer(1).await?;
    answer.is(200, "")?;
    let got = |name: &str| {
        answer.headers.iter().filter(|(seen, _)| seen == name).map(|(_, value)| value.as_str()).collect::<Vec<_>>()
    };
    ensure!(got("set-cookie") == ["a=1", "b=2"] && got("x-latin") == ["caf\u{e9}"], "headers {:?}", answer.headers);
    let lower = answer.headers.iter().all(|(name, _)| *name == name.to_ascii_lowercase());
    ensure!(lower, "a header name kept its case: {:?}", answer.headers);
    let seen = lock(&smoke.origin.hosts).last().cloned();
    ensure!(seen == Some((PUBLIC_HOST.to_string(), b"caf\xe9".to_vec())), "the origin saw {seen:?}");
    Ok(())
}

async fn h2_upload(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let body = noise(5_000_000);
    link.req(1, "POST", "/mcp?upload=whole", true).await?;
    for piece in body.chunks(64 << 10) {
        link.data(1, piece).await?;
    }
    link.end(1).await?;
    link.answer(1).await?.is(200, "5000000")?;
    smoke.origin.uploaded("whole", &Upload::Whole(body)).await
}

async fn h3_paths(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let dials = smoke.origin.dials.load(Ordering::Acquire);
    let unknown = [
        "/x",
        "/",
        "/%6dcp",
        "/v1%2Fhealth",
        "/v1/health/",
        "//mcp",
        "/mcp#x",
        "http://127.0.0.1/mcp",
        "/v1/health/../../mcp",
        "",
    ];
    for (id, path) in (1..).zip(unknown) {
        link.get(id, "GET", path).await?.is(404, "No local route for this path.\n")?;
    }
    let after = smoke.origin.dials.load(Ordering::Acquire);
    ensure!(after == dials, "an unknown path dialed the origin");
    link.get(21, "GET", "/v1/health?x=1").await?.is(200, "ok serve")?;
    link.get(22, "GET", "/.well-known/oauth-authorization-server/mcp").await?.is(200, "ok mcp")
}

async fn h4_events(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    link.req(1, "GET", "/mcp?events", false).await?;
    link.until("event 1", |frames| (Answer::read(frames, 1).body == b"data: 1\n\n").then_some(())).await?;
    let hits = lock(&smoke.origin.hosts).len();
    link.req(1, "GET", "/mcp", false).await?;
    link.get(2, "GET", "/mcp").await?.is(200, "ok mcp")?;
    ensure!(lock(&smoke.origin.hosts).len() == hits + 1, "a second req with an id in flight was served");
    smoke.origin.go.notify_one();
    link.answer(1).await?.is(200, "data: 1\n\ndata: 2\n\n")
}

async fn h5_cancel(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    link.req(1, "GET", "/mcp?forever", false).await?;
    link.until("some of the answer", |frames| (Answer::read(frames, 1).data > 3).then_some(())).await?;
    link.cancel(1).await?;
    let gone = smoke.origin.until("the close", Duration::from_secs(1), |origin| origin.left.load(Ordering::Acquire));
    gone.await?;
    link.get(2, "GET", "/mcp").await?.is(200, "ok mcp")?;
    link.quiet(1).await?;
    ensure!(!link.peek(1).ended, "a canceled stream ended");
    Ok(())
}

async fn h6_broken(smoke: &Smoke) -> anyhow::Result<()> {
    let (_client, link) = smoke.connect(LONG_PING).await?;
    for (id, tag, more) in [
        (1, "cancel", &[][..]),
        (2, "drop", &[]),
        (3, "cancel-sized", &[("content-length", "1000000")]),
        (4, "drop-sized", &[("content-length", "1000000")]),
    ] {
        let hits = lock(&smoke.origin.hosts).len();
        let path = format!("/mcp?upload={tag}");
        link.open_stream(json!({ "id": id, "method": "POST", "path": path, "body": true }), more).await?;
        link.data(id, &noise(BLOCK)).await?;
        let began = smoke.origin.until("the request", QUICK, |origin| lock(&origin.hosts).len() > hits);
        began.await?;
    }
    for (id, tag) in [(1, "cancel"), (3, "cancel-sized")] {
        link.cancel(id).await?;
        smoke.origin.uploaded(tag, &Upload::Broken).await?;
    }
    link.cut().await;
    for tag in ["drop", "drop-sized"] {
        smoke.origin.uploaded(tag, &Upload::Broken).await?;
    }
    Ok(())
}

async fn h5_queued(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        println!("     skipped: the kernel's own buffers are measured on Linux only");
        return Ok(());
    }
    let ids = 1..=240;
    link.paused.store(true, Ordering::Release);
    for id in ids.clone() {
        link.req(id, "GET", "/mcp?block", false).await?;
        sleep(Duration::from_millis(15)).await;
    }
    let kernel = smoke.filled(link).await?;
    let all = ids.clone().count() * BLOCK as usize;
    ensure!(kernel + (2 << 20) < all, "{kernel} bytes fit in the kernel; the queue was never full");
    let before = link.count(|_| true);
    for id in ids.clone() {
        link.cancel(id).await?;
    }
    sleep(Duration::from_millis(200)).await;
    link.paused.store(false, Ordering::Release);
    link.get(241, "GET", "/mcp").await?.is(200, "ok mcp")?;
    let late: usize = lock(&link.frames)[before..].iter().map(Message::len).sum();
    ensure!(late < kernel + (256 << 10), "{late} bytes came after the cancels, {kernel} of them already sent");
    println!("     {late} bytes came after the cancels, {kernel} of them already sent");
    Ok(())
}

async fn h7_refused(smoke: &Smoke) -> anyhow::Result<()> {
    let closed = TcpListener::bind("127.0.0.1:0").await?.local_addr()?.port();
    let mut client = Client::start(&smoke.url, &smoke.key, Ports { mcp: closed, ..smoke.ports }, LONG_PING);
    let link = Link::open(smoke, smoke.dial(QUICK).await?.stream, true).await?;
    link.send(&hello(1)).await?;
    client.connected().await?;
    let answer = link.get(1, "GET", "/mcp").await?;
    ensure!(answer.errors == 1 && answer.status == 0 && !answer.ended, "a closed origin port got {}", answer.status);
    link.get(2, "GET", "/v1/health").await?.is(200, "ok serve")
}

async fn h8_many(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let ids = || (0..64u32).map(|n| u32::MAX - n);
    for id in ids() {
        link.req(id, "POST", &format!("/mcp?echo={id}"), true).await?;
    }
    for part in ["one ", "two ", "three"] {
        for id in ids() {
            link.data(id, part.as_bytes()).await?;
        }
    }
    for id in ids() {
        link.end(id).await?;
    }
    for id in ids() {
        link.answer(id).await?.is(200, &format!("{id}:one two three"))?;
    }
    Ok(())
}

async fn h9_slow_relay(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    smoke.origin.written.store(0, Ordering::Release);
    link.req(1, "GET", "/mcp?big", false).await?;
    link.until("the head", |frames| (Answer::read(frames, 1).status == 200).then_some(())).await?;
    link.paused.store(true, Ordering::Release);
    let before = smoke.origin.written.load(Ordering::Acquire);
    sleep(Duration::from_secs(2)).await;
    let written = smoke.origin.written.load(Ordering::Acquire) - before;
    ensure!(written < 32 << 20, "the origin wrote {written} bytes while the relay read nothing");
    link.paused.store(false, Ordering::Release);
    let (mut at, mut seen) = (0, 0);
    let (period, block) = (BLOCK as usize, noise(BLOCK).repeat(3));
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let frames = lock(&link.frames)[at..].to_vec();
        at += frames.len();
        for got in &frames {
            match got {
                Message::Binary(bytes) => {
                    ensure!(bytes.len() <= 5 + (64 << 10), "a DATA frame held {} bytes", bytes.len() - 5);
                    ensure!(block[seen % period..].starts_with(&bytes[5..]), "the answer differs near byte {seen}");
                    seen += bytes.len() - 5;
                }
                Message::Text(text) if text.contains(r#""t":"end""#) => {
                    ensure!(seen == BIG, "{seen} of {BIG} bytes arrived");
                    println!("     the origin wrote {written} bytes while the relay read nothing");
                    return Ok(());
                }
                _ => {}
            }
        }
        ensure!(Instant::now() < deadline, "{seen} of {BIG} bytes arrived in time");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn h10_no_body(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    for (id, method, path, status) in [
        (1, "HEAD", "/mcp", 200),
        (2, "GET", "/mcp?status=204", 204),
        (3, "GET", "/mcp?status=304", 304),
        (4, "HEAD", "/x", 404),
    ] {
        let answer = link.get(id, method, path).await?;
        answer.is(status, "")?;
        ensure!(answer.data == 0, "{method} {path} sent DATA");
    }
    Ok(())
}

async fn h11_bad_headers(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let dials = smoke.origin.dials.load(Ordering::Acquire);
    for (id, header) in (1..).zip([("x-a", "\u{100}"), ("x-a", "b\nc"), ("x a", "b"), ("x-\u{100}", "b")]) {
        link.open_stream(json!({ "id": id, "method": "GET", "path": "/mcp" }), &[header]).await?;
        link.answer(id).await?.is(400, "Bad request.\n")?;
    }
    link.send(&json!({ "v": 1, "t": "req", "id": 9, "method": "GET", "path": 7 })).await?;
    link.answer(9).await?.is(400, "Bad request.\n")?;
    ensure!(smoke.origin.dials.load(Ordering::Acquire) == dials, "a bad header dialed the origin");
    Ok(())
}

async fn h12_cap(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let ids = [1u32, 2, 3];
    for id in ids {
        link.req(id, "POST", &format!("/mcp?stall=cap{id}"), true).await?;
    }
    let errors = || ids.map(|id| link.peek(id).errors);
    let mut sent = [0usize; 3];
    let block = vec![7; 64 << 10];
    while sent[0] < 24 << 20 {
        link.data(ids[0], &block).await?;
        sent[0] += block.len();
    }
    for round in 100.. {
        let total: usize = sent.iter().sum();
        ensure!(total < 64 << 20, "{total} bytes were taken with no error");
        for at in 0..16 {
            link.data(ids[1 + at % 2], &block).await?;
            sent[1 + at % 2] += block.len();
        }
        link.get(round, "GET", "/v1/health").await?.is(200, "ok serve")?;
        if errors().iter().sum::<usize>() > 0 {
            break;
        }
    }
    let total: usize = sent.iter().sum();
    ensure!(total >= 32 << 20, "the error came after {total} bytes");
    ensure!(errors() == [1, 0, 0], "the stream with the most waiting did not end: errors {:?}", errors());
    smoke.origin.go.notify_waiters();
    for ((id, sent), errors) in ids.into_iter().zip(sent).zip(errors()) {
        if errors == 1 {
            smoke.origin.uploaded(&format!("cap{id}"), &Upload::Broken).await?;
        } else {
            link.end(id).await?;
            link.answer(id).await?.is(200, &sent.to_string())?;
        }
    }
    println!("     one error after {total} bytes");
    Ok(())
}

async fn h13_framing(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let sized = ("Content-Length", "5");
    let mut ids = 1..;
    for (method, body, headers, wanted) in [
        ("DELETE", false, &[][..], "|||"),
        ("OPTIONS", false, &[("content-length", "0")], "|||"),
        ("DELETE", false, &[("transfer-encoding", "chunked")], "|||"),
        ("DELETE", true, &[], "|chunked||hello"),
        ("OPTIONS", true, &[("transfer-encoding", "gzip, chunked")], "|chunked||hello"),
        ("GET", true, &[], "|chunked||hello"),
        ("POST", true, &[("expect", "100-continue")], "|chunked||hello"),
        ("POST", true, &[sized, ("expect", "100-continue")], "5|||hello"),
    ] {
        let id = ids.next().unwrap_or_default();
        let frame = json!({ "id": id, "method": method, "path": "/mcp?framing", "body": body });
        link.open_stream(frame, headers).await?;
        if body {
            link.data(id, b"hello").await?;
            link.end(id).await?;
        }
        let answer = link.answer(id).await?;
        answer.is(200, wanted).with_context(|| format!("{method} {headers:?}"))?;
    }
    let dials = smoke.origin.dials.load(Ordering::Acquire);
    for headers in [
        &[sized, ("content-length", "5")][..],
        &[sized, ("content-length", "6")],
        &[sized, ("Transfer-Encoding", "chunked")],
        &[("content-length", "5, 5")],
        &[("content-length", "-1")],
        &[("content-length", "")],
        &[("content-length", "0000000000000005")],
    ] {
        for body in [false, true] {
            let id = ids.next().unwrap_or_default();
            link.open_stream(json!({ "id": id, "method": "POST", "path": "/mcp", "body": body }), headers).await?;
            let answer = link.answer(id).await?;
            answer.is(400, "Bad request.\n").with_context(|| format!("{headers:?}"))?;
        }
    }
    ensure!(smoke.origin.dials.load(Ordering::Acquire) == dials, "bad framing dialed the origin");
    Ok(())
}

async fn h14_lengths(smoke: &Smoke) -> anyhow::Result<()> {
    let origin = TcpListener::bind("127.0.0.1:0").await?;
    let ports = Ports { mcp: origin.local_addr()?.port(), ..smoke.ports };
    let mut client = Client::start(&smoke.url, &smoke.key, ports, LONG_PING);
    let link = Link::open(smoke, smoke.dial(QUICK).await?.stream, true).await?;
    link.send(&hello(1)).await?;
    client.connected().await?;
    for (id, first, then) in [(1, "hello", Some(b"!")), (2, "hell", None)] {
        let frame = json!({ "id": id, "method": "POST", "path": "/mcp", "body": true });
        link.open_stream(frame, &[("content-length", "5")]).await?;
        link.data(id, first.as_bytes()).await?;
        let mut seen = Vec::new();
        let mut stream = loop {
            let (mut stream, _) = timeout(QUICK, origin.accept()).await??;
            seen.resize(8 << 10, 0);
            let read = timeout(QUICK, stream.read(&mut seen)).await??;
            seen.truncate(read);
            if seen.starts_with(b"POST /mcp ") {
                break stream;
            }
        };
        while !seen.ends_with(format!("\r\n\r\n{first}").as_bytes()) {
            let read = timeout(QUICK, stream.read_buf(&mut seen)).await??;
            ensure!(read > 0, "the origin saw {:?}", String::from_utf8_lossy(&seen));
        }
        match then {
            Some(more) => link.data(id, more).await?,
            None => link.end(id).await?,
        }
        let answer = link.answer(id).await?;
        ensure!(answer.errors == 1 && answer.status == 0, "a wrong length got {}", answer.status);
        let before = seen.len();
        let rest = timeout(QUICK, stream.read_to_end(&mut seen)).await.context("the origin stayed connected")?;
        ensure!(seen.len() == before, "{rest:?}: the origin saw {:?}", String::from_utf8_lossy(&seen));
    }
    Ok(())
}

async fn h15_sync(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        println!("     skipped: the kernel's own buffers are measured on Linux only");
        return Ok(());
    }
    link.paused.store(true, Ordering::Release);
    for id in 1..=240 {
        link.req(id, "GET", "/mcp?forever", false).await?;
    }
    let kernel = smoke.filled(link).await?;
    let before = link.count(|_| true);
    let sync = json!({ "id": 241, "method": "GET", "path": "/_relay/sync", "body": false, "ws": false });
    link.open_stream(sync, &[]).await?;
    sleep(Duration::from_millis(300)).await;
    link.paused.store(false, Ordering::Release);
    link.answer(241).await?.is(404, "No local route for this path.\n")?;
    let is_end = |got: &Message| got.to_text().is_ok_and(|text| text.contains(r#""t":"end""#));
    let frames = lock(&link.frames)[before..].to_vec();
    let ahead: usize = frames.iter().take_while(|got| !is_end(got)).map(Message::len).sum();
    println!("     {ahead} bytes came before the sync answer, {kernel} of them already sent");
    ensure!(ahead < kernel + (3 << 19), "the sync answer waited for {ahead} bytes");
    Ok(())
}

async fn w1_echo(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let more = [("sec-websocket-protocol", "a, b"), ("sec-websocket-extensions", "permessage-deflate")];
    let answer = link.ws(1, "/v1/ws?x=1", &more).await?;
    let chosen = [("sec-websocket-protocol".to_string(), "b".to_string())];
    ensure!(answer.status == 101 && answer.headers == chosen, "{} {:?}", answer.status, answer.headers);
    link.echo(1, 2, "h\u{e9}llo".as_bytes()).await?;
    link.echo(1, 3, &[0, 255, 1]).await?;
    link.echo(1, 2, b"").await
}

async fn w2_refused(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    link.ws(1, "/v1/ws?refuse", &[]).await?;
    link.answer(1).await?.is(403, "no entry")
}

async fn w3_close_codes(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let carried = [1000, 1001, 1003, 1008, 1011, 1012, 1013, 4001];
    for (id, code) in (1..).zip(carried) {
        link.socket(id, &format!("/v1/ws?close={code}")).await?;
        link.ws_close(id, code, "origin").await?;
    }
    for (id, (kind, wanted, reason)) in
        (21..).zip([("close=1002", 1011, "origin"), ("bare", 1000, ""), ("lost", 1011, "")])
    {
        link.socket(id, &format!("/v1/ws?{kind}")).await?;
        link.ws_close(id, wanted, reason).await?;
    }
    let mapped = [(1005, 1000), (1002, 1011), (1006, 1011), (2000, 1011), (5000, 1011), (70000, 1011)];
    for (id, (code, wanted)) in (41..).zip(carried.map(|code| (code.into(), code)).into_iter().chain(mapped)) {
        link.socket(id, "/v1/ws").await?;
        link.send(&json!({ "v": 1, "t": "ws_close", "id": id, "code": code, "reason": "public" })).await?;
        let seen = (wanted, "public".to_string());
        smoke.origin.until("the close", QUICK, |origin| lock(&origin.closes).contains(&seen)).await?;
        lock(&smoke.origin.closes).clear();
        link.quiet(id).await?;
    }
    Ok(())
}

async fn w4_large(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    link.socket(1, "/v1/ws").await?;
    link.echo(1, 3, &noise(17 << 20)).await?;
    link.echo(1, 2, &vec![b'x'; 17 << 20]).await?;
    let most = RELAY_TAKES - FRAME_HEAD;
    link.socket(2, &format!("/v1/ws?send={most}")).await?;
    let whole =
        |got: &Message| got.len() == RELAY_TAKES && got.is_binary() && got.clone().into_data()[..5] == [3, 0, 0, 0, 2];
    link.until("32 MiB frame", |frames| frames.iter().any(whole).then_some(())).await?;
    link.socket(3, &format!("/v1/ws?send={}", most + 1)).await?;
    link.ws_close(3, 1009, "message too big").await?;
    link.echo(1, 2, b"still here").await
}

async fn w5_cancel(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let open = |wanted: usize| move |origin: &Origin| origin.sockets.load(Ordering::Acquire) == wanted;
    smoke.origin.until("the earlier sockets close", QUICK, open(0)).await?;
    link.socket(1, "/v1/ws").await?;
    link.echo(1, 2, b"up").await?;
    link.cancel(1).await?;
    smoke.origin.until("the socket close", Duration::from_secs(1), open(0)).await?;
    link.quiet(1).await
}

async fn w6_paths(_smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    for (id, path) in (1..).zip(["/mcp", "/v1/health", "/v1/ws/"]) {
        link.ws(id, path, &[]).await?;
        link.answer(id).await?.is(404, "No local route for this path.\n")?;
    }
    Ok(())
}

async fn w7_cap(smoke: &Smoke, link: &Link) -> anyhow::Result<()> {
    let (large, small) = (noise(20 << 20), noise(17 << 20));
    for (first, closing) in [(1, false), (3, true)] {
        let second = first + 1;
        for id in [first, second] {
            link.socket(id, "/v1/ws?stall").await?;
        }
        link.message(first, 3, &large).await?;
        if closing {
            let close = json!({ "v": 1, "t": "ws_close", "id": first, "code": 1000, "reason": "" });
            link.send(&close).await?;
        }
        link.message(second, 3, &small).await?;
        link.get(10 + first, "GET", "/v1/health").await?.is(200, "ok serve")?;
        let errors = [first, second].map(|id| link.peek(id).errors);
        ensure!(errors == [usize::from(!closing), 0], "closing {closing}: errors {errors:?}");
        smoke.origin.go.notify_waiters();
        link.echoed(second, 3, &small).await?;
    }
    Ok(())
}

async fn linked(
    name: &str,
    smoke: &Smoke,
    work: impl AsyncFnOnce(&Smoke, &Link) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let (mut client, link) = smoke.connect(LONG_PING).await?;
    step(name, work(smoke, &link)).await?;
    ensure!(
        client.events.try_recv().is_err() && !client.task.is_finished(),
        "{name}: the client dropped the connection"
    );
    ensure!(link.count(Message::is_close) == 0, "{name}: the connection closed");
    Ok(())
}

async fn cases(smoke: &Smoke) -> anyhow::Result<()> {
    step("C1 hello, bearer and User-Agent", c1_hello(smoke, &smoke.url)).await?;
    let (port, terminator) = smoke.terminate().await?;
    step("C1 again over wss", c1_hello(smoke, &format!("https://localhost:{port}"))).await?;
    terminator.abort();
    step("C2 401 ends Unauthorized after one dial", c2_unauthorized(smoke)).await?;
    step("C3 503s back off, then Connected", c3_backoff(smoke)).await?;
    let frame = Message::text(json!({ "v": 1, "t": "replaced" }).to_string());
    step("C4 replaced frame ends Replaced", replaced(smoke, frame)).await?;
    let close = CloseFrame { code: CloseCode::from(4000), reason: "replaced".into() };
    step("C5 close 4000 alone ends Replaced", replaced(smoke, Message::Close(Some(close)))).await?;
    step("C6 hello v2 closes and ends Outdated", c6_outdated(smoke)).await?;
    step("C7 no pong: Disconnected and redial within 2 s", c7_no_pong(smoke)).await?;
    step("C8 no hello: redial after 10 s", c8_no_hello(smoke)).await?;
    step("C9 frames it must ignore", c9_ignored(smoke)).await?;
    step("C10 bad config never dials", c10_bad_config(smoke)).await?;
    linked("H1 headers both ways", smoke, h1_headers).await?;
    linked("H2 5 MB upload arrives whole", smoke, h2_upload).await?;
    linked("H3 only the exact paths reach an origin", smoke, h3_paths).await?;
    linked("H4 events arrive as they are sent", smoke, h4_events).await?;
    linked("H5 cancel mid-answer", smoke, h5_cancel).await?;
    linked("H5 cancel drops what is queued", smoke, h5_queued).await?;
    step("H6 cut uploads are broken bodies", h6_broken(smoke)).await?;
    step("H7 closed origin port", h7_refused(smoke)).await?;
    linked("H8 64 requests at once", smoke, h8_many).await?;
    linked("H9 a slow relay slows the origin", smoke, h9_slow_relay).await?;
    linked("H10 answers with no body", smoke, h10_no_body).await?;
    linked("H11 bad headers are 400", smoke, h11_bad_headers).await?;
    linked("H12 request bytes are capped", smoke, h12_cap).await?;
    linked("H13 framing", smoke, h13_framing).await?;
    step("H14 a body of another length", h14_lengths(smoke)).await?;
    linked("H15 a full queue leaves room for a 404", smoke, h15_sync).await?;
    linked("W1 messages and the subprotocol", smoke, w1_echo).await?;
    linked("W2 a refused upgrade", smoke, w2_refused).await?;
    linked("W3 close codes both ways", smoke, w3_close_codes).await?;
    linked("W4 large messages", smoke, w4_large).await?;
    linked("W5 cancel closes the origin socket", smoke, w5_cancel).await?;
    linked("W6 sockets only on /v1/ws", smoke, w6_paths).await?;
    linked("W7 socket bytes are capped", smoke, w7_cap).await
}

#[tokio::main]
async fn run(scratch: &Path, tls: PathBuf) -> anyhow::Result<()> {
    cases(&Smoke::new(scratch, tls).await?).await
}

fn main() -> anyhow::Result<()> {
    let scratch = PathBuf::from(std::env::args_os().nth(1).context("usage: relay_smoke <scratch>")?);
    let tls = tls::test_ca(&scratch)?;
    std::env::set_var(TEST_CA, tls.join("ca.pem"));
    let started = Instant::now();
    run(&scratch, tls)?;
    println!("relay smoke passed in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}
