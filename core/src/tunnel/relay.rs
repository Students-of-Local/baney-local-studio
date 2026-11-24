use std::net::IpAddr;
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{interval_at, sleep, sleep_until, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, USER_AGENT};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream};

use self::streams::{Queued, Streams};
use super::Ports;
use crate::link::client::tls;
use crate::link::devices::random;

mod service;
mod socket;
mod streams;

pub use service::{WORKER_CONFIG, WORKER_JS};

const VERSION: u64 = 1;
const KEY_CHARS: usize = 43;
const CONNECT_PATH: &str = "/_relay/connect";
const PING: &str = r#"{"v":1,"t":"ping"}"#;
const REPLACED: u16 = 4000;
const CONNECT: Duration = Duration::from_secs(10);
const HELLO: Duration = Duration::from_secs(10);
const STEADY: Duration = Duration::from_secs(60);
const HANG_UP: Duration = Duration::from_millis(500);
const FIRST_WAIT_MS: u64 = 1_000;
const LONGEST_WAIT_MS: u64 = 30_000;
const MAX_MESSAGE: usize = 40 << 20;
const RELAY_TAKES: usize = 32 << 20;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Message>;

pub struct Config {
    pub url: String,
    pub key: String,
    pub ports: Ports,
    pub ping: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Connected,
    Disconnected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    Replaced,
    Unauthorized,
    Outdated,
    BadConfig(String),
}

enum Stop {
    Ended(Ended),
    Lost(String),
}

pub async fn run(config: Config, on: impl Fn(Event) + Send + Sync) -> Ended {
    let address = match address(&config) {
        Ok(address) => address,
        Err(message) => return Ended::BadConfig(message.to_string()),
    };
    let mut attempt = 0u32;
    loop {
        let mut up = None;
        let stop = match dial(&address, &config.key).await {
            Ok(socket) => session(socket, &config, &on, &mut up).await,
            Err(stop) => stop,
        };
        match stop {
            Stop::Ended(ended) => return ended,
            Stop::Lost(why) => on(Event::Disconnected(why.replace(&config.key, "[key]"))),
        }
        if up.is_some_and(|up: Instant| up.elapsed() >= STEADY) {
            attempt = 0;
        }
        sleep(backoff(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

fn address(config: &Config) -> Result<String, &'static str> {
    let bad = "the relay address must be https://<host> with no path";
    let url = Url::parse(&config.url).map_err(|_| bad)?;
    let bare = url.as_str().strip_suffix('/').is_some_and(|text| text.eq_ignore_ascii_case(&config.url));
    let host = url.host_str().unwrap_or_default();
    let loopback = || host.trim_matches(['[', ']']).parse().is_ok_and(|ip: IpAddr| ip.is_loopback());
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" if loopback() => "ws",
        _ => return Err(bad),
    };
    if !bare || host.is_empty() || !url.username().is_empty() || url.password().is_some() {
        return Err(bad);
    }
    let key = config.key.as_bytes();
    if key.len() != KEY_CHARS || !key.iter().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(byte)) {
        return Err("the relay key must be 43 characters of base64url");
    }
    let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
    Ok(format!("{scheme}://{host}{port}{CONNECT_PATH}"))
}

fn backoff(attempt: u32) -> Duration {
    let half = FIRST_WAIT_MS.saturating_mul(1 << attempt.min(16)).min(LONGEST_WAIT_MS) / 2;
    let roll = random::<8>().map_or(half, u64::from_le_bytes);
    Duration::from_millis(half + roll % (half + 1))
}

fn limits(most: usize) -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(most)).max_frame_size(Some(most))
}

async fn dial(address: &str, key: &str) -> Result<Socket, Stop> {
    let lost = |error: &dyn std::fmt::Display| Stop::Lost(format!("could not reach the relay: {error}"));
    let mut request = address.into_client_request().map_err(|error| lost(&error))?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|error| lost(&error))?;
    bearer.set_sensitive(true);
    let agent = HeaderValue::from_str(&format!("LocalStudio/{} (relay)", crate::VERSION));
    request.headers_mut().insert(AUTHORIZATION, bearer);
    request.headers_mut().insert(USER_AGENT, agent.map_err(|error| lost(&error))?);
    let connector = if address.starts_with("wss://") {
        Connector::Rustls(tls().map_err(|error| lost(&error))?)
    } else {
        Connector::Plain
    };
    let connect = connect_async_tls_with_config(request, Some(limits(MAX_MESSAGE)), true, Some(connector));
    match timeout(CONNECT, connect).await {
        Err(_) => Err(Stop::Lost("the relay did not answer within 10 s".into())),
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(tungstenite::Error::Http(answer))) if answer.status() == StatusCode::UNAUTHORIZED => {
            Err(Stop::Ended(Ended::Unauthorized))
        }
        Ok(Err(tungstenite::Error::Http(answer))) => {
            Err(Stop::Lost(format!("the relay answered {}", answer.status().as_u16())))
        }
        Ok(Err(error)) => Err(lost(&error)),
    }
}

async fn write(mut sink: Sink, mut queue: mpsc::UnboundedReceiver<Queued>, ping: Duration) {
    let mut pings = interval_at(Instant::now() + ping, ping);
    pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let queued = tokio::select! {
            biased;
            _ = pings.tick() => Queued::own(Message::text(PING)),
            next = queue.recv() => next.unwrap_or_else(|| Queued::own(Message::Close(None))),
        };
        let Some(message) = queued.message() else { continue };
        let last = message.is_close();
        if sink.send(message).await.is_err() || last {
            return;
        }
    }
}

async fn session(
    socket: Socket,
    config: &Config,
    on: &(impl Fn(Event) + Send + Sync),
    up: &mut Option<Instant>,
) -> Stop {
    let ping = config.ping.max(Duration::from_millis(1));
    let (sink, mut stream) = socket.split();
    let (out, queue) = mpsc::unbounded_channel();
    let mut writer = JoinSet::new();
    writer.spawn(write(sink, queue, ping));
    let mut streams = Streams::new(config.ports, out.clone());
    let mut deadline = Instant::now() + HELLO;
    let stop = loop {
        let next = tokio::select! {
            biased;
            () = streams.reap() => continue,
            next = stream.next() => next,
            () = sleep_until(deadline) => {
                break Stop::Lost(match up {
                    Some(_) => "the relay stopped answering pings".into(),
                    None => "the relay sent no hello within 10 s".into(),
                });
            }
            _ = writer.join_next() => return Stop::Lost("the relay connection broke".into()),
        };
        let text = match next {
            None => return Stop::Lost("the relay closed the connection".into()),
            Some(Err(error)) => return Stop::Lost(format!("the relay connection broke: {error}")),
            Some(Ok(Message::Close(frame))) => match frame.map(|frame| u16::from(frame.code)) {
                Some(REPLACED) => return Stop::Ended(Ended::Replaced),
                Some(code) => return Stop::Lost(format!("the relay closed the connection ({code})")),
                None => return Stop::Lost("the relay closed the connection".into()),
            },
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Binary(bytes))) => {
                streams.data(&bytes);
                continue;
            }
            Some(Ok(_)) => continue,
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
        let spoken = frame.get("v").and_then(Value::as_u64) == Some(VERSION);
        match frame.get("t").and_then(Value::as_str) {
            Some("hello") if !spoken => break Stop::Ended(Ended::Outdated),
            Some("hello") if up.is_none() => {
                *up = Some(Instant::now());
                deadline = Instant::now() + 2 * ping;
                on(Event::Connected);
            }
            Some("replaced") if spoken => return Stop::Ended(Ended::Replaced),
            Some("pong") if spoken && up.is_some() => deadline = Instant::now() + 2 * ping,
            Some(kind) if spoken && up.is_some() => streams.control(kind, &frame),
            _ => {}
        }
    };
    drop(streams);
    out.send(Queued::own(Message::Close(None))).ok();
    timeout(HANG_UP, writer.join_next()).await.ok();
    stop
}
