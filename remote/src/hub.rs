use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use local_studio_core::{FileContent, ImageAttachment, Item, PromptDraft, Studio, TranscriptOp, Update, Waker};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use crate::devices::{hex, now_ms, random, unhex};
use crate::noise::BLOB_ID;
use crate::ops::{self, OpError};

pub const PROTOCOL: u64 = 2;
pub const MIN_PROTOCOL: u64 = 2;
const WINDOW: usize = 200;
const PAGE_LIMIT: usize = 500;
const REMEMBERED: usize = 4096;
const BUFFERED: usize = 2000;
const KEPT: Duration = Duration::from_secs(600);
const MAX_WATCH: usize = 64;
const MAX_BLOB: usize = 25 << 20;
const MAX_PENDING: usize = 100 << 20;
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);
const SNAPSHOT: [&str; 5] = ["connection", "projects", "threads", "models", "daemon_default"];

pub struct Blob {
    pub id: [u8; BLOB_ID],
    pub data: Vec<u8>,
}

pub enum Out {
    Frame(String),
    Update { seq: u64, text: Arc<str>, blob: Option<Arc<Blob>> },
    Close(u16, String),
}

pub type ConnId = u64;
pub type Tx = mpsc::UnboundedSender<Out>;

struct Entry {
    seq: u64,
    at: Instant,
    text: Arc<str>,
    blob: Option<Arc<Blob>>,
}

struct Upload {
    mime: String,
    size: usize,
    data: Vec<u8>,
    ack: Option<Value>,
    at: Instant,
}

struct Session {
    key: String,
    conn: Option<(ConnId, Tx)>,
    seq: u64,
    buffer: VecDeque<Entry>,
    watching: HashSet<String>,
    left_at: Option<Instant>,
    uploads: HashMap<[u8; BLOB_ID], Upload>,
}

impl Session {
    fn new(key: String) -> Self {
        Self {
            key,
            conn: None,
            seq: 0,
            buffer: VecDeque::new(),
            watching: HashSet::new(),
            left_at: None,
            uploads: HashMap::new(),
        }
    }

    fn push(&mut self, text: Arc<str>, blob: Option<Arc<Blob>>) {
        self.seq += 1;
        let now = Instant::now();
        self.buffer.push_back(Entry { seq: self.seq, at: now, text: Arc::clone(&text), blob: blob.clone() });
        while self.buffer.len() > BUFFERED
            || self.buffer.front().is_some_and(|entry| now.duration_since(entry.at) > KEPT)
        {
            self.buffer.pop_front();
        }
        if let Some((_, tx)) = &self.conn {
            tx.send(Out::Update { seq: self.seq, text, blob }).ok();
        }
    }

    fn pending(&self) -> usize {
        self.uploads.values().map(|upload| upload.size).sum()
    }

    fn close(&mut self, code: u16, reason: &str) {
        if let Some((_, tx)) = self.conn.take() {
            tx.send(Out::Close(code, reason.to_string())).ok();
        }
    }
}

#[derive(Default)]
struct Mirror {
    items: Option<Vec<Item>>,
    info: Option<Arc<str>>,
    approvals: Option<Arc<str>>,
    queue: Option<Arc<str>>,
    subagents: Option<Arc<str>>,
}

struct Remembered<T> {
    order: VecDeque<String>,
    values: HashMap<String, T>,
}

impl<T> Default for Remembered<T> {
    fn default() -> Self {
        Self { order: VecDeque::new(), values: HashMap::new() }
    }
}

impl<T> Remembered<T> {
    fn insert(&mut self, key: String, value: T) {
        if self.values.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > REMEMBERED {
            if let Some(old) = self.order.pop_front() {
                self.values.remove(&old);
            }
        }
    }
}

#[derive(Default)]
struct State {
    next: ConnId,
    sessions: HashMap<String, Session>,
    watchers: HashMap<String, HashSet<String>>,
    mirrors: HashMap<String, Mirror>,
    snapshot: [Option<Arc<str>>; SNAPSHOT.len()],
    owners: Remembered<String>,
    git: HashMap<String, HashSet<String>>,
    accepted: Remembered<()>,
    sends: HashMap<String, (String, Instant)>,
}

impl State {
    fn deliver(&mut self, device: &str, text: Arc<str>, blob: Option<Arc<Blob>>) {
        if let Some(session) = self.sessions.get_mut(device) {
            session.push(text, blob);
        }
    }

    fn send_watchers(&mut self, thread: &str, text: &Arc<str>) {
        if let Some((request, at)) = self.sends.remove(thread) {
            let kind = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            eprintln!(
                "local-studio-serve: send {request} first update ({kind}) out at {} ms, {} ms after accept",
                now_ms(),
                at.elapsed().as_millis()
            );
        }
        let devices: Vec<String> = self.watchers.get(thread).into_iter().flatten().cloned().collect();
        for device in devices {
            self.deliver(&device, Arc::clone(text), None);
        }
    }

    fn broadcast(&mut self, text: &Arc<str>) {
        for session in self.sessions.values_mut() {
            session.push(Arc::clone(text), None);
        }
    }

    fn replay(&mut self, device: &str, thread: &str) {
        let Some(mirror) = self.mirrors.get(thread) else { return };
        let mut texts: Vec<Arc<str>> = mirror.info.iter().cloned().collect();
        if let Some(items) = &mirror.items {
            texts.push(windowed_reset(thread, items));
        }
        texts.extend([&mirror.approvals, &mirror.queue, &mirror.subagents].into_iter().flatten().cloned());
        for text in texts {
            self.deliver(device, text, None);
        }
    }

    fn owner(&self, request: Option<&str>) -> Option<String> {
        request
            .and_then(|request| self.owners.values.get(request))
            .filter(|device| self.sessions.contains_key(*device))
            .cloned()
    }

    fn release(&mut self, studio: &Studio, device: &str, thread: &str) {
        let Some(set) = self.watchers.get_mut(thread) else { return };
        set.remove(device);
        if set.is_empty() {
            self.watchers.remove(thread);
            self.mirrors.remove(thread);
            studio.unwatch_thread(thread.to_string());
        }
    }

    fn watch(&mut self, studio: &Studio, device: &str, thread: &str) {
        let Some(session) = self.sessions.get_mut(device) else { return };
        if !session.watching.insert(thread.to_string()) {
            return;
        }
        let set = self.watchers.entry(thread.to_string()).or_default();
        let first = set.is_empty();
        set.insert(device.to_string());
        if first {
            self.mirrors.insert(thread.to_string(), Mirror::default());
            studio.watch_thread(thread.to_string());
        } else {
            self.replay(device, thread);
        }
    }

    fn drop_session(&mut self, studio: &Studio, device: &str, code: u16, reason: &str) {
        let Some(mut session) = self.sessions.remove(device) else { return };
        session.close(code, reason);
        for thread in session.watching {
            self.release(studio, device, &thread);
        }
        for set in self.git.values_mut() {
            set.remove(device);
        }
    }
}

fn text_of(value: &Value) -> Arc<str> {
    Arc::from(value.to_string())
}

fn windowed_reset(thread: &str, items: &[Item]) -> Arc<str> {
    let earlier = items.len().saturating_sub(WINDOW);
    let update = json!({
        "type": "transcript",
        "thread": thread,
        "op": { "type": "reset", "items": &items[earlier..], "earlier": earlier },
    });
    text_of(&update)
}

pub struct Signal(Arc<Notify>);

impl Waker for Signal {
    fn wake(&self) {
        self.0.notify_one();
    }
}

pub fn signal() -> (Arc<Signal>, Arc<Notify>) {
    let notify = Arc::new(Notify::new());
    (Arc::new(Signal(Arc::clone(&notify))), notify)
}

pub struct Resume {
    pub seq: Option<u64>,
    pub epoch: Option<String>,
    pub watch: Vec<String>,
}

pub struct Hub {
    studio: Arc<Studio>,
    state: Mutex<State>,
    pub machine: Value,
    pub epoch: String,
    hooks: Vec<String>,
    http: reqwest::Client,
}

impl Hub {
    pub fn new(studio: Arc<Studio>, machine: Value, hooks: Vec<String>) -> anyhow::Result<Arc<Self>> {
        let http = reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build()?;
        let epoch = hex(&random::<8>()?);
        Ok(Arc::new(Self { studio, state: Mutex::new(State::default()), machine, epoch, hooks, http }))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn pump(self: Arc<Self>, notify: Arc<Notify>) {
        self.studio.load_models();
        loop {
            notify.notified().await;
            for update in self.studio.tick() {
                self.on_update(update);
            }
        }
    }

    pub async fn reap(self: Arc<Self>) {
        let mut every = tokio::time::interval(Duration::from_secs(15));
        loop {
            every.tick().await;
            let mut state = self.state();
            let now = Instant::now();
            let gone: Vec<String> = state
                .sessions
                .iter()
                .filter(|(_, session)| session.left_at.is_some_and(|left| now.duration_since(left) > KEPT))
                .map(|(device, _)| device.clone())
                .collect();
            for device in gone {
                state.drop_session(&self.studio, &device, 1000, "gone");
            }
            for session in state.sessions.values_mut() {
                session.uploads.retain(|_, upload| now.duration_since(upload.at) <= KEPT);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn on_update(&self, update: Update) {
        let value = match serde_json::to_value(&update) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("local-studio-serve: could not encode an update: {error}");
                return;
            }
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut state = self.state();
        match update {
            Update::Transcript { thread, op } => {
                let Some(mirror) = state.mirrors.get_mut(&thread) else { return };
                match op {
                    TranscriptOp::Reset { items } => {
                        let text = windowed_reset(&thread, &items);
                        mirror.items = Some(items);
                        state.send_watchers(&thread, &text);
                        return;
                    }
                    TranscriptOp::Insert { index, item } => {
                        if let Some(items) = mirror.items.as_mut() {
                            let index = usize::try_from(index).unwrap_or(usize::MAX);
                            if index <= items.len() {
                                items.insert(index, item);
                            } else {
                                mirror.items = None;
                            }
                        }
                    }
                    TranscriptOp::Update { index, item } => {
                        if let Some(items) = mirror.items.as_mut() {
                            match usize::try_from(index).ok().and_then(|index| items.get_mut(index)) {
                                Some(slot) => *slot = item,
                                None => mirror.items = None,
                            }
                        }
                    }
                }
                state.send_watchers(&thread, &text_of(&value));
            }
            Update::Thread { thread, .. }
            | Update::Approvals { thread, .. }
            | Update::Queue { thread, .. }
            | Update::Subagents { thread, .. } => {
                let text = text_of(&value);
                let Some(mirror) = state.mirrors.get_mut(&thread) else { return };
                let slot = match kind.as_str() {
                    "thread" => &mut mirror.info,
                    "approvals" => &mut mirror.approvals,
                    "queue" => &mut mirror.queue,
                    _ => &mut mirror.subagents,
                };
                *slot = Some(Arc::clone(&text));
                state.send_watchers(&thread, &text);
            }
            Update::WorktreeSetup { thread, .. } => state.send_watchers(&thread, &text_of(&value)),
            Update::Notification { thread, kind: notification, .. } => {
                state.broadcast(&text_of(&value));
                drop(state);
                self.notify_hooks(&thread, &value.get("kind").cloned().unwrap_or(json!(format!("{notification:?}"))));
            }
            Update::GitStatus { project, .. } => {
                let text = text_of(&value);
                for device in state.git.remove(&project).unwrap_or_default() {
                    state.deliver(&device, Arc::clone(&text), None);
                }
            }
            Update::MessageImage { request, mime_type, data } => {
                let Some(owner) = state.owner(Some(&request)) else { return };
                let Ok(id) = random::<BLOB_ID>() else { return };
                let header = json!({ "type": "blob", "request": request, "blob": hex(&id), "size": data.len(), "mime": mime_type });
                state.deliver(&owner, text_of(&header), Some(Arc::new(Blob { id, data })));
            }
            Update::FileContent { request, path, content } => {
                let Some(owner) = state.owner(Some(&request)) else { return };
                let (mime, data, language) = match content {
                    FileContent::Text { text, language } => {
                        ("text/plain; charset=utf-8".to_string(), text.into_bytes(), language)
                    }
                    FileContent::Image { mime_type, data } => (mime_type, data, None),
                    FileContent::Binary { size } | FileContent::TooLarge { size } => {
                        let what = if kind_of_file(&value) == "binary" { "is binary" } else { "is too large to send" };
                        let failed = json!({ "type": "request_failed", "request": request, "message": format!("{path} {what} ({size} bytes)") });
                        state.deliver(&owner, text_of(&failed), None);
                        return;
                    }
                };
                let Ok(id) = random::<BLOB_ID>() else { return };
                let header = json!({ "type": "blob", "request": request, "blob": hex(&id), "size": data.len(), "mime": mime, "path": path, "language": language });
                state.deliver(&owner, text_of(&header), Some(Arc::new(Blob { id, data })));
            }
            _ => match kind.as_str() {
                "connection" | "projects" | "threads" | "models" | "daemon_default" => {
                    let text = text_of(&value);
                    if let Some(index) = SNAPSHOT.iter().position(|name| *name == kind) {
                        state.snapshot[index] = Some(Arc::clone(&text));
                    }
                    state.broadcast(&text);
                }
                "thread_started" | "notice" | "request_done" | "request_failed" => state.broadcast(&text_of(&value)),
                "thread_search"
                | "archived_threads"
                | "commands"
                | "skills"
                | "diff"
                | "pull_requests"
                | "pull_request_detail" => {
                    let request = value.get("request").and_then(Value::as_str);
                    if let Some(owner) = state.owner(request) {
                        state.deliver(&owner, text_of(&value), None);
                    }
                }
                _ => {}
            },
        }
    }

    fn notify_hooks(&self, thread: &str, kind: &Value) {
        let body = json!({
            "machine": { "id": self.machine.get("id"), "name": self.machine.get("name") },
            "kind": kind,
            "thread": thread,
        });
        for (index, url) in self.hooks.iter().enumerate() {
            let request = self.http.post(url).json(&body);
            tokio::spawn(async move {
                match request.send().await {
                    Ok(response) if response.status().is_success() => {}
                    Ok(response) => eprintln!("local-studio-serve: notify hook {index} answered {}", response.status()),
                    Err(error) => eprintln!("local-studio-serve: notify hook {index} failed: {}", error.without_url()),
                }
            });
        }
    }

    pub fn attach(&self, device: &str, name: &str, key: &str, tx: Tx, resume: &Resume) -> ConnId {
        let mut state = self.state();
        state.next += 1;
        let conn = state.next;
        let fresh = state.sessions.get(device).is_none_or(|session| session.key != key);
        if fresh {
            state.drop_session(&self.studio, device, 4000, "replaced");
            state.sessions.insert(device.to_string(), Session::new(key.to_string()));
        }
        let snapshot: Vec<Value> =
            state.snapshot.iter().flatten().filter_map(|text| serde_json::from_str(text).ok()).collect();
        let Some(session) = state.sessions.get_mut(device) else { return conn };
        session.close(4000, "replaced by a newer socket");
        let delta = !fresh
            && resume.epoch.as_deref() == Some(self.epoch.as_str())
            && resume.seq.is_some_and(|seq| {
                seq <= session.seq
                    && (seq == session.seq || session.buffer.front().is_some_and(|entry| entry.seq <= seq + 1))
            });
        let base = if delta { resume.seq.unwrap_or_default() } else { session.seq };
        let hello = json!({
            "op": "hello",
            "protocol": PROTOCOL,
            "min_protocol": MIN_PROTOCOL,
            "epoch": self.epoch,
            "machine": self.machine,
            "device": { "id": device, "name": name },
            "resume": if delta { "delta" } else { "snapshot" },
            "seq": base,
            "snapshot": if delta { Vec::new() } else { snapshot },
        });
        tx.send(Out::Frame(hello.to_string())).ok();
        if delta {
            for entry in session.buffer.iter().filter(|entry| entry.seq > base) {
                tx.send(Out::Update { seq: entry.seq, text: Arc::clone(&entry.text), blob: entry.blob.clone() }).ok();
            }
        }
        session.conn = Some((conn, tx));
        session.left_at = None;
        let wanted: Vec<String> = resume.watch.iter().take(MAX_WATCH).cloned().collect();
        let held: Vec<String> = session.watching.iter().cloned().collect();
        for thread in held.iter().filter(|thread| !wanted.contains(thread)) {
            if let Some(session) = state.sessions.get_mut(device) {
                session.watching.remove(thread);
            }
            state.release(&self.studio, device, thread);
        }
        for thread in &wanted {
            if held.contains(thread) {
                if !delta {
                    state.replay(device, thread);
                }
            } else {
                state.watch(&self.studio, device, thread);
            }
        }
        conn
    }

    pub fn detach(&self, device: &str, conn: ConnId) {
        let mut state = self.state();
        if let Some(session) = state.sessions.get_mut(device) {
            if session.conn.as_ref().is_some_and(|(id, _)| *id == conn) {
                session.conn = None;
                session.left_at = Some(Instant::now());
            }
        }
    }

    pub fn retain(&self, known: &HashMap<String, String>) {
        let mut state = self.state();
        let gone: Vec<String> = state
            .sessions
            .iter()
            .filter(|(device, session)| known.get(*device) != Some(&session.key))
            .map(|(device, _)| device.clone())
            .collect();
        for device in gone {
            eprintln!("local-studio-serve: device {device} was revoked; closing its socket");
            state.drop_session(&self.studio, &device, 4401, "revoked");
        }
    }

    pub fn watch(&self, device: &str, thread: &str) -> Result<(), OpError> {
        let mut state = self.state();
        if state.sessions.get(device).is_some_and(|session| session.watching.len() >= MAX_WATCH) {
            return Err(OpError::new("too_many", format!("watch at most {MAX_WATCH} threads at once")));
        }
        state.watch(&self.studio, device, thread);
        Ok(())
    }

    pub fn unwatch(&self, device: &str, thread: &str) {
        let mut state = self.state();
        let was = state.sessions.get_mut(device).is_some_and(|session| session.watching.remove(thread));
        if was {
            state.release(&self.studio, device, thread);
        }
    }

    pub fn load_earlier(&self, device: &str, args: &Value) -> Result<Value, OpError> {
        let thread: String = ops::arg(args, "thread")?;
        let before: Option<u64> = ops::arg(args, "before")?;
        let limit: Option<u64> = ops::arg(args, "limit")?;
        let state = self.state();
        if !state.sessions.get(device).is_some_and(|session| session.watching.contains(&thread)) {
            return Err(OpError::new("not_watched", format!("watch thread {thread} before loading its transcript")));
        }
        let items =
            state.mirrors.get(&thread).and_then(|mirror| mirror.items.as_ref()).ok_or_else(|| {
                OpError::new("not_loaded", format!("thread {thread}'s transcript has not loaded yet"))
            })?;
        let before = before.and_then(|before| usize::try_from(before).ok()).unwrap_or(items.len()).min(items.len());
        let limit = limit.and_then(|limit| usize::try_from(limit).ok()).unwrap_or(WINDOW).min(PAGE_LIMIT);
        let start = before.saturating_sub(limit);
        Ok(json!({ "thread": thread, "start": start, "items": &items[start..before] }))
    }

    pub fn request(&self, device: &str, op: &str, args: &Value) -> Result<Option<Value>, OpError> {
        let mut state = self.state();
        let key = ops::idempotency_key(op, args);
        if key.as_ref().is_some_and(|key| state.accepted.values.contains_key(key)) {
            return Ok(Some(json!({ "duplicate": true })));
        }
        let session = state.sessions.get_mut(device).ok_or_else(|| OpError::new("closed", "this session ended"))?;
        let mut prompt = |value: &Value| resolve_prompt(session, value);
        ops::dispatch(&self.studio, op, args, &mut prompt)?;
        if let Some(request) = ops::request_of(args) {
            state.owners.insert(request, device.to_string());
        }
        if op == "git_status" {
            if let Some(project) = args.get("project").and_then(Value::as_str) {
                state.git.entry(project.to_string()).or_default().insert(device.to_string());
            }
        }
        if let Some(key) = key {
            state.accepted.insert(key, ());
        }
        if op == "send" {
            if let (Some(request), Some(thread)) =
                (args.get("request").and_then(Value::as_str), args.get("thread").and_then(Value::as_str))
            {
                eprintln!("local-studio-serve: send {request} accepted for thread {thread} at {} ms", now_ms());
                state.sends.insert(thread.to_string(), (request.to_string(), Instant::now()));
            }
        }
        Ok(None)
    }

    pub fn blob_put(&self, device: &str, id: Value, args: &Value) -> Result<(), OpError> {
        let blob: String = ops::arg(args, "blob")?;
        let size: usize = ops::arg(args, "size")?;
        let mime: String = ops::arg(args, "mime")?;
        let blob = unhex(&blob)
            .and_then(|bytes| <[u8; BLOB_ID]>::try_from(bytes).ok())
            .ok_or_else(|| OpError::new("bad_args", "args.blob must be 16 bytes in hex"))?;
        if size > MAX_BLOB {
            return Err(OpError::new("too_large", format!("a blob may be at most {MAX_BLOB} bytes")));
        }
        let mut state = self.state();
        let session = state.sessions.get_mut(device).ok_or_else(|| OpError::new("closed", "this session ended"))?;
        if session.uploads.contains_key(&blob) {
            return Err(OpError::new("bad_args", "this blob id is already in use"));
        }
        if session.pending() + size > MAX_PENDING {
            return Err(OpError::new("too_large", format!("at most {MAX_PENDING} bytes may be pending per device")));
        }
        let upload =
            Upload { mime, size, data: Vec::with_capacity(size.min(1 << 20)), ack: Some(id), at: Instant::now() };
        session.uploads.insert(blob, upload);
        Ok(())
    }

    pub fn blob_chunk(
        &self,
        device: &str,
        blob: [u8; BLOB_ID],
        last: bool,
        data: &[u8],
    ) -> Result<Option<Value>, String> {
        let mut state = self.state();
        let session = state.sessions.get_mut(device).ok_or("this session ended")?;
        let upload = session
            .uploads
            .get_mut(&blob)
            .ok_or_else(|| format!("a chunk for blob {} with no blob_put", hex(&blob)))?;
        let Some(ack) = upload.ack.clone() else {
            return Err(format!("a chunk after blob {} was complete", hex(&blob)));
        };
        if upload.data.len() + data.len() > upload.size {
            return Err(format!("blob {} is longer than its declared {} bytes", hex(&blob), upload.size));
        }
        upload.data.extend_from_slice(data);
        if !last {
            return Ok(None);
        }
        if upload.data.len() != upload.size {
            let short = upload.data.len();
            session.uploads.remove(&blob);
            let error = OpError::new("bad_blob", format!("the blob ended at {short} bytes, not the declared size"));
            return Ok(Some(json!({ "re": ack, "error": error.to_value() })));
        }
        upload.ack = None;
        upload.at = Instant::now();
        Ok(Some(json!({ "re": ack, "ok": true })))
    }
}

fn kind_of_file(value: &Value) -> &str {
    value.pointer("/content/type").and_then(Value::as_str).unwrap_or_default()
}

fn resolve_prompt(session: &mut Session, value: &Value) -> Result<PromptDraft, OpError> {
    let text: String = ops::arg(value, "text")?;
    let files: Option<Vec<String>> = ops::arg(value, "files")?;
    let ids: Option<Vec<String>> = ops::arg(value, "images")?;
    let mut images = Vec::new();
    for id in ids.unwrap_or_default() {
        let blob = unhex(&id)
            .and_then(|bytes| <[u8; BLOB_ID]>::try_from(bytes).ok())
            .ok_or_else(|| OpError::new("bad_args", format!("image {id:?} is not a blob id")))?;
        match session.uploads.get(&blob) {
            Some(upload) if upload.ack.is_none() => {}
            Some(_) => return Err(OpError::new("bad_blob", format!("blob {id} has not finished uploading"))),
            None => return Err(OpError::new("bad_blob", format!("no uploaded blob {id}"))),
        }
        if let Some(upload) = session.uploads.remove(&blob) {
            images.push(ImageAttachment { mime_type: upload.mime, data: upload.data });
        }
    }
    Ok(PromptDraft { text, images, files: files.unwrap_or_default() })
}
