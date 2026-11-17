use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use local_studio_core::{Item, Studio, TranscriptOp, Update, Waker};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use crate::ops;

pub const PROTOCOL: u64 = 1;
pub const MIN_PROTOCOL: u64 = 1;
const WINDOW: usize = 200;
const PAGE_LIMIT: usize = 500;
const REMEMBERED: usize = 4096;
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

pub enum Out {
    Frame(String),
    Update(Arc<str>),
    Close(String),
}

pub type ConnId = u64;

struct Conn {
    tx: mpsc::UnboundedSender<Out>,
    watching: HashSet<String>,
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
    conns: HashMap<ConnId, Conn>,
    watchers: HashMap<String, HashSet<ConnId>>,
    mirrors: HashMap<String, Mirror>,
    snapshot: [Option<Arc<str>>; 5],
    owners: Remembered<ConnId>,
    accepted: Remembered<()>,
}

impl State {
    fn send(&self, conn: ConnId, out: Out) {
        if let Some(conn) = self.conns.get(&conn) {
            conn.tx.send(out).ok();
        }
    }

    fn to_watchers(&self, thread: &str, text: &Arc<str>) {
        for conn in self.watchers.get(thread).into_iter().flatten() {
            self.send(*conn, Out::Update(Arc::clone(text)));
        }
    }

    fn broadcast(&self, text: &Arc<str>) {
        for conn in self.conns.values() {
            conn.tx.send(Out::Update(Arc::clone(text))).ok();
        }
    }

    fn replay(&self, conn: ConnId, thread: &str) {
        let Some(mirror) = self.mirrors.get(thread) else { return };
        if let Some(info) = &mirror.info {
            self.send(conn, Out::Update(Arc::clone(info)));
        }
        if let Some(items) = &mirror.items {
            self.send(conn, Out::Update(windowed_reset(thread, items)));
        }
        for text in [&mirror.approvals, &mirror.queue, &mirror.subagents].into_iter().flatten() {
            self.send(conn, Out::Update(Arc::clone(text)));
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

pub struct Hub {
    studio: Arc<Studio>,
    state: Mutex<State>,
    pub machine: Value,
    hooks: Vec<String>,
    http: reqwest::Client,
}

impl Hub {
    pub fn new(studio: Arc<Studio>, machine: Value, hooks: Vec<String>) -> anyhow::Result<Arc<Self>> {
        let http = reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build()?;
        Ok(Arc::new(Self { studio, state: Mutex::new(State::default()), machine, hooks, http }))
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

    #[allow(clippy::too_many_lines)]
    fn on_update(&self, update: Update) {
        let value = match serde_json::to_value(&update) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("local-studio-serve: could not encode an update: {error}");
                return;
            }
        };
        let mut state = self.state();
        match update {
            Update::Transcript { thread, op } => {
                let Some(mirror) = state.mirrors.get_mut(&thread) else { return };
                match op {
                    TranscriptOp::Reset { items } => {
                        let text = windowed_reset(&thread, &items);
                        mirror.items = Some(items);
                        state.to_watchers(&thread, &text);
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
                state.to_watchers(&thread, &text_of(&value));
            }
            Update::Thread { thread, .. }
            | Update::Approvals { thread, .. }
            | Update::Queue { thread, .. }
            | Update::Subagents { thread, .. } => {
                let text = text_of(&value);
                let Some(mirror) = state.mirrors.get_mut(&thread) else { return };
                let slot = match value.get("type").and_then(Value::as_str) {
                    Some("thread") => &mut mirror.info,
                    Some("approvals") => &mut mirror.approvals,
                    Some("queue") => &mut mirror.queue,
                    _ => &mut mirror.subagents,
                };
                *slot = Some(Arc::clone(&text));
                state.to_watchers(&thread, &text);
            }
            Update::ComposerText { thread, .. } | Update::WorktreeSetup { thread, .. } => {
                state.to_watchers(&thread, &text_of(&value));
            }
            Update::Connection { .. }
            | Update::Daemon { .. }
            | Update::Projects { .. }
            | Update::Threads { .. }
            | Update::Models { .. } => {
                let index = match value.get("type").and_then(Value::as_str) {
                    Some("connection") => 0,
                    Some("daemon") => 1,
                    Some("projects") => 2,
                    Some("threads") => 3,
                    _ => 4,
                };
                let text = text_of(&value);
                state.snapshot[index] = Some(Arc::clone(&text));
                state.broadcast(&text);
            }
            Update::Notification { .. } => {
                state.broadcast(&text_of(&value));
                drop(state);
                self.notify_hooks(&value);
            }
            _ => {
                let text = text_of(&value);
                let owner = value
                    .get("request")
                    .or_else(|| value.get("draft"))
                    .and_then(Value::as_str)
                    .and_then(|request| state.owners.values.get(request).copied())
                    .filter(|owner| state.conns.contains_key(owner));
                match owner {
                    Some(owner) => state.send(owner, Out::Update(text)),
                    None => state.broadcast(&text),
                }
            }
        }
    }

    fn notify_hooks(&self, notification: &Value) {
        for url in &self.hooks {
            let body = json!({ "machine": self.machine, "notification": notification });
            let request = self.http.post(url).json(&body);
            let index = self.hooks.iter().position(|hook| hook == url).unwrap_or_default();
            tokio::spawn(async move {
                match request.send().await {
                    Ok(response) if response.status().is_success() => {}
                    Ok(response) => eprintln!("local-studio-serve: notify hook {index} answered {}", response.status()),
                    Err(error) => {
                        eprintln!("local-studio-serve: notify hook {index} failed: {}", error.without_url());
                    }
                }
            });
        }
    }

    pub fn join(&self, tx: mpsc::UnboundedSender<Out>, first: impl FnOnce(&[Arc<str>]) -> String) -> ConnId {
        let mut state = self.state();
        state.next += 1;
        let id = state.next;
        let snapshot: Vec<Arc<str>> = state.snapshot.iter().flatten().cloned().collect();
        tx.send(Out::Frame(first(&snapshot))).ok();
        state.conns.insert(id, Conn { tx, watching: HashSet::new() });
        id
    }

    pub fn leave(&self, conn: ConnId) {
        let watching: Vec<String> = {
            let mut state = self.state();
            state.conns.remove(&conn).map(|conn| conn.watching.into_iter().collect()).unwrap_or_default()
        };
        for thread in watching {
            self.release(conn, &thread);
        }
    }

    fn release(&self, conn: ConnId, thread: &str) {
        let last = {
            let mut state = self.state();
            let Some(set) = state.watchers.get_mut(thread) else { return };
            set.remove(&conn);
            if set.is_empty() {
                state.watchers.remove(thread);
                state.mirrors.remove(thread);
                true
            } else {
                false
            }
        };
        if last {
            self.studio.unwatch_thread(thread.to_string());
        }
    }

    pub fn watch(&self, conn: ConnId, thread: &str) {
        let mut state = self.state();
        let Some(entry) = state.conns.get_mut(&conn) else { return };
        if !entry.watching.insert(thread.to_string()) {
            return;
        }
        let set = state.watchers.entry(thread.to_string()).or_default();
        let first = set.is_empty();
        set.insert(conn);
        if first {
            state.mirrors.insert(thread.to_string(), Mirror::default());
            self.studio.watch_thread(thread.to_string());
        } else {
            state.replay(conn, thread);
        }
    }

    pub fn unwatch(&self, conn: ConnId, thread: &str) {
        let was = {
            let mut state = self.state();
            state.conns.get_mut(&conn).is_some_and(|entry| entry.watching.remove(thread))
        };
        if was {
            self.release(conn, thread);
        }
    }

    pub fn load_earlier(
        &self,
        conn: ConnId,
        thread: &str,
        before: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Value, String> {
        let state = self.state();
        if !state.conns.get(&conn).is_some_and(|entry| entry.watching.contains(thread)) {
            return Err(format!("watch thread {thread} before loading its transcript"));
        }
        let items = state
            .mirrors
            .get(thread)
            .and_then(|mirror| mirror.items.as_ref())
            .ok_or_else(|| format!("thread {thread}'s transcript has not loaded yet"))?;
        let before = before.and_then(|before| usize::try_from(before).ok()).unwrap_or(items.len()).min(items.len());
        let limit = limit.and_then(|limit| usize::try_from(limit).ok()).unwrap_or(WINDOW).min(PAGE_LIMIT);
        let start = before.saturating_sub(limit);
        Ok(json!({ "thread": thread, "start": start, "items": &items[start..before] }))
    }

    pub fn request(&self, conn: ConnId, op: &str, args: &Value) -> Result<Option<Value>, String> {
        let mut state = self.state();
        let key = ops::idempotency_key(op, args);
        if let Some(key) = &key {
            if state.accepted.values.contains_key(key) {
                return Ok(Some(json!({ "duplicate": true })));
            }
        }
        ops::dispatch(&self.studio, op, args)?;
        if let Some(request) = ops::request_of(args) {
            state.owners.insert(request, conn);
        }
        if let Some(key) = key {
            state.accepted.insert(key, ());
        }
        Ok(None)
    }
}
