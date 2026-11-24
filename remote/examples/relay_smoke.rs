use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use local_studio_core::link::devices::random;
use local_studio_core::tunnel::relay::{self, Config, Ended, Event};
use local_studio_core::tunnel::Ports;
use local_studio_core::VERSION;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const QUICK: Duration = Duration::from_secs(10);
const LONG_PING: Duration = Duration::from_secs(30);
const NO_REDIAL: Duration = Duration::from_secs(3);

struct Dial {
    stream: TcpStream,
    head: String,
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

    async fn ended(&mut self, within: Duration) -> anyhow::Result<Ended> {
        Ok(timeout(within, &mut self.task).await.context("run did not return")??)
    }
}

type Socket = WebSocketStream<TcpStream>;

struct Smoke {
    key: String,
    listener: TcpListener,
    url: String,
    ports: Ports,
}

impl Smoke {
    async fn new() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self {
            key: URL_SAFE_NO_PAD.encode(random::<32>()?),
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
                    return anyhow::Ok(Dial { stream, head });
                }
            }
        };
        timeout(within, dialed).await.context("the client did not dial")?
    }

    async fn refuse(&self, status: &str) -> anyhow::Result<()> {
        let Dial { mut stream, head } = self.dial(QUICK).await?;
        let mut request = vec![0u8; head.len()];
        stream.read_exact(&mut request).await?;
        let key = &self.key;
        let answer = format!("HTTP/1.1 {status}\r\nx-echo: {key}\r\ncontent-length: {}\r\n\r\n{key}", key.len());
        stream.write_all(answer.as_bytes()).await?;
        Ok(stream.shutdown().await?)
    }

    async fn quiet(&self, within: Duration) -> anyhow::Result<()> {
        ensure!(self.dial(within).await.is_err(), "the client dialed again");
        Ok(())
    }

    async fn greet(&self, stream: TcpStream, version: u64) -> anyhow::Result<Socket> {
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        socket.send(Message::text(hello(version).to_string())).await?;
        Ok(socket)
    }

    async fn connect(&self, ping: Duration) -> anyhow::Result<(Client, Socket)> {
        let mut client = self.client(ping);
        let socket = self.greet(self.dial(QUICK).await?.stream, 1).await?;
        client.connected().await?;
        Ok((client, socket))
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
    let _socket = smoke.greet(dial.stream, 1).await?;
    client.connected().await
}

async fn c2_unauthorized(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    smoke.refuse("401 Unauthorized").await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Unauthorized, "ended {ended:?}");
    smoke.quiet(NO_REDIAL).await
}

async fn replaced(smoke: &Smoke, message: Message) -> anyhow::Result<()> {
    let (mut client, mut socket) = smoke.connect(LONG_PING).await?;
    socket.send(message).await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Replaced, "ended {ended:?}");
    smoke.quiet(NO_REDIAL).await
}

async fn c6_outdated(smoke: &Smoke) -> anyhow::Result<()> {
    let mut client = smoke.client(LONG_PING);
    let mut socket = smoke.greet(smoke.dial(QUICK).await?.stream, 2).await?;
    let ended = client.ended(QUICK).await?;
    ensure!(ended == Ended::Outdated, "ended {ended:?}");
    let close = timeout(QUICK, socket.next()).await.context("the client did not close")?;
    ensure!(matches!(close, Some(Ok(Message::Close(_)))), "the client sent {close:?}");
    smoke.quiet(NO_REDIAL).await
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
    let frame = Message::text(json!({ "v": 1, "t": "replaced" }).to_string());
    step("C4 replaced frame ends Replaced", replaced(smoke, frame)).await?;
    let close = CloseFrame { code: CloseCode::from(4000), reason: "replaced".into() };
    step("C5 close 4000 alone ends Replaced", replaced(smoke, Message::Close(Some(close)))).await?;
    step("C6 hello v2 closes and ends Outdated", c6_outdated(smoke)).await?;
    step("C10 bad config never dials", c10_bad_config(smoke)).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    cases(&Smoke::new().await?).await?;
    println!("relay smoke passed in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}
