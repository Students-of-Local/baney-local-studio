use std::collections::HashMap;
use std::future::Future;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message;

use super::VERSION;
use crate::tunnel::{path_ports, Ports};

const DATA: u8 = 1;
const HEAD: usize = 5;
const CHUNK: usize = 64 << 10;
const UNREACHABLE: &str = "the origin did not answer";
const BAD_ANSWER: &str = "the origin sent a bad answer";
const BROKEN: &str = "the origin's answer broke off";
const NO_BODIES: &str = "request bodies are not carried yet";

struct Out {
    id: u32,
    queue: mpsc::UnboundedSender<Message>,
}

impl Out {
    fn send(&self, message: Message) {
        self.queue.send(message).ok();
    }

    fn control(&self, mut frame: Value) -> Message {
        frame["v"] = VERSION.into();
        frame["id"] = self.id.into();
        Message::text(frame.to_string())
    }

    fn data(&self, kind: u8, payload: &[u8]) -> Message {
        let mut frame = Vec::with_capacity(HEAD + payload.len());
        frame.push(kind);
        frame.extend_from_slice(&self.id.to_be_bytes());
        frame.extend_from_slice(payload);
        Message::binary(frame)
    }

    fn head(&self, status: u16, headers: &[(String, String)]) {
        self.send(self.control(json!({ "t": "res", "status": status, "headers": headers })));
    }

    fn end(&self) {
        self.send(self.control(json!({ "t": "end" })));
    }

    fn plain(&self, status: u16, text: &str, method: &str) {
        self.head(status, &[("content-type".into(), "text/plain; charset=utf-8".into())]);
        if method != "HEAD" {
            self.send(self.data(DATA, text.as_bytes()));
        }
        self.end();
    }
}

#[derive(Deserialize)]
struct Open {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    #[serde(default)]
    body: bool,
    #[serde(default)]
    ws: bool,
}

impl Open {
    fn request(&self) -> Option<Request<Empty<Bytes>>> {
        let mut request = Request::new(Empty::new());
        *request.method_mut() = Method::from_bytes(self.method.as_bytes()).ok()?;
        *request.uri_mut() = self.path.parse().ok()?;
        let headers = request.headers_mut();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(&latin1(name)?).ok()?;
            headers.try_append(name, HeaderValue::from_bytes(&latin1(value)?).ok()?).ok()?;
        }
        Some(request)
    }
}

fn pairs<'a>(headers: impl Iterator<Item = (&'a HeaderName, &'a HeaderValue)>) -> Vec<(String, String)> {
    let text = |value: &HeaderValue| value.as_bytes().iter().map(|byte| char::from(*byte)).collect();
    headers.map(|(name, value)| (name.to_string(), text(value))).collect()
}

fn latin1(text: &str) -> Option<Vec<u8>> {
    text.chars().map(|char| u8::try_from(char).ok()).collect()
}

pub(super) struct Streams {
    routes: Vec<(&'static str, u16)>,
    queue: mpsc::UnboundedSender<Message>,
    open: HashMap<u32, AbortHandle>,
    tasks: JoinSet<()>,
}

impl Streams {
    pub(super) fn new(ports: Ports, queue: mpsc::UnboundedSender<Message>) -> Self {
        Self { routes: path_ports(ports), queue, open: HashMap::new(), tasks: JoinSet::new() }
    }

    pub(super) async fn reap(&mut self) {
        let done = match self.tasks.join_next_with_id().await {
            Some(Ok((id, ()))) => id,
            Some(Err(error)) => error.id(),
            None => return std::future::pending().await,
        };
        self.open.retain(|_, task| task.id() != done);
    }

    pub(super) fn control(&mut self, kind: &str, frame: &Value) {
        let id = frame.get("id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
        let Some(id) = id.filter(|id| *id != 0) else { return };
        if kind == "req" && !self.open.contains_key(&id) {
            let out = Out { id, queue: self.queue.clone() };
            let open = Open::deserialize(frame).ok();
            let task = self.tasks.spawn(answer(out, open, self.routes.clone()));
            self.open.insert(id, task);
        }
    }
}

async fn answer(out: Out, open: Option<Open>, routes: Vec<(&'static str, u16)>) {
    let Some(open) = open else { return out.plain(400, "Bad request.\n", "") };
    let path = open.path.split('?').next().unwrap_or_default();
    let route = routes.iter().find(|(public, _)| *public == path && !open.ws);
    let Some((_, port)) = route else { return out.plain(404, "No local route for this path.\n", &open.method) };
    let Some(request) = open.request() else {
        return out.plain(400, "Bad request.\n", &open.method);
    };
    let sent = if open.body { Err(NO_BODIES) } else { forward(&out, request, *port).await };
    if let Err(why) = sent {
        out.send(out.control(json!({ "t": "error", "message": why })));
    }
}

async fn forward(out: &Out, request: Request<Empty<Bytes>>, port: u16) -> Result<(), &'static str> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.map_err(|_| UNREACHABLE)?;
    stream.set_nodelay(true).map_err(|_| UNREACHABLE)?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.map_err(|_| UNREACHABLE)?;
    let head = request.method() == Method::HEAD;
    let answered = async {
        let response = sender.send_request(request).await.map_err(|_| UNREACHABLE)?;
        respond(out, response, head).await
    };
    drive(answered, connection).await
}

async fn drive<T>(answered: impl Future<Output = T>, connection: impl Future) -> T {
    tokio::pin!(answered);
    tokio::select! {
        done = &mut answered => return done,
        _ = connection => {}
    }
    answered.await
}

async fn respond(out: &Out, response: Response<Incoming>, head: bool) -> Result<(), &'static str> {
    let status = response.status().as_u16();
    if !(200..600).contains(&status) {
        return Err(BAD_ANSWER);
    }
    out.head(status, &pairs(response.headers().iter()));
    let mut body = response.into_body();
    let bare = head || matches!(status, 204 | 205 | 304);
    while let Some(frame) = if bare { None } else { body.frame().await } {
        let Ok(data) = frame.map_err(|_| BROKEN)?.into_data() else { continue };
        for piece in data.chunks(CHUNK) {
            out.send(out.data(DATA, piece));
        }
    }
    out.end();
    Ok(())
}
