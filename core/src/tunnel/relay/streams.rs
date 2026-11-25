use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{ready, Context, Poll};
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::client::conn::http1;
use hyper::header::{HeaderName, HeaderValue, CONTENT_LENGTH, EXPECT, TRANSFER_ENCODING};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use super::socket::Socket;
use super::VERSION;
use crate::tunnel::{path_ports, Ports};

const DATA: u8 = 1;
pub(super) const WS_TEXT: u8 = 2;
pub(super) const WS_BINARY: u8 = 3;
const WS_PATH: &str = "/v1/ws";
const WS_CLOSE: Duration = Duration::from_secs(5);
pub(super) const HEAD: usize = 5;
const CHUNK: usize = 64 << 10;
const SEND_ROOM: usize = 1 << 20;
const LEAST_COST: usize = 64;
const LENGTH_DIGITS: usize = 15;
const BODY_ROOM: usize = 32 << 20;
const OPEN: u8 = 0;
const DONE: u8 = 1;
const STOPPED: u8 = 2;
const UNREACHABLE: &str = "the origin did not answer";
pub(super) const BAD_ANSWER: &str = "the origin sent a bad answer";
const BROKEN: &str = "the origin's answer broke off";
const TOO_MUCH: &str = "the origin reads too slowly";
const TOO_LONG: &str = "the request body is longer than its content-length";
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
pub(super) struct Out {
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

    async fn room(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        let cost = u32::try_from(bytes.clamp(1, SEND_ROOM)).ok()?;
        Arc::clone(&self.room).acquire_many_owned(cost).await.ok()
    }

    pub(super) async fn send(&self, message: Message) {
        let room = self.room(message.len()).await;
        self.push(message, room);
    }

    pub(super) async fn relay(&self, kind: u8, payload: Bytes) {
        let room = self.room(HEAD + payload.len()).await;
        self.push(self.data(kind, &payload), room);
    }

    pub(super) async fn last(&self, message: Message) {
        let room = self.room(message.len()).await;
        if self.settle(DONE) {
            self.push(message, room);
        }
    }

    fn error(&self, why: &str) {
        if self.settle(DONE) {
            self.push(self.control(json!({ "t": "error", "message": why })), None);
        }
    }

    fn fail(&self, why: &str) {
        if self.settle(STOPPED) {
            self.queue.send(Queued::own(self.control(json!({ "t": "error", "message": why })))).ok();
        }
    }

    pub(super) fn control(&self, mut frame: Value) -> Message {
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

    pub(super) async fn head(&self, status: u16, headers: &[(String, String)]) {
        self.send(self.control(json!({ "t": "res", "status": status, "headers": headers }))).await;
    }

    async fn end(&self) {
        self.last(self.control(json!({ "t": "end" }))).await;
    }

    fn plain(&self, status: u16, text: &str, method: &str) {
        let headers = [["content-type", "text/plain; charset=utf-8"]];
        self.push(self.control(json!({ "t": "res", "status": status, "headers": headers })), None);
        if method != "HEAD" {
            self.push(self.data(DATA, text.as_bytes()), None);
        }
        if self.settle(DONE) {
            self.push(self.control(json!({ "t": "end" })), None);
        }
    }
}

pub(super) enum Feed {
    Data(Bytes),
    Text(Bytes),
    Close(CloseFrame),
    End,
}

type Share = Arc<Mutex<Option<OwnedSemaphorePermit>>>;

struct Held {
    bytes: Vec<u8>,
    cost: usize,
    share: Share,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut share = self.share.lock().unwrap_or_else(PoisonError::into_inner);
        drop(share.as_mut().and_then(|share| share.split(self.cost)));
    }
}

impl AsRef<[u8]> for Held {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

struct Upload(Option<mpsc::UnboundedReceiver<Feed>>);

impl Body for Upload {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let Some(feed) = self.0.as_mut() else { return Poll::Ready(None) };
        Poll::Ready(match ready!(feed.poll_recv(cx)) {
            Some(Feed::Data(bytes)) => Some(Ok(Frame::data(bytes))),
            Some(_) => {
                self.0 = None;
                None
            }
            None => Some(Err(io::Error::other("the request body broke off"))),
        })
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
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
    fn length(&self) -> Result<Option<u64>, ()> {
        let named = |wanted: HeaderName| {
            self.headers.iter().filter(move |(name, _)| name.eq_ignore_ascii_case(wanted.as_str()))
        };
        let mut lengths = named(CONTENT_LENGTH).map(|(_, value)| value);
        let Some(length) = lengths.next() else { return Ok(None) };
        let digits = (1..=LENGTH_DIGITS).contains(&length.len()) && length.bytes().all(|byte| byte.is_ascii_digit());
        let alone = lengths.next().is_none() && named(TRANSFER_ENCODING).next().is_none();
        if digits && alone {
            Ok(length.parse().ok())
        } else {
            Err(())
        }
    }

    fn request(&self, body: Upload) -> Option<Request<Upload>> {
        let (framed, length) = (!body.is_end_stream(), self.length().ok()?);
        let mut request = Request::new(body);
        *request.method_mut() = Method::from_bytes(self.method.as_bytes()).ok()?;
        *request.uri_mut() = self.path.parse().ok()?;
        let headers = request.headers_mut();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(&latin1(name)?).ok()?;
            headers.try_append(name, HeaderValue::from_bytes(&latin1(value)?).ok()?).ok()?;
        }
        headers.remove(EXPECT);
        if !framed {
            headers.remove(CONTENT_LENGTH);
        } else if length.is_none() {
            headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        }
        Some(request)
    }
}

pub(super) fn pairs<'a>(headers: impl Iterator<Item = (&'a HeaderName, &'a HeaderValue)>) -> Vec<(String, String)> {
    let text = |value: &HeaderValue| value.as_bytes().iter().map(|byte| char::from(*byte)).collect();
    headers.map(|(name, value)| (name.to_string(), text(value))).collect()
}

fn latin1(text: &str) -> Option<Vec<u8>> {
    text.chars().map(|char| u8::try_from(char).ok()).collect()
}

struct Entry {
    out: Out,
    feed: mpsc::UnboundedSender<Feed>,
    ws: bool,
    left: Option<u64>,
    share: Share,
    task: AbortHandle,
}

impl Entry {
    fn open(&self) -> bool {
        self.out.state.load(Ordering::Acquire) == OPEN
    }

    fn held(&self) -> usize {
        let share = self.share.lock().unwrap_or_else(PoisonError::into_inner);
        share.as_ref().map_or(0, OwnedSemaphorePermit::num_permits)
    }

    fn stop(self, why: &str) {
        self.task.abort();
        self.out.fail(why);
        self.share.lock().unwrap_or_else(PoisonError::into_inner).take();
    }

    fn feed(&self, kind: u8, payload: &[u8], room: &Arc<Semaphore>) -> bool {
        let feed = match kind {
            DATA if !self.ws => Feed::Data,
            WS_TEXT if self.ws => Feed::Text,
            WS_BINARY if self.ws => Feed::Data,
            _ => return true,
        };
        let cost = payload.len().max(LEAST_COST);
        let more = u32::try_from(cost).ok().and_then(|cost| Arc::clone(room).try_acquire_many_owned(cost).ok());
        let Some(more) = more else { return false };
        let mut share = self.share.lock().unwrap_or_else(PoisonError::into_inner);
        match share.as_mut() {
            Some(share) => share.merge(more),
            None => *share = Some(more),
        }
        drop(share);
        let held = Held { bytes: payload.to_vec(), cost, share: Arc::clone(&self.share) };
        self.feed.send(feed(Bytes::from_owner(held))).ok();
        true
    }
}

pub(super) struct Streams {
    routes: Vec<(&'static str, u16)>,
    queue: mpsc::UnboundedSender<Queued>,
    send_room: Arc<Semaphore>,
    body_room: Arc<Semaphore>,
    open: HashMap<u32, Entry>,
    tasks: JoinSet<()>,
}

impl Streams {
    pub(super) fn new(ports: Ports, queue: mpsc::UnboundedSender<Queued>) -> Self {
        Self {
            routes: path_ports(ports),
            queue,
            send_room: Arc::new(Semaphore::new(SEND_ROOM)),
            body_room: Arc::new(Semaphore::new(BODY_ROOM)),
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
            if entry.task.id() == done {
                entry.out.fail(FAILED);
            }
            entry.open() || Arc::strong_count(&entry.out.state) > 1
        });
    }

    pub(super) fn control(&mut self, kind: &str, frame: &Value) {
        let id = frame.get("id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
        let Some(id) = id.filter(|id| *id != 0) else { return };
        match kind {
            "req" => self.start(id, frame),
            "end" => {
                if let Some(entry) = self.open.get(&id).filter(|entry| !entry.ws) {
                    entry.feed.send(Feed::End).ok();
                }
            }
            "cancel" => {
                if let Some(entry) = self.open.remove(&id) {
                    entry.out.state.store(STOPPED, Ordering::Release);
                    entry.task.abort();
                }
            }
            "ws_close" => {
                let Some(entry) = self.open.get(&id).filter(|entry| entry.ws && entry.open()) else { return };
                entry.out.state.store(STOPPED, Ordering::Release);
                entry.feed.send(Feed::close(frame)).ok();
                let task = entry.task.clone();
                self.tasks.spawn(async move {
                    sleep(WS_CLOSE).await;
                    task.abort();
                });
            }
            _ => {}
        }
    }

    pub(super) fn data(&mut self, bytes: &[u8]) {
        let Some((head, payload)) = bytes.split_first_chunk::<HEAD>() else { return };
        let [kind, id @ ..] = *head;
        let id = u32::from_be_bytes(id);
        let Some(entry) = self.open.get_mut(&id).filter(|entry| entry.open()) else { return };
        if let Some(left) = entry.left.as_mut().filter(|_| kind == DATA) {
            let Some(less) = left.checked_sub(payload.len() as u64) else { return self.fail(id, TOO_LONG) };
            *left = less;
        }
        while self.open.get(&id).is_some_and(|entry| !entry.feed(kind, payload, &self.body_room)) {
            let waits = |(other, entry): &(&u32, &Entry)| {
                entry.held() + if **other == id { payload.len().max(LEAST_COST) } else { 0 }
            };
            let worst = self.open.iter().max_by_key(waits).map_or(id, |(worst, _)| *worst);
            self.fail(worst, TOO_MUCH);
        }
    }

    fn fail(&mut self, id: u32, why: &str) {
        if let Some(entry) = self.open.remove(&id) {
            entry.stop(why);
        }
    }

    fn start(&mut self, id: u32, frame: &Value) {
        if self.open.get(&id).is_some_and(Entry::open) {
            return;
        }
        let state = Arc::new(AtomicU8::new(OPEN));
        let out = Out { id, state, queue: self.queue.clone(), room: Arc::clone(&self.send_room) };
        let (feed, upload) = mpsc::unbounded_channel();
        let open = Open::deserialize(frame).ok();
        let ws = open.as_ref().is_some_and(|open| open.ws);
        let sized = open.as_ref().filter(|open| open.body && !open.ws);
        let left = sized.and_then(|open| open.length().ok()).flatten();
        let task = self.tasks.spawn(answer(out.clone(), open, upload, self.routes.clone()));
        self.open.insert(id, Entry { out, feed, ws, left, share: Share::default(), task });
    }
}

async fn answer(out: Out, open: Option<Open>, feed: mpsc::UnboundedReceiver<Feed>, routes: Vec<(&'static str, u16)>) {
    let Some(open) = open else { return out.plain(400, "Bad request.\n", "") };
    let path = open.path.split('?').next().unwrap_or_default();
    let route = routes.iter().find(|(public, _)| *public == path && (!open.ws || path == WS_PATH));
    let Some((_, port)) = route else { return out.plain(404, "No local route for this path.\n", &open.method) };
    let (socket, feed) = if open.ws { (Some(Socket::new(feed)), None) } else { (None, open.body.then_some(feed)) };
    let request = open.request(Upload(feed)).and_then(|mut request| match &socket {
        Some(socket) => socket.ask(request.headers_mut()).map(|()| request),
        None => Some(request),
    });
    let Some(request) = request else { return out.plain(400, "Bad request.\n", &open.method) };
    if let Err(why) = forward(&out, request, *port, socket).await {
        out.error(why);
    }
}

async fn forward(out: &Out, request: Request<Upload>, port: u16, socket: Option<Socket>) -> Result<(), &'static str> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.map_err(|_| UNREACHABLE)?;
    stream.set_nodelay(true).map_err(|_| UNREACHABLE)?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.map_err(|_| UNREACHABLE)?;
    let head = request.method() == Method::HEAD;
    let answered = async {
        let response = sender.send_request(request).await.map_err(|_| UNREACHABLE)?;
        match socket {
            Some(socket) if response.status() == StatusCode::SWITCHING_PROTOCOLS => socket.pump(out, response).await,
            _ => respond(out, response, head).await,
        }
    };
    drive(answered, connection.with_upgrades()).await
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
