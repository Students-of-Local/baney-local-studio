use std::fmt::Write;
use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;

use super::devices::{Devices, KEY_BYTES};

pub const DEFAULT_PORT: u16 = 18093;

#[must_use]
pub fn scheme(channel: &str) -> String {
    match channel {
        "stable" | "" => "localstudio".to_string(),
        other => format!("localstudio-{other}"),
    }
}

#[must_use]
pub fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").ok();
        }
    }
    encoded
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let pair = text.get(at + 1..at + 3)?;
                decoded.push(u8::from_str_radix(pair, 16).ok()?);
                at += 3;
            }
            b'+' => {
                decoded.push(b' ');
                at += 1;
            }
            byte => {
                decoded.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

#[must_use]
pub fn host_name() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local-studio".to_string())
}

pub fn tailscale_url() -> Option<String> {
    let output = std::process::Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let status: Value = serde_json::from_slice(&output.stdout).ok()?;
    if status.get("BackendState").and_then(Value::as_str).is_some_and(|state| state != "Running") {
        return None;
    }
    let name = status.pointer("/Self/DNSName")?.as_str()?.trim_end_matches('.');
    (!name.is_empty()).then(|| format!("wss://{name}/v1/ws"))
}

fn serve_settings(data_dir: &Path) -> Value {
    std::fs::read(data_dir.join("remote").join("serve.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

#[must_use]
pub fn serving(data_dir: &Path) -> bool {
    if !data_dir.join("remote").join("serve.json").is_file() {
        return false;
    }
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], serve_port(data_dir)));
    std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(300)).is_ok()
}

pub fn serve_port(data_dir: &Path) -> u16 {
    serve_settings(data_dir)
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_PORT)
}

#[must_use]
pub fn link(channel: &str, url: &str, name: &str, id: &str, secret: &[u8; KEY_BYTES]) -> String {
    format!(
        "{}://pair?url={}&name={}&id={id}#{}",
        scheme(channel),
        encode(url),
        encode(name),
        URL_SAFE_NO_PAD.encode(secret)
    )
}

pub struct Minted {
    pub id: String,
    pub link: String,
    pub url: String,
    pub expires_at: i64,
    pub scopes: Vec<String>,
}

pub fn mint(
    data_dir: &Path,
    channel: &str,
    url: Option<String>,
    name: Option<String>,
    scopes: &[&str],
) -> anyhow::Result<Minted> {
    let settings = serve_settings(data_dir);
    let url = url.or_else(tailscale_url).unwrap_or_else(|| format!("ws://127.0.0.1:{}/v1/ws", serve_port(data_dir)));
    let name = name
        .or_else(|| settings.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_string))
        .unwrap_or_else(host_name);
    let (id, secret, expires_at) = Devices::new(data_dir).open_pairing(scopes)?;
    let scopes = scopes.iter().map(|scope| (*scope).to_string()).collect();
    Ok(Minted { link: link(channel, &url, &name, &id, &secret.0), id, url, expires_at, scopes })
}

#[derive(Debug)]
pub struct PairLink {
    pub url: String,
    pub name: String,
    pub id: String,
    pub secret: [u8; KEY_BYTES],
}

pub fn parse(text: &str, channel: &str) -> Result<PairLink, String> {
    let text = text.trim();
    let own = scheme(channel);
    let Some((given, rest)) = text.split_once("://") else {
        return Err("this is not a Local Studio pairing link".to_string());
    };
    if !given.eq_ignore_ascii_case(&own) {
        return Err(format!(
            "this link is for {given}, and this app pairs only {own} links: pair machines on the same channel"
        ));
    }
    let Some(query) = rest.strip_prefix("pair?") else { return Err("this is not a pairing link".to_string()) };
    let (query, secret) = query.split_once('#').ok_or("the link has no pairing secret after #")?;
    let secret = URL_SAFE_NO_PAD
        .decode(secret.trim())
        .ok()
        .and_then(|bytes| <[u8; KEY_BYTES]>::try_from(bytes).ok())
        .ok_or("the link's pairing secret is not 32 bytes of base64url")?;
    let (mut url, mut name, mut id) = (None, None, None);
    for part in query.split('&') {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let value = decode(value).ok_or_else(|| format!("the link's {key} is not valid"))?;
        match key {
            "url" => url = Some(value),
            "name" => name = Some(value),
            "id" => id = Some(value),
            _ => {}
        }
    }
    let url = url
        .filter(|url| url.starts_with("ws://") || url.starts_with("wss://"))
        .ok_or("the link names no ws:// or wss:// address")?;
    let id = id
        .filter(|id| id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("the link has no pairing id")?;
    let name = name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| "machine".to_string());
    Ok(PairLink { url, name, id, secret })
}
