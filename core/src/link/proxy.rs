use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use http_body_util::BodyExt;
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use super::client::{Inner, Lookup, Out, Piece};
use super::devices::{hex, random, write_private};

pub const PORT: u16 = 18095;
pub const FLEET_BODY: usize = ((8 << 20) - (4 << 10)) / 4 * 3;
const PASSED: [&str; 3] = ["content-type", "accept", "x-local-studio-endpoint"];
const RETRY: &str = "x-local-studio-retry";
const BIND_RETRY: Duration = Duration::from_secs(2);
const HEADERS_WITHIN: Duration = Duration::from_secs(10);
const DRAINED: usize = 4 * FLEET_BODY;

#[must_use]
pub fn peer_url(machine: &str) -> String {
    format!("http://127.0.0.1:{PORT}/m/{machine}")
}

fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("remote").join("fleet-proxy-token")
}

#[must_use]
pub fn token(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(token_path(data_dir)).ok()?;
    let token = text.trim();
    (token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| token.to_string())
}

pub(crate) fn mint_token(data_dir: &Path) -> anyhow::Result<String> {
    if let Some(token) = token(data_dir) {
        return Ok(token);
    }
    let token = hex(&random::<32>()?);
    write_private(&token_path(data_dir), token.as_bytes())?;
    Ok(token)
}

pub(crate) fn drop_token(data_dir: &Path) {
    std::fs::remove_file(token_path(data_dir)).ok();
}

pub(super) async fn serve(inner: Arc<Inner>) {
    let Some(token) = token(&inner.data_dir) else {
        inner.warn(format!(
            "The fleet link proxy has no token in {}, so it stays off; run `local-studio fleet join` again",
            token_path(&inner.data_dir).display()
        ));
        return;
    };
    let token: Arc<str> = Arc::from(token);
    let mut told = false;
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", PORT)).await {
            Ok(listener) => break listener,
            Err(error) => {
                if !told {
                    told = true;
                    inner.warn(format!(
                        "The fleet link proxy could not listen on 127.0.0.1:{PORT} ({error}); it keeps trying"
                    ));
                }
                tokio::time::sleep(BIND_RETRY).await;
            }
        }
    };
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let inner = Arc::clone(&inner);
        let token = Arc::clone(&token);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let inner = Arc::clone(&inner);
                let token = Arc::clone(&token);
                async move { Ok::<_, Infallible>(handle(&inner, &token, request).await) }
            });
            http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADERS_WITHIN)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .ok();
        });
    }
}

struct Cancel {
    tx: mpsc::UnboundedSender<Out>,
    stream: Option<String>,
}

impl Cancel {
    fn disarm(&mut self) {
        self.stream = None;
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            self.tx.send(Out::FleetCancel(stream)).ok();
        }
    }
}

struct Answer {
    full: Option<Bytes>,
    pieces: Option<mpsc::UnboundedReceiver<Piece>>,
    cancel: Option<Cancel>,
}

impl Answer {
    fn full(bytes: impl Into<Bytes>) -> Self {
        let bytes = bytes.into();
        Self { full: (!bytes.is_empty()).then_some(bytes), pieces: None, cancel: None }
    }

    fn ended(&mut self) {
        self.pieces = None;
        if let Some(cancel) = self.cancel.as_mut() {
            cancel.disarm();
        }
    }
}

impl Body for Answer {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(bytes) = this.full.take() {
            return Poll::Ready(Some(Ok(Frame::data(bytes))));
        }
        let Some(pieces) = this.pieces.as_mut() else { return Poll::Ready(None) };
        match pieces.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Piece::Data(data))) => Poll::Ready(Some(Ok(Frame::data(Bytes::from(data))))),
            Poll::Ready(Some(Piece::End(None))) => {
                this.ended();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Piece::End(Some(error)))) => {
                this.ended();
                Poll::Ready(Some(Err(std::io::Error::other(error))))
            }
            Poll::Ready(None) => {
                this.ended();
                Poll::Ready(Some(Err(std::io::Error::other("the desktop link closed mid-stream"))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.full.is_none() && self.pieces.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        match (&self.full, &self.pieces) {
            (Some(bytes), _) => SizeHint::with_exact(bytes.len() as u64),
            (None, None) => SizeHint::with_exact(0),
            (None, Some(_)) => SizeHint::default(),
        }
    }
}

type Reply = Response<Answer>;

fn reply(status: StatusCode, kind: Option<&str>, body: Answer) -> Reply {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Some(kind) = kind.and_then(|kind| HeaderValue::from_str(kind).ok()) {
        response.headers_mut().insert(CONTENT_TYPE, kind);
    }
    response
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Reply {
    let body = json!({ "error": { "code": code, "message": message } }).to_string();
    reply(status, Some("application/json"), Answer::full(body))
}

fn unsent(mut response: Reply) -> Reply {
    response.headers_mut().insert(RETRY, HeaderValue::from_static("1"));
    response
}

fn authorized(key: &str, headers: &HeaderMap) -> bool {
    let Some(given) = headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok()) else { return false };
    let Some(given) = given.get(..7).filter(|scheme| scheme.eq_ignore_ascii_case("bearer ")).map(|_| given[7..].trim())
    else {
        return false;
    };
    let (given, key) = (Sha256::digest(given.as_bytes()), Sha256::digest(key.as_bytes()));
    given.iter().zip(key.iter()).fold(0u8, |differs, (a, b)| differs | (a ^ b)) == 0
}

fn route(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/m/")?;
    let at = rest.find('/')?;
    let (machine, path) = rest.split_at(at);
    (!machine.is_empty()).then_some((machine, path))
}

async fn handle(inner: &Inner, token: &str, request: Request<Incoming>) -> Reply {
    if !authorized(token, request.headers()) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "send this machine's fleet proxy token as a bearer token",
        );
    }
    let target = request.uri().path().to_string();
    let Some((machine, path)) = route(&target) else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "fleet link paths are /m/<machine id>/<controller path>");
    };
    let (name, tx) = match inner.fleet_link(machine) {
        Lookup::Unknown => {
            let message = format!("no machine {machine} is linked here with fleet access");
            return refuse(StatusCode::NOT_FOUND, "unknown_machine", &message);
        }
        Lookup::Down(name) => {
            let message = format!("{name} is not connected over its desktop link");
            return unsent(refuse(StatusCode::SERVICE_UNAVAILABLE, "link_down", &message));
        }
        Lookup::Up(name, tx) => (name, tx),
    };
    let declared = request.headers().get(CONTENT_LENGTH).and_then(|value| value.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|length| length > DRAINED as u64) {
        return too_large();
    }
    let method = request.method().clone();
    let mut headers = Map::new();
    for passed in PASSED {
        if let Some(value) = request.headers().get(passed).and_then(|value| value.to_str().ok()) {
            headers.insert(passed.to_string(), Value::String(value.to_string()));
        }
    }
    let query = request.uri().query().unwrap_or_default().to_string();
    let path = path.to_string();
    let Some(body) = read_body(request.into_body()).await else {
        return refuse(StatusCode::BAD_REQUEST, "bad_body", "the request body broke off");
    };
    let Some(body) = body else { return too_large() };
    let mut args = json!({ "method": method.as_str(), "path": path, "headers": headers });
    if !query.is_empty() {
        args["query"] = Value::String(query);
    }
    if !body.is_empty() {
        args["body"] = Value::String(STANDARD.encode(&body));
    }
    let streaming = method == Method::POST;
    let stream = if streaming {
        let Ok(id) = random::<12>() else {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, "random", "could not name the fleet stream");
        };
        Some(hex(&id))
    } else {
        None
    };
    let (answer, answered) = oneshot::channel();
    let (pieces, received) = mpsc::unbounded_channel();
    let mut cancel = Cancel { tx: tx.clone(), stream: stream.clone() };
    let out = Out::Fleet { args, stream: stream.map(|id| (id, pieces)), answer };
    if tx.send(out).is_err() {
        cancel.disarm();
        let message = format!("{name} disconnected");
        return unsent(refuse(StatusCode::SERVICE_UNAVAILABLE, "link_down", &message));
    }
    let Ok(answered) = answered.await else {
        cancel.disarm();
        let message = format!("{name} disconnected before it answered");
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "link_lost", &message);
    };
    let result = match answered {
        Ok(result) => result,
        Err((code, message)) => {
            cancel.disarm();
            return refused(&name, &code, &message);
        }
    };
    let status = result
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let kind = result.pointer("/headers/content-type").and_then(Value::as_str);
    let retry = result.pointer(&format!("/headers/{RETRY}")).and_then(Value::as_str) == Some("1");
    let marked = |response: Reply| if retry { unsent(response) } else { response };
    if result.get("stream").is_some() {
        return marked(reply(status, kind, Answer { full: None, pieces: Some(received), cancel: Some(cancel) }));
    }
    cancel.disarm();
    match result.get("body").and_then(Value::as_str).map(|body| STANDARD.decode(body)) {
        None => marked(reply(status, kind, Answer::full(Bytes::new()))),
        Some(Ok(body)) => marked(reply(status, kind, Answer::full(body))),
        Some(Err(_)) => {
            let message = format!("{name} answered with a body that is not base64");
            refuse(StatusCode::BAD_GATEWAY, "bad_answer", &message)
        }
    }
}

async fn read_body(mut body: Incoming) -> Option<Option<Vec<u8>>> {
    let mut kept = Vec::new();
    let mut read = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.ok()?.into_data() else { continue };
        read += data.len();
        if read > DRAINED {
            return Some(None);
        }
        if read <= FLEET_BODY {
            kept.extend_from_slice(&data);
        } else {
            kept = Vec::new();
        }
    }
    Some((read <= FLEET_BODY).then_some(kept))
}

fn too_large() -> Reply {
    let message = format!("a fleet request body may be at most {FLEET_BODY} bytes");
    refuse(StatusCode::PAYLOAD_TOO_LARGE, "too_large", &message)
}

fn refused(name: &str, code: &str, message: &str) -> Reply {
    match code {
        "forbidden" => refuse(StatusCode::FORBIDDEN, code, message),
        "bad_args" => refuse(StatusCode::BAD_REQUEST, code, message),
        "not_remote" => {
            let message = format!("update {name} to use it in the fleet: {message}");
            unsent(refuse(StatusCode::BAD_GATEWAY, code, &message))
        }
        "unsent" => unsent(refuse(StatusCode::SERVICE_UNAVAILABLE, "link_down", message)),
        _ => refuse(StatusCode::BAD_GATEWAY, code, message),
    }
}
