use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::upgrade::Upgraded;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::access;
use crate::devices::{DeviceInfo, Devices};
use crate::hub::{self, ConnId, Hub, Out};

const SILENCE: Duration = Duration::from_secs(60);
const OPENING: Duration = Duration::from_secs(10);
const OPENING_FRAMES: usize = 4;
const MAX_MESSAGE: usize = 16 << 20;
const DRAIN: Duration = Duration::from_secs(2);

type Socket = WebSocketStream<TokioIo<Upgraded>>;
type Sink = SplitSink<Socket, Message>;
type Stream = SplitStream<Socket>;
type Reply = Response<Full<Bytes>>;

pub struct Server {
    pub hub: Arc<Hub>,
    pub devices: Devices,
    pub allowed: Vec<String>,
}

pub async fn run(listener: TcpListener, server: Arc<Server>, stop: impl Future<Output = ()>) {
    tokio::pin!(stop);
    loop {
        let stream = tokio::select! {
            () = &mut stop => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    eprintln!("local-studio-serve: accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let server = Arc::clone(&server);
                async move { Ok::<_, Infallible>(handle(&server, request)) }
            });
            http1::Builder::new().serve_connection(TokioIo::new(stream), service).with_upgrades().await.ok();
        });
    }
}

fn reply(status: StatusCode, body: &str) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    response
}

fn header(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn forwarded_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("x-forwarded-host")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .rfind(|value| !value.is_empty())
        .map(str::to_string)
}

fn handle(server: &Arc<Server>, mut request: Request<Incoming>) -> Reply {
    let host = header(request.headers(), header::HOST).unwrap_or_default().to_string();
    let forwarded = forwarded_host(request.headers());
    let host_ok = access::host_allowed(&host, &server.allowed)
        && forwarded.as_deref().is_none_or(|forwarded| access::host_allowed(forwarded, &server.allowed));
    if !host_ok {
        eprintln!("local-studio-serve: refused a request for host {host:?} (forwarded {forwarded:?})");
        return reply(StatusCode::FORBIDDEN, "this host is not allowed");
    }
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/v1/health") => {
            let mut response = reply(
                StatusCode::OK,
                &json!({ "ok": true, "protocol": hub::PROTOCOL, "min_protocol": hub::MIN_PROTOCOL }).to_string(),
            );
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            response
        }
        (&Method::GET, "/v1/ws") => upgrade(server, &mut request, &host, forwarded.as_deref()),
        (_, "/v1/health" | "/v1/ws") => reply(StatusCode::METHOD_NOT_ALLOWED, "use GET"),
        _ => reply(StatusCode::NOT_FOUND, "not found"),
    }
}

fn upgrade(server: &Arc<Server>, request: &mut Request<Incoming>, host: &str, forwarded: Option<&str>) -> Reply {
    let headers = request.headers();
    let origin = header(headers, header::ORIGIN);
    if !access::origin_allowed(origin, host, forwarded) {
        eprintln!("local-studio-serve: refused a socket from origin {origin:?} for host {host:?}");
        return reply(StatusCode::FORBIDDEN, "this origin is not allowed");
    }
    let wants_socket = header(headers, header::UPGRADE).is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && header(headers, header::CONNECTION)
            .is_some_and(|value| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("upgrade")))
        && header(headers, header::SEC_WEBSOCKET_VERSION) == Some("13");
    let key = headers.get(header::SEC_WEBSOCKET_KEY).filter(|_| wants_socket);
    let Some(key) = key else { return reply(StatusCode::BAD_REQUEST, "expected a WebSocket upgrade") };
    let accept = derive_accept_key(key.as_bytes());
    let pending = hyper::upgrade::on(request);
    let server = Arc::clone(server);
    tokio::spawn(async move {
        match pending.await {
            Ok(upgraded) => {
                let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE));
                let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
                session(server, socket).await;
            }
            Err(error) => eprintln!("local-studio-serve: the socket upgrade failed: {error}"),
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::UPGRADE, "websocket")
        .header(header::CONNECTION, "Upgrade")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "could not answer the upgrade"))
}

async fn next_text(stream: &mut Stream) -> Option<String> {
    while let Some(message) = stream.next().await {
        match message.ok()? {
            Message::Text(text) => return Some(text.to_string()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

async fn close(sink: &mut Sink, code: CloseCode, reason: &str) {
    sink.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await.ok();
}

async fn say(sink: &mut Sink, frame: &Value) -> bool {
    sink.send(Message::text(frame.to_string())).await.is_ok()
}

struct Hello {
    protocol: u64,
    device: String,
    credential: String,
    watch: Vec<String>,
}

async fn session(server: Arc<Server>, socket: Socket) {
    let (mut sink, mut stream) = socket.split();
    for _ in 0..OPENING_FRAMES {
        let Ok(Some(text)) = tokio::time::timeout(OPENING, next_text(&mut stream)).await else {
            close(&mut sink, CloseCode::Policy, "expected pair or hello").await;
            return;
        };
        let frame: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match frame.get("op").and_then(Value::as_str) {
            Some("pair") => {
                if !pair(&server, &mut sink, &frame).await {
                    return;
                }
            }
            Some("hello") => {
                let theirs = frame.get("protocol").and_then(Value::as_u64).unwrap_or(1);
                let their_min = frame.get("min_protocol").and_then(Value::as_u64).unwrap_or(theirs);
                let protocol = theirs.min(hub::PROTOCOL);
                if protocol < their_min.max(hub::MIN_PROTOCOL) {
                    eprintln!(
                        "local-studio-serve: refused a hello speaking protocols {their_min} to {theirs}; this server speaks {} to {}",
                        hub::MIN_PROTOCOL,
                        hub::PROTOCOL
                    );
                    let incompatible =
                        json!({ "op": "incompatible", "protocol": hub::PROTOCOL, "min_protocol": hub::MIN_PROTOCOL });
                    say(&mut sink, &incompatible).await;
                    close(&mut sink, CloseCode::Protocol, "incompatible protocol").await;
                    return;
                }
                let hello = Hello {
                    protocol,
                    device: frame.get("device").and_then(Value::as_str).unwrap_or_default().to_string(),
                    credential: frame.get("credential").and_then(Value::as_str).unwrap_or_default().to_string(),
                    watch: frame
                        .pointer("/resume/watch")
                        .and_then(Value::as_array)
                        .map(|threads| threads.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default(),
                };
                attached(&server, sink, stream, hello).await;
                return;
            }
            _ => {
                close(&mut sink, CloseCode::Policy, "expected pair or hello").await;
                return;
            }
        }
    }
    close(&mut sink, CloseCode::Policy, "expected hello").await;
}

async fn pair(server: &Server, sink: &mut Sink, frame: &Value) -> bool {
    let secret = frame.get("secret").and_then(Value::as_str).unwrap_or_default();
    let info: DeviceInfo =
        frame.get("device").cloned().and_then(|device| serde_json::from_value(device).ok()).unwrap_or_default();
    match server.devices.pair(secret, info.clone()) {
        Ok(Ok(credential)) => {
            eprintln!("local-studio-serve: paired {} ({})", info.name, info.id);
            say(sink, &json!({ "op": "paired", "credential": credential.reveal(), "machine": server.hub.machine }))
                .await
        }
        Ok(Err(refusal)) => {
            eprintln!("local-studio-serve: refused a pairing from {:?}: {refusal}", info.name);
            say(sink, &json!({ "op": "unpaired", "error": refusal.to_string() })).await;
            close(sink, CloseCode::Policy, "unpaired").await;
            false
        }
        Err(error) => {
            eprintln!("local-studio-serve: pairing failed: {error:#}");
            close(sink, CloseCode::Error, "the server could not pair").await;
            false
        }
    }
}

fn still_paired(devices: &Devices, device: &str, credential: &str) -> bool {
    match devices.check(device, credential) {
        Ok(found) => found.is_some(),
        Err(error) => {
            eprintln!("local-studio-serve: could not read the device ledger: {error:#}");
            false
        }
    }
}

async fn attached(server: &Arc<Server>, mut sink: Sink, mut stream: Stream, hello: Hello) {
    let Some(device) = server.devices.check(&hello.device, &hello.credential).ok().flatten() else {
        eprintln!("local-studio-serve: refused a hello from unknown or revoked device {:?}", hello.device);
        say(&mut sink, &json!({ "op": "unpaired" })).await;
        close(&mut sink, CloseCode::Policy, "unpaired").await;
        return;
    };
    if let Err(error) = server.devices.touch(&device.info.id) {
        eprintln!("local-studio-serve: could not note the device as seen: {error:#}");
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let opening = json!({ "op": "hello", "protocol": hello.protocol, "min_protocol": hub::MIN_PROTOCOL, "machine": server.hub.machine, "device": { "id": device.info.id, "name": device.info.name } });
    let conn = server.hub.join(tx.clone(), |snapshot| {
        let mut text = opening.to_string();
        text.pop();
        text.push_str(",\"snapshot\":[");
        text.push_str(&snapshot.iter().map(AsRef::as_ref).collect::<Vec<&str>>().join(","));
        text.push_str("]}");
        text
    });
    for thread in &hello.watch {
        server.hub.watch(conn, thread);
    }
    let writer = tokio::spawn(write(sink, rx));
    read(server, conn, &tx, &mut stream, &hello).await;
    server.hub.leave(conn);
    drop(tx);
    tokio::time::timeout(DRAIN, writer).await.ok();
}

async fn write(mut sink: Sink, mut rx: mpsc::UnboundedReceiver<Out>) {
    let mut seq: u64 = 0;
    while let Some(out) = rx.recv().await {
        let text = match out {
            Out::Frame(text) => text,
            Out::Update(update) => {
                seq += 1;
                format!("{{\"op\":\"update\",\"seq\":{seq},\"update\":{update}}}")
            }
            Out::Close(reason) => {
                close(&mut sink, CloseCode::Policy, &reason).await;
                return;
            }
        };
        if sink.send(Message::text(text)).await.is_err() {
            return;
        }
    }
    close(&mut sink, CloseCode::Normal, "bye").await;
}

async fn read(server: &Arc<Server>, conn: ConnId, tx: &mpsc::UnboundedSender<Out>, stream: &mut Stream, hello: &Hello) {
    loop {
        let message = match tokio::time::timeout(SILENCE, stream.next()).await {
            Err(_) => {
                tx.send(Out::Close("silent for 60 s".to_string())).ok();
                return;
            }
            Ok(None | Some(Err(_))) => return,
            Ok(Some(Ok(message))) => message,
        };
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => return,
            _ => continue,
        };
        if !still_paired(&server.devices, &hello.device, &hello.credential) {
            eprintln!("local-studio-serve: closed a socket for revoked device {:?}", hello.device);
            tx.send(Out::Frame(json!({ "op": "unpaired" }).to_string())).ok();
            tx.send(Out::Close("unpaired".to_string())).ok();
            return;
        }
        if let Some(reply) = answer(&server.hub, conn, &text) {
            tx.send(Out::Frame(reply.to_string())).ok();
        }
    }
}

fn answer(hub: &Hub, conn: ConnId, text: &str) -> Option<Value> {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        return Some(json!({ "op": "error", "error": "a frame must be a JSON object" }));
    };
    let op = frame.get("op").and_then(Value::as_str).unwrap_or_default();
    if op == "ping" {
        return Some(json!({ "op": "pong" }));
    }
    if op == "pong" {
        return None;
    }
    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    let args = frame.get("args").cloned().unwrap_or_else(|| json!({}));
    let thread = args.get("thread").and_then(Value::as_str);
    let result = match (op, thread) {
        ("watch", Some(thread)) => {
            hub.watch(conn, thread);
            Ok(None)
        }
        ("unwatch", Some(thread)) => {
            hub.unwatch(conn, thread);
            Ok(None)
        }
        ("load_earlier", Some(thread)) => hub
            .load_earlier(
                conn,
                thread,
                args.get("before").and_then(Value::as_u64),
                args.get("limit").and_then(Value::as_u64),
            )
            .map(Some),
        ("watch" | "unwatch" | "load_earlier", None) => Err("args.thread is required".to_string()),
        ("pair" | "hello", _) => Err(format!("{op} is only valid as the first frame")),
        _ => hub.request(conn, op, &args),
    };
    Some(match result {
        Ok(None) => json!({ "re": id, "ok": true }),
        Ok(Some(result)) => json!({ "re": id, "ok": true, "result": result }),
        Err(error) => json!({ "re": id, "error": error }),
    })
}
