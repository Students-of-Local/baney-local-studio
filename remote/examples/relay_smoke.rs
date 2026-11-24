use std::convert::Infallible;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
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
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::WebSocketStream;

const PING: &str = r#"{"v":1,"t":"ping"}"#;
const QUICK: Duration = Duration::from_secs(10);
const LONG_PING: Duration = Duration::from_secs(30);
const NO_REDIAL: Duration = Duration::from_secs(3);
const PUBLIC_HOST: &str = "relay.example.com";
const MARK: &str = "x-relay-smoke";
const BIG: usize = 3_201 * 65_521;
const BLOCK: u32 = 65_521;

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
    sink: Arc<tokio::sync::Mutex<Sender>>,
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
        let socket = tokio_tungstenite::accept_async(stream).await?;
        let (sink, mut stream) = socket.split();
        let link = Self {
            client,
            frames: Arc::default(),
            changed: Arc::default(),
            paused: Arc::default(),
            sink: Arc::new(sink.into()),
        };
        let (seen, log) = (link.clone(), Arc::clone(&smoke.log));
        tokio::spawn(async move {
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
        Ok(link)
    }

    async fn raw(&self, message: Message) -> anyhow::Result<()> {
        Ok(self.sink.lock().await.send(message).await?)
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

    async fn quiet(&self, id: u32) -> anyhow::Result<()> {
        let before = self.count(|_| true);
        sleep(Duration::from_millis(300)).await;
        let late = Answer::read(&lock(&self.frames)[before..], id);
        ensure!(late.data == 0 && !late.ended && late.errors == 0 && late.status == 0, "a stopped stream sent more");
        Ok(())
    }

    async fn cancel(&self, id: u32) -> anyhow::Result<()> {
        self.send(&json!({ "v": 1, "t": "cancel", "id": id, "reason": "client" })).await
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

#[derive(Default)]
struct Origin {
    dials: AtomicUsize,
    hosts: Mutex<Vec<(String, Vec<u8>)>>,
    written: AtomicUsize,
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
                tokio::spawn(serve);
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

    async fn until(&self, what: &str, within: Duration, done: impl Fn(&Self) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + within;
        while !done(self) {
            ensure!(Instant::now() < deadline, "the origin never saw {what}");
            sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }
}

struct Smoke {
    key: String,
    log: Arc<Mutex<File>>,
    listener: TcpListener,
    url: String,
    ports: Ports,
    origin: Arc<Origin>,
}

impl Smoke {
    async fn new(scratch: &PathBuf) -> anyhow::Result<Self> {
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
        })
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

async fn c1_hello(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
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
    ensure!(!Answer::read(&lock(&link.frames), 1).ended, "a canceled stream ended");
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
    let mut kernel = 0;
    let deadline = Instant::now() + QUICK;
    loop {
        sleep(Duration::from_millis(300)).await;
        let now = smoke.kernel_bytes(link)?;
        if now > 0 && now == kernel {
            break;
        }
        ensure!(Instant::now() < deadline, "the link never filled: {kernel} then {now} bytes");
        kernel = now;
    }
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
    step("C1 hello, bearer and User-Agent", c1_hello(smoke)).await?;
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
    linked("H3 only the exact paths reach an origin", smoke, h3_paths).await?;
    linked("H4 events arrive as they are sent", smoke, h4_events).await?;
    linked("H5 cancel mid-answer", smoke, h5_cancel).await?;
    linked("H5 cancel drops what is queued", smoke, h5_queued).await?;
    step("H7 closed origin port", h7_refused(smoke)).await?;
    linked("H9 a slow relay slows the origin", smoke, h9_slow_relay).await?;
    linked("H10 answers with no body", smoke, h10_no_body).await?;
    linked("H11 bad headers are 400", smoke, h11_bad_headers).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let scratch = PathBuf::from(std::env::args_os().nth(1).context("usage: relay_smoke <scratch>")?);
    let started = Instant::now();
    cases(&Smoke::new(&scratch).await?).await?;
    println!("relay smoke passed in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}
