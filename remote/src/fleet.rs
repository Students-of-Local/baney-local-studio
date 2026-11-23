use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, Url};
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::devices::{Devices, FLEET};
use crate::hub::{ConnId, Hub, Out, Tx};
use crate::limits::{
    FLEET_ANSWER, FLEET_BODY, FLEET_CHUNK, FLEET_CONNECT, FLEET_IDLE, FLEET_PER_MINUTE, FLEET_STREAMS, FLEET_WINDOW,
};
use crate::ops::OpError;

const ROUTES: [(&str, &str, bool); 6] = [
    ("GET", "/v1/models", true),
    ("POST", "/v1/chat/completions", true),
    ("POST", "/v1/completions", true),
    ("POST", "/v1/messages", true),
    ("POST", "/v1/responses", true),
    ("GET", "/api/node", false),
];
const PASSED: [&str; 3] = ["content-type", "accept", "x-local-studio-endpoint"];
const HOP: &str = "x-local-studio-hop";
const RETRY: &str = "x-local-studio-retry";
const MAX_QUERY: usize = 4096;
const MAX_HEADER: usize = 1024;
const MAX_STREAM_ID: usize = 64;

struct Call {
    conn: ConnId,
    stream: Option<String>,
    cancel: Option<oneshot::Sender<()>>,
    abort: Option<AbortHandle>,
}

#[derive(Default)]
struct Usage {
    recent: VecDeque<Instant>,
    calls: HashMap<u64, Call>,
}

pub struct Fleet {
    hub: Arc<Hub>,
    devices: Devices,
    data_dir: std::path::PathBuf,
    controller: String,
    http: reqwest::Client,
    next: AtomicU64,
    usage: Mutex<HashMap<String, Usage>>,
}

struct Asked {
    method: Method,
    path: &'static str,
    hop: bool,
    query: Vec<(String, String)>,
    headers: HeaderMap,
    body: Vec<u8>,
    stream: bool,
    stream_id: Option<String>,
}

struct Reply {
    tx: Tx,
    id: Value,
}

impl Reply {
    fn send(&self, result: &Value) {
        self.tx.send(Out::Frame(json!({ "re": self.id, "ok": true, "result": result }).to_string()));
    }

    fn status(&self, status: u16, message: &str) {
        self.send(&json!({
            "status": status,
            "headers": { "content-type": "text/plain; charset=utf-8" },
            "body": STANDARD.encode(message.as_bytes()),
        }));
    }

    fn unsent(&self, status: u16, message: &str) {
        self.send(&json!({
            "status": status,
            "headers": { "content-type": "text/plain; charset=utf-8", RETRY: "1" },
            "body": STANDARD.encode(message.as_bytes()),
        }));
    }
}

fn forbidden(message: impl Into<String>) -> OpError {
    OpError::new("forbidden", message)
}

fn bad(message: impl Into<String>) -> OpError {
    OpError::new("bad_args", message)
}

fn query_of(value: Option<&Value>) -> Result<Vec<(String, String)>, OpError> {
    let pairs = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) if text.is_empty() => Vec::new(),
        Some(Value::String(text)) => {
            if text.len() > MAX_QUERY || text.contains('#') {
                return Err(bad(format!("args.query must be at most {MAX_QUERY} bytes and hold no #")));
            }
            let parsed = Url::parse(&format!("http://query.invalid/?{text}"))
                .map_err(|error| bad(format!("args.query: {error}")))?;
            parsed.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect()
        }
        Some(Value::Object(fields)) => fields
            .iter()
            .map(|(name, value)| match value {
                Value::String(text) => Ok((name.clone(), text.clone())),
                Value::Number(number) => Ok((name.clone(), number.to_string())),
                Value::Bool(flag) => Ok((name.clone(), flag.to_string())),
                _ => Err(bad(format!("args.query.{name} must be a string"))),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(bad("args.query must be a string or an object of strings")),
    };
    if pairs.iter().map(|(name, value)| name.len() + value.len() + 2).sum::<usize>() > MAX_QUERY {
        return Err(bad(format!("args.query may be at most {MAX_QUERY} bytes")));
    }
    Ok(pairs)
}

fn headers_of(value: Option<&Value>) -> Result<HeaderMap, OpError> {
    let mut headers = HeaderMap::new();
    let fields = match value {
        None | Some(Value::Null) => return Ok(headers),
        Some(Value::Object(fields)) => fields,
        Some(_) => return Err(bad("args.headers must be an object of strings")),
    };
    for (name, value) in fields {
        let name = name.to_ascii_lowercase();
        if !PASSED.contains(&name.as_str()) {
            continue;
        }
        let text = value.as_str().ok_or_else(|| bad(format!("args.headers.{name} must be a string")))?;
        if text.len() > MAX_HEADER {
            return Err(bad(format!("args.headers.{name} may be at most {MAX_HEADER} bytes")));
        }
        let value =
            HeaderValue::from_str(text).map_err(|_| bad(format!("args.headers.{name} is not a valid value")))?;
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad("a header name is not valid"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn asked(args: &Value) -> Result<Asked, OpError> {
    let method = args.get("method").and_then(Value::as_str).ok_or_else(|| bad("args.method must be a string"))?;
    let path = args.get("path").and_then(Value::as_str).ok_or_else(|| bad("args.path must be a string"))?;
    let Some(&(_, path, hop)) = ROUTES.iter().find(|(known, route, _)| *known == method && *route == path) else {
        return Err(forbidden(format!(
            "{method} {path} does not cross a desktop link; only the model list, the four generation paths and GET /api/node do"
        )));
    };
    let method = if method == "GET" { Method::GET } else { Method::POST };
    // A client may name its stream up front, so it can cancel before the controller answers.
    let (stream, stream_id) = match args.get("stream") {
        None | Some(Value::Null) => (false, None),
        Some(Value::Bool(flag)) => (*flag, None),
        Some(Value::String(id))
            if !id.is_empty()
                && id.len() <= MAX_STREAM_ID
                && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') =>
        {
            (true, Some(id.clone()))
        }
        Some(_) => {
            return Err(bad(format!(
                "args.stream must be true, false, or a stream id of 1 to {MAX_STREAM_ID} letters, digits, - or _"
            )))
        }
    };
    let body = match args.get("body") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => STANDARD.decode(text).map_err(|_| bad("args.body must be base64"))?,
        Some(_) => return Err(bad("args.body must be a base64 string")),
    };
    Ok(Asked {
        method,
        path,
        hop,
        query: query_of(args.get("query"))?,
        headers: headers_of(args.get("headers"))?,
        body,
        stream,
        stream_id,
    })
}

fn shown_headers(response: &reqwest::Response) -> Value {
    let mut shown = Map::new();
    if let Some(kind) = response.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok()) {
        shown.insert("content-type".to_string(), json!(kind));
    }
    if response.headers().get(RETRY).is_some_and(|value| value.as_bytes() == b"1") {
        shown.insert(RETRY.to_string(), json!("1"));
    }
    Value::Object(shown)
}

impl Fleet {
    pub fn new(hub: Arc<Hub>, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let http = reqwest::Client::builder()
            .connect_timeout(FLEET_CONNECT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        Ok(Arc::new(Self {
            hub,
            devices: Devices::new(data_dir),
            data_dir: data_dir.to_path_buf(),
            controller: local_studio_core::fleet::controller_url(data_dir),
            http,
            next: AtomicU64::new(0),
            usage: Mutex::new(HashMap::new()),
        }))
    }

    fn usage(&self) -> MutexGuard<'_, HashMap<String, Usage>> {
        self.usage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn scoped(&self, device: &str) -> Result<(), OpError> {
        let known = self
            .devices
            .device(device)
            .map_err(|error| OpError::new("unavailable", format!("could not read the device list: {error:#}")))?;
        match known {
            Some(known) if known.has_scope(FLEET) => Ok(()),
            _ => Err(forbidden(
                "this device was not paired with the fleet scope; pair it with `local-studio fleet invite`",
            )),
        }
    }

    pub fn http(self: &Arc<Self>, device: &str, conn: ConnId, tx: &Tx, id: Value, args: &Value) -> Result<(), OpError> {
        self.scoped(device)?;
        let asked = asked(args)?;
        let reply = Reply { tx: tx.clone(), id };
        if asked.body.len() > FLEET_BODY {
            reply.status(413, &format!("a fleet request body may be at most {FLEET_BODY} bytes"));
            return Ok(());
        }
        let mut usage = self.usage();
        let mine = usage.entry(device.to_string()).or_default();
        let now = Instant::now();
        while mine.recent.front().is_some_and(|at| now.duration_since(*at) >= FLEET_WINDOW) {
            mine.recent.pop_front();
        }
        let streams = mine.calls.values().filter(|call| call.stream.is_some()).count();
        if mine.recent.len() >= FLEET_PER_MINUTE {
            drop(usage);
            reply.unsent(429, &format!("at most {FLEET_PER_MINUTE} fleet requests a minute from one device"));
            return Ok(());
        }
        if asked.stream && streams >= FLEET_STREAMS {
            drop(usage);
            reply.unsent(429, &format!("at most {FLEET_STREAMS} fleet streams at once from one device"));
            return Ok(());
        }
        if let Some(id) = &asked.stream_id {
            if mine.calls.values().any(|call| call.stream.as_deref() == Some(id.as_str())) {
                drop(usage);
                return Err(bad(format!("a fleet stream named {id} is already open on this device")));
            }
        }
        mine.recent.push_back(now);
        let call = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let stream = asked.stream.then(|| asked.stream_id.clone().unwrap_or_else(|| format!("~{call}")));
        let (cancel, cancelled) = oneshot::channel();
        mine.calls.insert(call, Call { conn, stream: stream.clone(), cancel: Some(cancel), abort: None });
        let fleet = Arc::clone(self);
        let owner = device.to_string();
        let task = tokio::spawn(async move {
            fleet.forward(&owner, (call, conn), asked, reply, stream, cancelled).await;
            fleet.finished(&owner, call);
        });
        if let Some(entry) = mine.calls.get_mut(&call) {
            entry.abort = Some(task.abort_handle());
        }
        Ok(())
    }

    pub fn cancel(&self, device: &str, args: &Value) -> Result<(), OpError> {
        self.scoped(device)?;
        let stream = args.get("stream").and_then(Value::as_str).ok_or_else(|| bad("args.stream must be a string"))?;
        if stream.len() > MAX_STREAM_ID {
            return Err(bad("args.stream is too long"));
        }
        let mut usage = self.usage();
        let found = usage.get_mut(device).and_then(|mine| {
            mine.calls
                .values_mut()
                .find(|call| call.stream.as_deref() == Some(stream))
                .and_then(|call| call.cancel.take())
        });
        match found {
            Some(cancel) => {
                cancel.send(()).ok();
                Ok(())
            }
            None => Err(OpError::new("unknown_stream", format!("no fleet stream {stream} is open for this device"))),
        }
    }

    pub fn detach(&self, device: &str, conn: ConnId) {
        let mut usage = self.usage();
        let Some(mine) = usage.get_mut(device) else { return };
        mine.calls.retain(|_, call| {
            if call.conn != conn {
                return true;
            }
            if let Some(abort) = &call.abort {
                abort.abort();
            }
            false
        });
    }

    fn finished(&self, device: &str, call: u64) {
        let mut usage = self.usage();
        if let Some(mine) = usage.get_mut(device) {
            mine.calls.remove(&call);
            if mine.calls.is_empty() && mine.recent.is_empty() {
                usage.remove(device);
            }
        }
    }

    fn request(&self, asked: Asked) -> Result<reqwest::RequestBuilder, String> {
        let mut url = Url::parse(&format!("{}{}", self.controller, asked.path))
            .map_err(|error| format!("the controller address {} is not valid: {error}", self.controller))?;
        // Fleet requests carry this machine's fleet key, so they only ever go to a controller on this machine.
        if url.scheme() != "http" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) {
            return Err(format!("the Local controller address {} is not on this machine", self.controller));
        }
        if !asked.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&asked.query);
        }
        let mut headers = asked.headers;
        if let Some(key) = local_studio_core::fleet::key(&self.data_dir) {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| "this machine's fleet key is not a valid header value".to_string())?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        if asked.hop {
            headers.insert(HOP, HeaderValue::from_static("1"));
        }
        let mut request = self.http.request(asked.method.clone(), url).headers(headers);
        if asked.method == Method::POST {
            request = request.body(asked.body);
        }
        Ok(request)
    }

    async fn forward(
        &self,
        device: &str,
        (call, conn): (u64, ConnId),
        asked: Asked,
        reply: Reply,
        stream: Option<String>,
        mut cancelled: oneshot::Receiver<()>,
    ) {
        let request = match self.request(asked) {
            Ok(request) => request,
            Err(message) => {
                reply.unsent(503, &message);
                return;
            }
        };
        let sent = tokio::select! {
            sent = tokio::time::timeout(FLEET_ANSWER, request.send()) => {
                let Ok(sent) = sent else {
                    reply.status(504, "the Local controller did not answer in time");
                    return;
                };
                sent
            }
            _ = &mut cancelled => {
                reply.status(499, "cancelled");
                return;
            }
        };
        let mut response = match sent {
            Ok(response) => response,
            Err(error) => {
                let message = format!("the Local controller on {} did not answer", self.controller);
                if error.is_connect() {
                    reply.unsent(503, &message);
                } else {
                    reply.status(503, &message);
                }
                return;
            }
        };
        let status = response.status().as_u16();
        let headers = shown_headers(&response);
        let Some(stream) = stream else {
            let mut body = Vec::new();
            loop {
                let Ok(next) = tokio::time::timeout(FLEET_IDLE, response.chunk()).await else {
                    reply.status(504, "the Local controller's answer stalled");
                    return;
                };
                match next {
                    Ok(Some(bytes)) => {
                        if body.len() + bytes.len() > FLEET_BODY {
                            reply.status(413, &format!("the controller's answer passed {FLEET_BODY} bytes"));
                            return;
                        }
                        body.extend_from_slice(&bytes);
                    }
                    Ok(None) => break,
                    Err(_) => {
                        reply.status(503, "the Local controller's answer broke off");
                        return;
                    }
                }
            }
            reply.send(&json!({ "status": status, "headers": headers, "body": STANDARD.encode(&body) }));
            return;
        };
        reply.send(&json!({ "status": status, "headers": headers, "stream": stream }));
        let error = loop {
            let next = tokio::select! {
                next = tokio::time::timeout(FLEET_IDLE, response.chunk()) => match next {
                    Ok(next) => next,
                    Err(_) => break Some("the Local controller's stream stalled".to_string()),
                },
                _ = &mut cancelled => break Some("cancelled".to_string()),
            };
            match next {
                Ok(Some(bytes)) => {
                    for piece in bytes.chunks(FLEET_CHUNK) {
                        let chunk = json!({ "type": "fleet_chunk", "stream": stream, "data": STANDARD.encode(piece) });
                        if !self.hub.deliver(device, conn, &chunk) {
                            return;
                        }
                    }
                }
                Ok(None) => break None,
                Err(_) => break Some("the Local controller's stream broke off".to_string()),
            }
        };
        drop(response);
        self.finished(device, call);
        let mut end = json!({ "type": "fleet_end", "stream": stream });
        if let Some(error) = error {
            end["error"] = json!(error);
        }
        self.hub.deliver(device, conn, &end);
    }
}
