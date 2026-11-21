use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::{Capability, Consent, Grants, Plugin};

pub const SOCKET_ENV: &str = "LOCAL_STUDIO_ASK";
pub const TOKEN_ENV: &str = "LOCAL_STUDIO_ASK_TOKEN";
pub const THREAD_ENV: &str = "LOCAL_STUDIO_THREAD";
pub const PID_ENV: &str = "LOCAL_STUDIO_ASK_PID";
pub const TOKENS_FILE: &str = "ask-tokens.json";
pub const NOTE_TYPE: &str = "local-studio-ask";
pub const MAX_LINE: usize = 16 * 1024;
pub const MAX_TEXT: usize = 2000;
pub const MAX_CHOICES: usize = 8;
pub const MAX_CHOICE: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Answered(String),
    Cancelled,
    Refused(String),
}

impl Outcome {
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Outcome::Answered(answer) => json!({ "status": "answered", "answer": answer }),
            Outcome::Cancelled => json!({ "status": "cancelled" }),
            Outcome::Refused(error) => json!({ "status": "refused", "error": error }),
        }
    }
}

pub struct Ask {
    pub plugin: String,
    pub thread: String,
    pub request: String,
    pub text: String,
    pub choices: Vec<String>,
    pub reply: oneshot::Sender<Outcome>,
}

pub enum Event {
    Ask(Ask),
    Gone { thread: String, request: String },
    Replaced(Option<String>),
}

#[derive(Clone, Default)]
pub struct Tokens {
    inner: Arc<Mutex<TokenTable>>,
}

#[derive(Default)]
struct TokenTable {
    file: Option<PathBuf>,
    issued: HashMap<(String, String), String>,
    holders: HashMap<String, (String, String)>,
}

#[derive(Deserialize)]
struct Saved {
    plugin: String,
    thread: String,
    token: String,
}

impl TokenTable {
    fn insert(&mut self, key: (String, String), token: String) {
        self.holders.insert(digest(&token), key.clone());
        self.issued.insert(key, token);
    }

    fn save(&self) -> Result<(), String> {
        let Some(file) = &self.file else { return Ok(()) };
        let saved: Vec<Value> = self
            .issued
            .iter()
            .map(|((plugin, thread), token)| json!({ "plugin": plugin, "thread": thread, "token": token }))
            .collect();
        crate::workspace::providers::write_private(file, &Value::Array(saved))
    }
}

impl Tokens {
    #[must_use]
    pub fn load(data_dir: &Path) -> Self {
        let file = data_dir.join(TOKENS_FILE);
        let saved: Vec<Saved> =
            std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let mut table = TokenTable { file: Some(file), ..TokenTable::default() };
        for Saved { plugin, thread, token } in saved {
            table.insert((plugin, thread), token);
        }
        Self { inner: Arc::new(Mutex::new(table)) }
    }

    pub fn issue(&self, plugin: &str, thread: &str) -> Result<String, String> {
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (plugin.to_string(), thread.to_string());
        if let Some(token) = table.issued.get(&key) {
            return Ok(token.clone());
        }
        let mut bytes = [0_u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|error| format!("no random token: {error}"))?;
        let token = super::install::hex(&bytes);
        table.insert(key.clone(), token.clone());
        if let Err(error) = table.save() {
            table.issued.remove(&key);
            table.holders.remove(&digest(&token));
            return Err(error);
        }
        Ok(token)
    }

    #[must_use]
    pub fn holder(&self, token: &str) -> Option<(String, String)> {
        let table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        table.holders.get(&digest(token)).cloned()
    }

    pub fn retain(&self, keep: impl Fn(&str, &str) -> bool) {
        let mut table = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let before = table.issued.len();
        table.issued.retain(|(plugin, thread), _| keep(plugin, thread));
        table.holders.retain(|_, (plugin, thread)| keep(plugin, thread));
        if table.issued.len() != before {
            let _ = table.save();
        }
    }
}

fn digest(token: &str) -> String {
    super::install::hex(&Sha256::digest(token.as_bytes()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: Option<String>,
    thread: Option<String>,
    text: String,
    #[serde(default)]
    choices: Vec<String>,
}

pub fn parse(line: &[u8], tokens: &Tokens) -> Result<(String, String, String, Vec<String>), String> {
    let request: Request = serde_json::from_slice(line).map_err(|error| format!("bad request: {error}"))?;
    let token = request.token.filter(|token| !token.is_empty()).ok_or("a question needs the plugin's token")?;
    let (plugin, thread) = tokens.holder(&token).ok_or("unknown token")?;
    if request.thread.as_ref().is_some_and(|asked| *asked != thread) {
        return Err("the token belongs to another thread".to_string());
    }
    let text = request.text.trim().to_string();
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return Err(format!("text must be 1 to {MAX_TEXT} characters"));
    }
    if request.choices.len() > MAX_CHOICES {
        return Err(format!("at most {MAX_CHOICES} choices"));
    }
    let mut choices: Vec<String> = Vec::with_capacity(request.choices.len());
    for choice in request.choices {
        let choice = choice.trim().to_string();
        if choice.is_empty() || choice.chars().count() > MAX_CHOICE {
            return Err(format!("each choice must be 1 to {MAX_CHOICE} characters"));
        }
        if choices.contains(&choice) {
            return Err(format!("duplicate choice: {choice}"));
        }
        choices.push(choice);
    }
    Ok((plugin, thread, text, choices))
}

#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
    let preferred = data_dir.join("ask.sock");
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    let tag: String = digest(&data_dir.to_string_lossy()).chars().take(12).collect();
    std::env::temp_dir().join(format!("ls-ask-{tag}.sock"))
}

#[must_use]
pub fn asks(plugin: &Plugin, grants: &Grants) -> bool {
    plugin.asks(Capability::AskUser) && plugin.asks(Capability::McpServer) && grants.consent(plugin) == Consent::Ready
}

#[must_use]
pub fn acp_servers(
    plugins: &[Plugin],
    grants: &Grants,
    socket: &Path,
    thread: &str,
    tokens: &Tokens,
    foreign: &[String],
) -> (Vec<Value>, Vec<String>) {
    let asking: Vec<Plugin> = plugins.iter().filter(|plugin| asks(plugin, grants)).cloned().collect();
    let mut servers = Vec::new();
    let mut errors = Vec::new();
    for (name, entry) in super::mcp_servers(&asking, grants) {
        let Some(plugin) = asking.iter().find(|plugin| plugin.mcp_name() == name) else { continue };
        if foreign.contains(&name) {
            continue;
        }
        if plugin.mcp.as_ref().is_some_and(|mcp| !mcp.env.is_empty()) {
            errors.push(format!(
                "{name} cannot ask you questions: its [mcp] env names host variables, and Eukhe resolves those only in settings.json entries"
            ));
            continue;
        }
        let token = match tokens.issue(&plugin.id, thread) {
            Ok(token) => token,
            Err(error) => {
                errors.push(format!("{name} cannot ask you questions: {error}"));
                continue;
            }
        };
        let env = json!({
            super::NO_BYTECODE.0: "1",
            SOCKET_ENV: socket.to_string_lossy(),
            TOKEN_ENV: token,
            THREAD_ENV: thread,
            PID_ENV: std::process::id().to_string(),
        });
        servers.push(json!({
            "type": "stdio",
            "name": name,
            "command": entry["command"],
            "args": entry["args"],
            "cwd": entry["cwd"],
            "env": env,
        }));
    }
    (servers, errors)
}

#[cfg(unix)]
pub use server::serve;

#[cfg(unix)]
mod server {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::{oneshot, Semaphore};

    use super::{parse, Ask, Event, Outcome, Tokens, MAX_LINE};

    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    const CHECK_EVERY: Duration = Duration::from_secs(1);
    const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
    const MAX_CONNECTIONS: usize = 64;
    const HALF_CLOSED: &str =
        "the plugin closed its sending side, so the question was withdrawn; keep the connection open until the reply";
    static NEXT: AtomicU64 = AtomicU64::new(1);

    type Mark = (u64, u64, i64, i64);

    fn mark(path: &Path) -> Option<Mark> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some((meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()))
    }

    fn bind(path: &Path, replaced: bool) -> std::io::Result<(UnixListener, Mark, u32)> {
        if std::fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.file_type().is_socket() || (replaced && !meta.is_dir()))
        {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let meta = std::fs::symlink_metadata(path)?;
        let held = (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec());
        let probe = std::os::unix::net::UnixStream::connect(path)?;
        probe.set_nonblocking(true)?;
        let peer = UnixStream::from_std(probe)?.peer_cred()?.pid();
        if peer != i32::try_from(std::process::id()).ok() || mark(path) != Some(held) {
            return Err(std::io::Error::other("another process took the path while Local Studio made it"));
        }
        Ok((listener, held, meta.uid()))
    }

    pub fn serve(
        path: &Path,
        tokens: Tokens,
        events: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> std::io::Result<tokio::task::JoinHandle<()>> {
        let (mut listener, mut held, own) = bind(path, false)?;
        let path = path.to_path_buf();
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        Ok(tokio::spawn(async move {
            let mut tick = tokio::time::interval(CHECK_EVERY);
            let (mut warned, mut failing, mut calm) = (false, false, 0_u32);
            loop {
                let (accepted, ticked) = tokio::select! {
                    accepted = listener.accept() => if let Ok((stream, _)) = accepted {
                        (Some(stream), false)
                    } else {
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        (None, false)
                    },
                    _ = tick.tick() => (None, true),
                };
                if mark(&path) == Some(held) {
                    calm = calm.saturating_add(u32::from(ticked));
                    if calm >= 2 {
                        (warned, failing) = (false, false);
                    }
                } else {
                    calm = 0;
                    match bind(&path, true) {
                        Ok((fresh, fresh_mark, _)) => {
                            (listener, held) = (fresh, fresh_mark);
                            if !warned {
                                warned = true;
                                events(Event::Replaced(None));
                            }
                        }
                        Err(error) if !failing => {
                            failing = true;
                            events(Event::Replaced(Some(error.to_string())));
                        }
                        Err(_) => {}
                    }
                }
                let Some(stream) = accepted else { continue };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
                let (tokens, events) = (tokens.clone(), Arc::clone(&events));
                tokio::spawn(async move {
                    connection(stream, own, &tokens, &events).await;
                    drop(permit);
                });
            }
        }))
    }

    async fn connection(stream: UnixStream, own: u32, tokens: &Tokens, events: &Arc<dyn Fn(Event) + Send + Sync>) {
        if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(own) {
            return;
        }
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read.take(MAX_LINE as u64));
        let mut line = Vec::new();
        let read_line = tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut line)).await;
        let outcome = match read_line {
            Ok(Ok(_)) if line.ends_with(b"\n") => match parse(&line, tokens) {
                Ok((plugin, thread, text, choices)) => {
                    let request = format!("ask-{}", NEXT.fetch_add(1, Ordering::Relaxed));
                    let (reply, answer) = oneshot::channel();
                    events(Event::Ask(Ask {
                        plugin,
                        thread: thread.clone(),
                        request: request.clone(),
                        text,
                        choices,
                        reply,
                    }));
                    let mut rest = reader.into_inner().into_inner();
                    let mut sink = [0_u8; 256];
                    let mut answer = answer;
                    loop {
                        tokio::select! {
                            outcome = &mut answer => break outcome.unwrap_or(Outcome::Cancelled),
                            read = rest.read(&mut sink) => match read {
                                Ok(0) => {
                                    events(Event::Gone { thread, request });
                                    break Outcome::Refused(HALF_CLOSED.to_string());
                                }
                                Err(_) => {
                                    events(Event::Gone { thread, request });
                                    return;
                                }
                                Ok(_) => {}
                            },
                        }
                    }
                }
                Err(error) => Outcome::Refused(error),
            },
            Ok(Ok(_)) => Outcome::Refused(format!("send one JSON line under {MAX_LINE} bytes")),
            Ok(Err(error)) => Outcome::Refused(format!("read: {error}")),
            Err(_) => Outcome::Refused("no request within 5 s".to_string()),
        };
        let mut text = outcome.to_value().to_string();
        text.push('\n');
        let _ = write.write_all(text.as_bytes()).await;
        let _ = write.shutdown().await;
    }
}
