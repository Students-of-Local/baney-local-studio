use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
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
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const PING: &str = r#"{"v":1,"t":"ping"}"#;
const QUICK: Duration = Duration::from_secs(10);
const LONG_PING: Duration = Duration::from_secs(30);
const NO_REDIAL: Duration = Duration::from_secs(3);

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
    frames: Arc<Mutex<Vec<Message>>>,
    changed: Arc<Notify>,
    sink: Arc<tokio::sync::Mutex<Sender>>,
}

impl Link {
    async fn open(smoke: &Smoke, stream: TcpStream, pongs: bool) -> anyhow::Result<Self> {
        let socket = tokio_tungstenite::accept_async(stream).await?;
        let (sink, mut stream) = socket.split();
        let link = Self { frames: Arc::default(), changed: Arc::default(), sink: Arc::new(sink.into()) };
        let (seen, log) = (link.clone(), Arc::clone(&smoke.log));
        tokio::spawn(async move {
            loop {
                let got = match stream.next().await {
                    Some(Ok(message)) => message,
                    Some(Err(_)) | None => Message::Close(None),
                };
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

struct Smoke {
    key: String,
    log: Arc<Mutex<File>>,
    listener: TcpListener,
    url: String,
    ports: Ports,
}

impl Smoke {
    async fn new(scratch: &PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(scratch)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self {
            key: URL_SAFE_NO_PAD.encode(random::<32>()?),
            log: Arc::new(Mutex::new(File::create(scratch.join("frames.log"))?)),
            url: format!("http://{}", listener.local_addr()?),
            listener,
            ports: Ports { serve: 1, mcp: 1 },
        })
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
    let more = |frames: &[Message]| (frames.iter().filter(|got| is_ping(got)).count() >= before + 2).then_some(());
    link.until("two more pings", more).await?;
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
    step("C10 bad config never dials", c10_bad_config(smoke)).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let scratch = PathBuf::from(std::env::args_os().nth(1).context("usage: relay_smoke <scratch>")?);
    let started = Instant::now();
    cases(&Smoke::new(&scratch).await?).await?;
    println!("relay smoke passed in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}
