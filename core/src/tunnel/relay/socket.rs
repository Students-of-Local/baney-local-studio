use std::future::pending;

use futures_util::{SinkExt, StreamExt};
use hyper::body::Bytes;
use hyper::header::{
    HeaderMap, HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_EXTENSIONS, SEC_WEBSOCKET_KEY,
    SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION, UPGRADE,
};
use hyper::Response;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::{Error, Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use super::streams::{pairs, Feed, Out, BAD_ANSWER, HEAD, WS_BINARY, WS_TEXT};
use super::{limits, HANG_UP, RELAY_TAKES};

const NORMAL: u16 = 1000;
const NO_STATUS: u16 = 1005;
const LOST: u16 = 1006;
const TOO_BIG: u16 = 1009;
const INTERNAL: u16 = 1011;
const REASON_BYTES: usize = 123;

pub(super) struct Socket {
    key: String,
    feed: mpsc::UnboundedReceiver<Feed>,
}

pub(super) fn close(code: u64, reason: &str) -> (u16, &str) {
    let code = match u16::try_from(code) {
        Ok(NO_STATUS) => NORMAL,
        Ok(code @ (1000 | 1001 | 1003 | 1007..=1014 | 3000..=4999)) => code,
        _ => INTERNAL,
    };
    let end = (0..=reason.len().min(REASON_BYTES)).rev().find(|end| reason.is_char_boundary(*end));
    (code, &reason[..end.unwrap_or_default()])
}

impl Socket {
    pub(super) fn new(feed: mpsc::UnboundedReceiver<Feed>) -> Self {
        Self { key: generate_key(), feed }
    }

    pub(super) fn ask(&self, headers: &mut HeaderMap) -> Option<()> {
        headers.remove(SEC_WEBSOCKET_EXTENSIONS);
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
        headers.insert(SEC_WEBSOCKET_KEY, HeaderValue::from_str(&self.key).ok()?);
        Some(())
    }

    pub(super) async fn pump<B>(mut self, out: &Out, response: Response<B>) -> Result<(), &'static str> {
        let accept = response.headers().get(SEC_WEBSOCKET_ACCEPT).map(HeaderValue::as_bytes);
        if accept != Some(derive_accept_key(self.key.as_bytes()).as_bytes()) {
            return Err(BAD_ANSWER);
        }
        let chosen = pairs(response.headers().iter().filter(|(name, _)| **name == SEC_WEBSOCKET_PROTOCOL));
        let upgraded = hyper::upgrade::on(response).await.map_err(|_| BAD_ANSWER)?;
        let limits = Some(limits(RELAY_TAKES - HEAD));
        let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, limits).await;
        out.head(101, &chosen).await;
        let (mut sink, mut stream) = socket.split();
        let up = async {
            while let Some(feed) = self.feed.recv().await {
                let held = feed.bytes();
                let Some(message) = feed.message() else { continue };
                let last = message.is_close();
                let sent = sink.send(message).await;
                drop(held);
                if sent.is_err() || last {
                    break;
                }
            }
            pending().await
        };
        let down = async {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Text(text))) => out.relay(WS_TEXT, text.into()).await,
                    Some(Ok(Message::Binary(bytes))) => out.relay(WS_BINARY, bytes).await,
                    Some(Ok(Message::Close(Some(frame)))) => return (frame.code.into(), frame.reason),
                    Some(Ok(Message::Close(None))) => return (NO_STATUS, Utf8Bytes::default()),
                    Some(Ok(_)) => {}
                    Some(Err(Error::Capacity(_))) => return (TOO_BIG, "message too big".into()),
                    Some(Err(_)) | None => return (LOST, Utf8Bytes::default()),
                }
            }
        };
        let (code, reason) = tokio::select! {
            never = up => never,
            closed = down => closed,
        };
        let (code, reason) = close(code.into(), &reason);
        out.last(out.control(json!({ "t": "ws_close", "code": code, "reason": reason }))).await;
        timeout(HANG_UP, stream.next()).await.ok();
        Ok(())
    }
}

impl Feed {
    pub(super) fn close(frame: &Value) -> Self {
        let code = frame.get("code").and_then(Value::as_u64).unwrap_or_default();
        let (code, reason) = close(code, frame.get("reason").and_then(Value::as_str).unwrap_or_default());
        Self::Close(CloseFrame { code: code.into(), reason: reason.into() })
    }

    fn bytes(&self) -> Option<Bytes> {
        match self {
            Self::Text(bytes) | Self::Data(bytes) => Some(bytes.clone()),
            Self::Close(_) | Self::End => None,
        }
    }

    fn message(self) -> Option<Message> {
        match self {
            Self::Text(bytes) => Utf8Bytes::try_from(bytes).ok().map(Message::Text),
            Self::Data(bytes) => Some(Message::Binary(bytes)),
            Self::Close(frame) => Some(Message::Close(Some(frame))),
            Self::End => None,
        }
    }
}
