use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message;

use super::VERSION;
use crate::tunnel::{path_ports, Ports};

const DATA: u8 = 1;
const HEAD: usize = 5;
const CHUNK: usize = 64 << 10;
const SEND_ROOM: usize = 1 << 20;
const OPEN: u8 = 0;
const DONE: u8 = 1;
const STOPPED: u8 = 2;
const UNREACHABLE: &str = "the origin did not answer";
const BAD_ANSWER: &str = "the origin sent a bad answer";
const BROKEN: &str = "the origin's answer broke off";
const NO_BODIES: &str = "request bodies are not carried yet";
const FAILED: &str = "the request failed";

pub(super) struct Queued {
    state: Arc<AtomicU8>,
    message: Message,
    _room: Option<OwnedSemaphorePermit>,
}

impl Queued {
    pub(super) fn own(message: Message) -> Self {
        Self { state: Arc::new(AtomicU8::new(DONE)), message, _room: None }
    }

    pub(super) fn message(&self) -> Option<Message> {
        (self.state.load(Ordering::Acquire) != STOPPED).then(|| self.message.clone())
    }
}

#[derive(Clone)]
struct Out {
    id: u32,
    state: Arc<AtomicU8>,
    queue: mpsc::UnboundedSender<Queued>,
    room: Arc<Semaphore>,
}

impl Out {
    fn settle(&self, state: u8) -> bool {
        self.state.compare_exchange(OPEN, state, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    fn push(&self, message: Message, room: Option<OwnedSemaphorePermit>) {
        self.queue.send(Queued { state: Arc::clone(&self.state), message, _room: room }).ok();
    }

    async fn room(&self, message: &Message) -> Option<OwnedSemaphorePermit> {
        let cost = u32::try_from(message.len().clamp(1, SEND_ROOM)).ok()?;
        Arc::clone(&self.room).acquire_many_owned(cost).await.ok()
    }

    async fn send(&self, message: Message) {
        let room = self.room(&message).await;
        self.push(message, room);
    }

    async fn last(&self, message: Message) {
        let room = self.room(&message).await;
        if self.settle(DONE) {
            self.push(message, room);
        }
    }

    fn fail(&self, why: &str) {
        if self.settle(STOPPED) {
            self.queue.send(Queued::own(self.control(json!({ "t": "error", "message": why })))).ok();
        }
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

    async fn head(&self, status: u16, headers: &[(String, String)]) {
        self.send(self.control(json!({ "t": "res", "status": status, "headers": headers }))).await;
    }

    async fn end(&self) {
        self.last(self.control(json!({ "t": "end" }))).await;
    }

    async fn plain(&self, status: u16, text: &str, method: &str) {
        self.head(status, &[("content-type".into(), "text/plain; charset=utf-8".into())]).await;
        if method != "HEAD" {
            self.send(self.data(DATA, text.as_bytes())).await;
        }
        self.end().await;
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

struct Entry {
    out: Out,
    task: AbortHandle,
}

pub(super) struct Streams {
    routes: Vec<(&'static str, u16)>,
    queue: mpsc::UnboundedSender<Queued>,
    send_room: Arc<Semaphore>,
    open: HashMap<u32, Entry>,
    tasks: JoinSet<()>,
}

impl Streams {
    pub(super) fn new(ports: Ports, queue: mpsc::UnboundedSender<Queued>) -> Self {
        Self {
            routes: path_ports(ports),
            queue,
            send_room: Arc::new(Semaphore::new(SEND_ROOM)),
            open: HashMap::new(),
            tasks: JoinSet::new(),
        }
    }

    pub(super) async fn reap(&mut self) {
        let done = match self.tasks.join_next_with_id().await {
            Some(Ok((id, ()))) => id,
            Some(Err(error)) => error.id(),
            None => return std::future::pending().await,
        };
        self.open.retain(|_, entry| {
            let ended = entry.task.id() == done;
            if ended {
                entry.out.fail(FAILED);
            }
            !ended
        });
    }

    pub(super) fn control(&mut self, kind: &str, frame: &Value) {
        let id = frame.get("id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
        let Some(id) = id.filter(|id| *id != 0) else { return };
        match kind {
            "req" => self.start(id, frame),
            "cancel" => {
                if let Some(entry) = self.open.remove(&id) {
                    entry.out.state.store(STOPPED, Ordering::Release);
                    entry.task.abort();
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, id: u32, frame: &Value) {
        if self.open.get(&id).is_some_and(|entry| entry.out.state.load(Ordering::Acquire) == OPEN) {
            return;
        }
        let state = Arc::new(AtomicU8::new(OPEN));
        let out = Out { id, state, queue: self.queue.clone(), room: Arc::clone(&self.send_room) };
        let open = Open::deserialize(frame).ok();
        let task = self.tasks.spawn(answer(out.clone(), open, self.routes.clone()));
        self.open.insert(id, Entry { out, task });
    }
}

async fn answer(out: Out, open: Option<Open>, routes: Vec<(&'static str, u16)>) {
    let Some(open) = open else { return out.plain(400, "Bad request.\n", "").await };
    let path = open.path.split('?').next().unwrap_or_default();
    let route = routes.iter().find(|(public, _)| *public == path && !open.ws);
    let Some((_, port)) = route else { return out.plain(404, "No local route for this path.\n", &open.method).await };
    let Some(request) = open.request() else {
        return out.plain(400, "Bad request.\n", &open.method).await;
    };
    let sent = if open.body { Err(NO_BODIES) } else { forward(&out, request, *port).await };
    if let Err(why) = sent {
        out.last(out.control(json!({ "t": "error", "message": why }))).await;
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
    out.head(status, &pairs(response.headers().iter())).await;
    let mut body = response.into_body();
    let bare = head || matches!(status, 204 | 205 | 304);
    while let Some(frame) = if bare { None } else { body.frame().await } {
        let Ok(data) = frame.map_err(|_| BROKEN)?.into_data() else { continue };
        for piece in data.chunks(CHUNK) {
            out.send(out.data(DATA, piece)).await;
        }
    }
    out.end().await;
    Ok(())
}
