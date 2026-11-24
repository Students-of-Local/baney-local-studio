use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snow::HandshakeState;
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use super::devices::{hex, now_ms, random, unhex, write_private};
use super::noise::{self, Channel, Inbound, Joiner, BANNED, BLOB_ID, UNPAIRED};
use super::pairing::{self, PairLink};
use crate::model::{Machine, MachineState, NoticeLevel, Project, PromptDraft, ThreadRow, Update};
use crate::studio::Mailbox;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Message>;
type Stream = SplitStream<Socket>;

const CONNECT: Duration = Duration::from_secs(10);
const FIRST_WAIT_MS: u64 = 1_000;
const LONGEST_WAIT_MS: u64 = 30_000;
const STEADY: Duration = Duration::from_secs(60);
const PING: Duration = Duration::from_secs(20);
const SILENT: Duration = Duration::from_secs(50);
const REFUSED_MS: i64 = 10 * 60 * 1000;
const PAGE: u64 = 500;
const HANG_UP: Duration = Duration::from_secs(2);
pub const TEST_CA: &str = "LOCAL_STUDIO_LINK_TEST_CA";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Route {
    url: String,
    #[serde(default)]
    last_success: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    id: String,
    name: String,
    device: String,
    key: String,
    server: String,
    #[serde(default)]
    urls: Vec<Route>,
    #[serde(default)]
    os: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    channel: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fleet: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    machines: Vec<Saved>,
}

pub(super) enum Piece {
    Data(Vec<u8>),
    End(Option<String>),
}

pub(super) type Answered = Result<Value, (String, String)>;

pub(super) enum Out {
    Request { op: String, args: Value, prompt: Option<PromptDraft>, request: Option<String> },
    Watch(String),
    Unwatch(String),
    Fleet { args: Value, stream: Option<(String, mpsc::UnboundedSender<Piece>)>, answer: oneshot::Sender<Answered> },
    FleetCancel(String),
}

pub(super) enum Lookup {
    Unknown,
    Down(String),
    Up(String, mpsc::UnboundedSender<Out>),
}

struct Link {
    saved: Saved,
    state: MachineState,
    url: String,
    latency: Option<u32>,
    watches: HashMap<String, usize>,
    tx: Option<mpsc::UnboundedSender<Out>>,
    task: Option<JoinHandle<()>>,
    resume: Option<(String, u64)>,
}

impl Link {
    fn new(saved: Saved) -> Self {
        let url = saved.urls.first().map(|route| route.url.clone()).unwrap_or_default();
        Self {
            saved,
            state: MachineState::Connecting,
            url,
            latency: None,
            watches: HashMap::new(),
            tx: None,
            task: None,
            resume: None,
        }
    }

    fn record(&self) -> Machine {
        Machine {
            id: self.saved.id.clone(),
            name: self.saved.name.clone(),
            os: self.saved.os.clone(),
            version: self.saved.version.clone(),
            channel: self.saved.channel.clone(),
            url: self.url.clone(),
            state: self.state.clone(),
            latency_ms: self.latency,
        }
    }

    fn send(&self, out: Out) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(out).is_ok())
    }
}

enum Offer {
    Minting,
    Minted(String),
    Closed,
}

#[derive(Default)]
struct State {
    machines: Vec<Link>,
    selected: Option<(String, String)>,
    offer: Option<(String, Offer)>,
    proxy: Option<JoinHandle<()>>,
}

impl State {
    fn minting(&self, request: &str) -> bool {
        self.offer.as_ref().is_some_and(|(latest, offer)| latest == request && matches!(offer, Offer::Minting))
    }

    fn settle(&mut self, request: &str) {
        if self.offer.as_ref().is_some_and(|(latest, _)| latest == request) {
            self.offer = None;
        }
    }

    fn link(&mut self, machine: &str) -> Option<&mut Link> {
        self.machines.iter_mut().find(|link| link.saved.id == machine)
    }
}

pub(super) struct Inner {
    handle: Handle,
    mailbox: Arc<Mailbox>,
    path: PathBuf,
    pub(super) data_dir: PathBuf,
    state: Mutex<State>,
    minting: Mutex<()>,
}

pub(crate) struct Links(Arc<Inner>);

impl Links {
    pub(crate) fn new(handle: Handle, mailbox: Arc<Mailbox>, data_dir: &Path) -> Self {
        let path = data_dir.join("remote").join("links.json");
        let ledger: Ledger = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                mailbox.notice(NoticeLevel::Error, format!("Could not read {}: {error}", path.display()));
                Ledger::default()
            }),
            Err(_) => Ledger::default(),
        };
        let inner = Arc::new(Inner {
            handle,
            mailbox,
            path,
            data_dir: data_dir.to_path_buf(),
            state: Mutex::new(State::default()),
            minting: Mutex::new(()),
        });
        {
            let mut state = inner.lock();
            for saved in ledger.machines {
                let id = saved.id.clone();
                let mut link = Link::new(saved);
                link.task = Some(inner.handle.spawn(machine_loop(Arc::clone(&inner), id, None)));
                state.machines.push(link);
            }
            if !state.machines.is_empty() {
                inner.publish(&state);
            }
            inner.proxy(&mut state);
        }
        Self(inner)
    }

    pub(crate) fn pair(&self, text: &str) {
        match pairing::parse(text, crate::CHANNEL) {
            Ok(link) => {
                self.0.handle.spawn(pair_task(Arc::clone(&self.0), link, None));
            }
            Err(message) => self.0.mailbox.notice(NoticeLevel::Error, format!("Could not pair: {message}")),
        }
    }

    pub(crate) fn join_fleet(&self, request: String, text: &str) {
        match pairing::parse(text, crate::CHANNEL) {
            Ok(link) => {
                self.0.handle.spawn(pair_task(Arc::clone(&self.0), link, Some(request)));
            }
            Err(message) => {
                self.0.mailbox.push(Update::RequestFailed { request, message: format!("Could not pair: {message}") });
            }
        }
    }

    pub(crate) fn forget(&self, machine: &str) {
        let mut state = self.0.lock();
        let Some(at) = state.machines.iter().position(|link| link.saved.id == machine) else {
            drop(state);
            self.0.mailbox.notice(NoticeLevel::Warning, format!("No linked machine {machine} to forget"));
            return;
        };
        let link = state.machines.remove(at);
        if let Some(task) = link.task {
            task.abort();
        }
        if state.selected.as_ref().is_some_and(|(selected, _)| selected == machine) {
            state.selected = None;
        }
        self.0.save(&state);
        self.0.publish(&state);
        self.0.proxy(&mut state);
        drop(state);
        self.0.mailbox.forget_rows(machine);
        if link.saved.fleet {
            if !self.0.lock().machines.iter().any(|link| link.saved.fleet) {
                super::proxy::drop_token(&self.0.data_dir);
            }
            self.0.handle.spawn(unpeer(Arc::clone(&self.0), machine.to_string(), link.saved.name));
        }
    }

    pub(crate) fn name(&self, machine: &str) -> String {
        self.0.name(machine)
    }

    pub(crate) fn request(
        &self,
        machine: &str,
        op: &str,
        args: Value,
        prompt: Option<PromptDraft>,
        request: Option<String>,
    ) {
        if prompt.as_ref().is_some_and(|prompt| !prompt.files.is_empty()) {
            let message = format!(
                "{} cannot open files attached here: they are paths on this machine. Paste their text or attach images instead",
                self.0.name(machine)
            );
            match request {
                Some(request) => self.0.mailbox.push(Update::RequestFailed { request, message }),
                None => self.0.mailbox.notice(NoticeLevel::Warning, message),
            }
            return;
        }
        let state = self.0.lock();
        let sent = state.machines.iter().find(|link| link.saved.id == machine).map(|link| {
            let out = Out::Request { op: op.to_string(), args, prompt, request: request.clone() };
            (link.send(out), link.saved.name.clone())
        });
        drop(state);
        let message = match sent {
            Some((true, _)) => return,
            Some((false, name)) => format!("{name} is not connected; try again once it is"),
            None => format!("no linked machine {machine}"),
        };
        match request {
            Some(request) => self.0.mailbox.push(Update::RequestFailed { request, message }),
            None => self.0.mailbox.notice(NoticeLevel::Warning, message),
        }
    }

    pub(crate) fn open_pairing(&self, request: String, data_dir: PathBuf) {
        let replaced = self.0.lock().offer.replace((request.clone(), Offer::Minting));
        let inner = Arc::clone(&self.0);
        self.0.handle.spawn_blocking(move || {
            let _serial = inner.minting.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((_, Offer::Minted(id))) = replaced {
                expire(&inner.mailbox, &data_dir, &id);
            }
            if !inner.lock().minting(&request) {
                inner.lock().settle(&request);
                return;
            }
            let minted = pairing::mint(&data_dir, crate::CHANNEL, None, None, &[]);
            let mut state = inner.lock();
            let current = state.minting(&request);
            let update = match minted {
                Ok(minted) if !current => {
                    state.settle(&request);
                    drop(state);
                    expire(&inner.mailbox, &data_dir, &minted.id);
                    return;
                }
                Ok(minted) => {
                    state.offer = Some((request.clone(), Offer::Minted(minted.id)));
                    drop(state);
                    Update::Pairing {
                        request,
                        link: minted.link,
                        url: minted.url,
                        serving: pairing::serving(&data_dir),
                        expires_at: minted.expires_at,
                    }
                }
                Err(error) => {
                    state.settle(&request);
                    drop(state);
                    Update::RequestFailed { request, message: format!("Could not open a pairing: {error:#}") }
                }
            };
            inner.mailbox.push(update);
        });
    }

    pub(crate) fn close_pairing(&self, request: &str, data_dir: PathBuf) {
        let mut state = self.0.lock();
        let Some((latest, offer)) = state.offer.as_mut() else { return };
        if latest != request {
            return;
        }
        if !matches!(offer, Offer::Minted(_)) {
            *offer = Offer::Closed;
            return;
        }
        let Some((_, Offer::Minted(id))) = state.offer.take() else { return };
        drop(state);
        let mailbox = Arc::clone(&self.0.mailbox);
        self.0.handle.spawn_blocking(move || expire(&mailbox, &data_dir, &id));
    }

    pub(crate) fn watch(&self, machine: &str, thread: &str) {
        let mut state = self.0.lock();
        let Some(link) = state.link(machine) else { return };
        let count = link.watches.entry(thread.to_string()).or_default();
        *count += 1;
        if *count == 1 {
            link.send(Out::Watch(thread.to_string()));
        }
    }

    pub(crate) fn unwatch(&self, machine: &str, thread: &str) {
        let mut state = self.0.lock();
        Self::release(&mut state, machine, thread);
    }

    fn release(state: &mut State, machine: &str, thread: &str) {
        let Some(link) = state.link(machine) else { return };
        let Some(count) = link.watches.get_mut(thread) else { return };
        *count -= 1;
        if *count == 0 {
            link.watches.remove(thread);
            link.send(Out::Unwatch(thread.to_string()));
        }
    }

    pub(crate) fn select(&self, target: Option<(&str, &str)>) {
        let mut state = self.0.lock();
        let wanted = target.map(|(machine, thread)| (machine.to_string(), thread.to_string()));
        if state.selected == wanted {
            return;
        }
        if let Some((machine, thread)) = state.selected.take() {
            Self::release(&mut state, &machine, &thread);
        }
        drop(state);
        if let Some((machine, thread)) = target {
            self.watch(machine, thread);
            self.0.lock().selected = wanted;
        }
    }

    pub(crate) fn shutdown(&self) {
        let mut state = self.0.lock();
        if let Some(proxy) = state.proxy.take() {
            proxy.abort();
        }
        for link in &mut state.machines {
            link.tx = None;
            if let Some(task) = link.task.take() {
                task.abort();
            }
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn warn(&self, text: String) {
        self.mailbox.notice(NoticeLevel::Warning, text);
    }

    pub(super) fn fleet_link(&self, machine: &str) -> Lookup {
        let state = self.lock();
        let Some(link) = state.machines.iter().find(|link| link.saved.fleet && link.saved.id == machine) else {
            return Lookup::Unknown;
        };
        match (&link.state, &link.tx) {
            (MachineState::Connected, Some(tx)) => Lookup::Up(link.saved.name.clone(), tx.clone()),
            _ => Lookup::Down(link.saved.name.clone()),
        }
    }

    fn proxy(self: &Arc<Self>, state: &mut State) {
        let wanted = state.machines.iter().any(|link| link.saved.fleet);
        match (&state.proxy, wanted) {
            (None, true) => state.proxy = Some(self.handle.spawn(super::proxy::serve(Arc::clone(self)))),
            (Some(_), false) => {
                if let Some(proxy) = state.proxy.take() {
                    proxy.abort();
                }
            }
            _ => {}
        }
    }

    fn name(&self, machine: &str) -> String {
        self.lock()
            .machines
            .iter()
            .find(|link| link.saved.id == machine)
            .map_or_else(|| machine.to_string(), |link| link.saved.name.clone())
    }

    fn publish(&self, state: &State) {
        self.mailbox.push(Update::Machines { machines: state.machines.iter().map(Link::record).collect() });
    }

    fn save(&self, state: &State) {
        let ledger = Ledger { machines: state.machines.iter().map(|link| link.saved.clone()).collect() };
        let written = serde_json::to_vec_pretty(&ledger)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| write_private(&self.path, &bytes))
            .and_then(|()| {
                let folder = self.path.parent().unwrap_or(Path::new("."));
                crate::platform::set_mode(folder, 0o700).map_err(anyhow::Error::from)
            });
        if let Err(error) = written {
            self.mailbox.notice(NoticeLevel::Error, format!("Could not save linked machines: {error:#}"));
        }
    }

    fn set_state(&self, machine: &str, next: MachineState) {
        let mut state = self.lock();
        let Some(link) = state.link(machine) else { return };
        if matches!((&link.state, &next), (MachineState::Offline { .. }, MachineState::Offline { .. })) {
            return;
        }
        if link.state == next {
            return;
        }
        link.state = next;
        self.publish(&state);
    }

    fn latency(&self, machine: &str, latency: u32) {
        let mut state = self.lock();
        let Some(link) = state.link(machine) else { return };
        if link.latency == Some(latency) {
            return;
        }
        link.latency = Some(latency);
        self.publish(&state);
    }

    fn prepare(&self, machine: &str) -> Option<(Saved, Vec<String>, Value)> {
        let mut state = self.lock();
        let link = state.link(machine)?;
        let mut routes = link.saved.urls.clone();
        routes.sort_by_key(|route| std::cmp::Reverse(route.last_success.unwrap_or(i64::MIN)));
        let urls = routes.into_iter().map(|route| route.url).collect();
        let watch: Vec<&String> = link.watches.keys().collect();
        let resume = match &link.resume {
            Some((epoch, seq)) => json!({ "seq": seq, "epoch": epoch, "watch": watch }),
            None => json!({ "watch": watch }),
        };
        if !matches!(link.state, MachineState::Offline { .. }) {
            link.state = MachineState::Connecting;
            self.publish(&state);
        }
        Some((state.link(machine)?.saved.clone(), urls, resume))
    }

    fn connected(&self, machine: &str, tx: mpsc::UnboundedSender<Out>, url: &str, hello: &Value) -> Option<String> {
        let mut state = self.lock();
        let link = state.link(machine)?;
        let said = |key: &str| hello.pointer(&format!("/machine/{key}")).and_then(Value::as_str).map(str::to_string);
        if let Some(name) = said("name").filter(|name| !name.is_empty()) {
            link.saved.name = name;
        }
        link.saved.os = said("os").unwrap_or_default();
        link.saved.version = said("version").unwrap_or_default();
        link.saved.channel = said("channel").unwrap_or_default();
        let advertised: Vec<String> = hello
            .pointer("/machine/urls")
            .and_then(Value::as_array)
            .map(|urls| urls.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let mut routes: Vec<Route> = advertised
            .into_iter()
            .map(|url| {
                let last_success =
                    link.saved.urls.iter().find(|route| route.url == url).and_then(|route| route.last_success);
                Route { url, last_success }
            })
            .collect();
        match routes.iter_mut().find(|route| route.url == url) {
            Some(route) => route.last_success = Some(now_ms()),
            None => routes.insert(0, Route { url: url.to_string(), last_success: Some(now_ms()) }),
        }
        link.saved.urls = routes;
        link.url = url.to_string();
        link.state = MachineState::Connected;
        link.tx = Some(tx);
        let name = link.saved.name.clone();
        self.save(&state);
        self.publish(&state);
        Some(name)
    }

    fn disconnected(&self, machine: &str, resume: Option<(String, u64)>) {
        let mut state = self.lock();
        let Some(link) = state.link(machine) else { return };
        link.tx = None;
        link.latency = None;
        if resume.is_some() {
            link.resume = resume;
        }
    }

    fn adopt(self: &Arc<Self>, mut saved: Saved, opened: Opened) {
        let id = saved.id.clone();
        let mut state = self.lock();
        if let Some(old) = state.machines.iter().position(|link| link.saved.id == id) {
            let old = state.machines.remove(old);
            saved.fleet |= old.saved.fleet;
            if let Some(task) = old.task {
                task.abort();
            }
        }
        let mut link = Link::new(saved);
        link.url.clone_from(&opened.url);
        link.task = Some(self.handle.spawn(machine_loop(Arc::clone(self), id, Some(opened))));
        state.machines.push(link);
        self.save(&state);
        self.publish(&state);
        self.proxy(&mut state);
    }
}

struct Opened {
    sink: Sink,
    stream: Stream,
    channel: Channel,
    joiner: Joiner,
    hello: Value,
    url: String,
}

enum Failure {
    Closed(Option<u16>),
    Incompatible(u64, u64),
    Broken(String),
}

impl From<snow::Error> for Failure {
    fn from(error: snow::Error) -> Self {
        Self::Broken(format!("the handshake failed: {error}"))
    }
}

impl Failure {
    fn describe(&self) -> String {
        match self {
            Self::Closed(Some(code)) => format!("the machine closed the socket ({code})"),
            Self::Closed(None) => "the socket closed".to_string(),
            Self::Incompatible(protocol, min) => {
                format!(
                    "the machine speaks protocols {min} to {protocol}, and this app speaks {} to {}",
                    noise::MIN_PROTOCOL,
                    noise::PROTOCOL
                )
            }
            Self::Broken(message) => message.clone(),
        }
    }
}

enum Next {
    Binary(Vec<u8>),
    Text(String),
    Closed(Option<u16>),
}

fn expire(mailbox: &Mailbox, data_dir: &Path, id: &str) {
    if let Err(error) = super::devices::Devices::new(data_dir).close_pairing(id) {
        mailbox.notice(NoticeLevel::Error, format!("Could not close the pairing: {error:#}"));
    }
}

async fn next(stream: &mut Stream) -> Next {
    loop {
        return match stream.next().await {
            Some(Ok(Message::Binary(bytes))) => Next::Binary(bytes.to_vec()),
            Some(Ok(Message::Text(text))) => Next::Text(text.to_string()),
            Some(Ok(Message::Close(frame))) => Next::Closed(frame.map(|frame| u16::from(frame.code))),
            Some(Ok(_)) => continue,
            None | Some(Err(_)) => Next::Closed(None),
        };
    }
}

async fn next_by(stream: &mut Stream) -> Result<Next, Failure> {
    tokio::time::timeout(CONNECT, next(stream))
        .await
        .map_err(|_| Failure::Broken("the machine did not answer within 10 s".to_string()))
}

fn incompatible(text: &str) -> Failure {
    let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    if value.get("op").and_then(Value::as_str) == Some("incompatible") {
        let number = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or_default();
        return Failure::Incompatible(number("protocol"), number("min_protocol"));
    }
    Failure::Broken("the machine sent text during the handshake".to_string())
}

async fn send_records(sink: &mut Sink, channel: &Channel, records: Vec<Vec<u8>>) -> bool {
    for record in records {
        let Ok(message) = channel.seal(&record) else { return false };
        if sink.send(Message::binary(message)).await.is_err() {
            return false;
        }
    }
    true
}

pub(crate) fn tls() -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    if let Some(path) = std::env::var_os(TEST_CA) {
        use rustls::pki_types::pem::PemObject;
        let shown = Path::new(&path).display().to_string();
        let certs = rustls::pki_types::CertificateDer::pem_file_iter(&path)
            .map_err(|error| format!("{TEST_CA} {shown}: {error}"))?;
        for cert in certs {
            let cert = cert.map_err(|error| format!("{TEST_CA} {shown}: {error}"))?;
            roots.add(cert).map_err(|error| format!("{TEST_CA} {shown}: {error}"))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("TLS setup failed: {error}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

async fn open_socket(url: &str) -> Result<(Sink, Stream), Failure> {
    let connector = if url.get(..6).is_some_and(|scheme| scheme.eq_ignore_ascii_case("wss://")) {
        Connector::Rustls(tls().map_err(Failure::Broken)?)
    } else {
        Connector::Plain
    };
    let connect = tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector));
    let (socket, _) = tokio::time::timeout(CONNECT, connect)
        .await
        .map_err(|_| Failure::Broken(format!("{url} did not answer within 10 s")))?
        .map_err(|error| Failure::Broken(format!("{url}: {error}")))?;
    Ok(socket.split())
}

async fn greet(
    mut sink: Sink,
    mut stream: Stream,
    state: HandshakeState,
    url: &str,
    resume: Value,
) -> Result<Opened, Failure> {
    let channel = Channel::new(state.into_transport_mode()?);
    let hello = json!({
        "op": "hello",
        "device": { "name": pairing::host_name(), "app": crate::VERSION, "channel": crate::CHANNEL },
        "resume": resume,
    });
    if !send_records(&mut sink, &channel, noise::json_records(&hello.to_string())).await {
        return Err(Failure::Closed(None));
    }
    let mut joiner = Joiner::default();
    loop {
        let bytes = match next_by(&mut stream).await? {
            Next::Binary(bytes) => bytes,
            Next::Text(text) => return Err(incompatible(&text)),
            Next::Closed(code) => return Err(Failure::Closed(code)),
        };
        let record = channel.open(&bytes).map_err(|_| Failure::Broken("the machine's hello did not decrypt".into()))?;
        match joiner.feed(record).map_err(Failure::Broken)? {
            None => {}
            Some(Inbound::Json(text)) => {
                let hello: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let named = hello.pointer("/machine/id").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
                if hello.get("op").and_then(Value::as_str) != Some("hello") || !named {
                    return Err(Failure::Broken("the machine's first frame was not its hello".to_string()));
                }
                return Ok(Opened { sink, stream, channel, joiner, hello, url: url.to_string() });
            }
            Some(Inbound::Chunk { .. }) => return Err(Failure::Broken("a blob before the machine's hello".into())),
        }
    }
}

async fn open_kk(saved: &Saved, url: &str, resume: Value) -> Result<Opened, Failure> {
    let key = unhex(&saved.key).ok_or_else(|| Failure::Broken("the saved device key is not hex".to_string()))?;
    let server = unhex(&saved.server).ok_or_else(|| Failure::Broken("the saved machine key is not hex".to_string()))?;
    let (mut sink, mut stream) = open_socket(url).await?;
    let header = noise::header("KK", &saved.device);
    sink.send(Message::text(header.clone())).await.map_err(|_| Failure::Closed(None))?;
    let mut state = snow::Builder::new(noise::KK.parse()?)
        .local_private_key(&key)?
        .remote_public_key(&server)?
        .prologue(&noise::prologue(&header))?
        .build_initiator()?;
    sink.send(Message::binary(noise::write(&mut state, &[])?)).await.map_err(|_| Failure::Closed(None))?;
    match next_by(&mut stream).await? {
        Next::Binary(bytes) => {
            noise::read(&mut state, &bytes)?;
        }
        Next::Text(text) => return Err(incompatible(&text)),
        Next::Closed(code) => return Err(Failure::Closed(code)),
    }
    greet(sink, stream, state, url, resume).await
}

fn device_id() -> anyhow::Result<String> {
    let bytes = random::<16>()?;
    let text = hex(&bytes);
    Ok(format!("{}-{}-{}-{}-{}", &text[..8], &text[8..12], &text[12..16], &text[16..20], &text[20..]))
}

async fn pair_open(link: &PairLink) -> Result<(Saved, Opened), Failure> {
    let pair = snow::Builder::new(noise::KK.parse()?).generate_keypair()?;
    let device = device_id().map_err(|error| Failure::Broken(error.to_string()))?;
    let (mut sink, mut stream) = open_socket(&link.url).await?;
    let header = noise::header("XXpsk3", &link.id);
    sink.send(Message::text(header.clone())).await.map_err(|_| Failure::Closed(None))?;
    let mut state = snow::Builder::new(noise::XX.parse()?)
        .local_private_key(&pair.private)?
        .psk(3, &link.secret)?
        .prologue(&noise::prologue(&header))?
        .build_initiator()?;
    sink.send(Message::binary(noise::write(&mut state, &[])?)).await.map_err(|_| Failure::Closed(None))?;
    match next_by(&mut stream).await? {
        Next::Binary(bytes) => {
            noise::read(&mut state, &bytes)?;
        }
        Next::Text(text) => return Err(incompatible(&text)),
        Next::Closed(code) => return Err(Failure::Closed(code)),
    }
    let server =
        state.get_remote_static().map(<[u8]>::to_vec).ok_or_else(|| Failure::Broken("no machine key".into()))?;
    let record = json!({
        "id": device,
        "name": pairing::host_name(),
        "platform": std::env::consts::OS,
        "app": crate::VERSION,
        "channel": crate::CHANNEL,
    });
    sink.send(Message::binary(noise::write(&mut state, record.to_string().as_bytes())?))
        .await
        .map_err(|_| Failure::Closed(None))?;
    let opened = greet(sink, stream, state, &link.url, json!({ "watch": [] })).await?;
    let said = |key: &str| opened.hello.pointer(&format!("/machine/{key}")).and_then(Value::as_str).unwrap_or_default();
    let name = Some(said("name")).filter(|name| !name.is_empty()).unwrap_or(&link.name).to_string();
    let saved = Saved {
        id: said("id").to_string(),
        name,
        device,
        key: hex(&pair.private),
        server: hex(&server),
        urls: vec![Route { url: link.url.clone(), last_success: Some(now_ms()) }],
        os: said("os").to_string(),
        version: said("version").to_string(),
        channel: said("channel").to_string(),
        fleet: false,
    };
    Ok((saved, opened))
}

async fn pair_task(inner: Arc<Inner>, link: PairLink, fleet: Option<String>) {
    match pair_open(&link).await {
        Ok((mut saved, opened)) => {
            let token = match fleet.as_ref().map(|_| super::proxy::mint_token(&inner.data_dir)).transpose() {
                Ok(token) => token,
                Err(error) => {
                    let message = format!("Could not create the fleet proxy token: {error:#}");
                    if let Some(request) = fleet {
                        inner.mailbox.push(Update::RequestFailed { request, message });
                    }
                    return;
                }
            };
            saved.fleet = fleet.is_some();
            let (id, name) = (saved.id.clone(), saved.name.clone());
            inner.adopt(saved, opened);
            let (Some(request), Some(token)) = (fleet, token) else { return };
            let body = json!({ "url": super::proxy::peer_url(&id), "via": "link", "token": token });
            match controller_peers(&inner, &body).await {
                Ok(()) => inner.mailbox.push(Update::RequestDone { request, message: id }),
                Err(error) => {
                    let base = crate::fleet::controller_url(&inner.data_dir);
                    let message = format!("Paired with {name}, but the Local controller at {base} did not add it as a fleet peer: {error}. Run `local-studio fleet join` again with a new invite once the controller runs.");
                    inner.warn(message.clone());
                    inner.mailbox.push(Update::RequestFailed { request, message });
                }
            }
        }
        Err(failure) => {
            let reason = match failure {
                Failure::Closed(Some(UNPAIRED)) => {
                    "the machine refused the pairing: the link expired, was used, or a newer one replaced it"
                        .to_string()
                }
                other => other.describe(),
            };
            let message = format!("Pairing with {} failed: {reason}", link.name);
            match fleet {
                Some(request) => inner.mailbox.push(Update::RequestFailed { request, message }),
                None => inner.mailbox.notice(NoticeLevel::Error, message),
            }
        }
    }
}

async fn controller_peers(inner: &Inner, body: &Value) -> Result<(), String> {
    let base = crate::fleet::controller_url(&inner.data_dir);
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = http.post(format!("{base}/api/peers")).json(body);
    if let Some(key) = crate::fleet::key(&inner.data_dir) {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("it answered {}", response.status()))
    }
}

async fn unpeer(inner: Arc<Inner>, machine: String, name: String) {
    let body = json!({ "url": super::proxy::peer_url(&machine), "via": "link", "remove": true });
    if let Err(error) = controller_peers(&inner, &body).await {
        let base = crate::fleet::controller_url(&inner.data_dir);
        inner.warn(format!("Could not remove {name} from the Local controller at {base}: {error}"));
    }
}

fn backoff(attempt: u32) -> Duration {
    let cap = FIRST_WAIT_MS.saturating_mul(1 << attempt.min(16)).min(LONGEST_WAIT_MS);
    let roll = random::<8>().map_or(cap, u64::from_le_bytes);
    Duration::from_millis(roll % (cap + 1))
}

enum End {
    Closed(Option<u16>),
    Lost,
    Forgotten,
}

async fn machine_loop(inner: Arc<Inner>, machine: String, first: Option<Opened>) {
    let mut opened = first;
    let mut attempt = 0u32;
    loop {
        let found = if let Some(opened) = opened.take() {
            Ok(opened)
        } else {
            let Some((saved, urls, resume)) = inner.prepare(&machine) else { return };
            let mut outcome = Err(Failure::Broken("the machine has no address".to_string()));
            for url in urls {
                outcome = open_kk(&saved, &url, resume.clone()).await;
                if matches!(outcome, Ok(_) | Err(Failure::Closed(Some(UNPAIRED | BANNED)) | Failure::Incompatible(..)))
                {
                    break;
                }
            }
            outcome
        };
        let ended = match found {
            Ok(opened) => {
                let started = Instant::now();
                let ended = run(&inner, &machine, opened).await;
                if started.elapsed() >= STEADY {
                    attempt = 0;
                }
                match ended {
                    End::Forgotten => return,
                    End::Closed(code) => Failure::Closed(code),
                    End::Lost => Failure::Closed(None),
                }
            }
            Err(failure) => failure,
        };
        match ended {
            Failure::Closed(Some(UNPAIRED)) => {
                inner.set_state(&machine, MachineState::PairAgain);
                return;
            }
            Failure::Closed(Some(BANNED)) => {
                let until = now_ms() + REFUSED_MS;
                inner.set_state(&machine, MachineState::Refused { until });
                tokio::time::sleep(Duration::from_millis(u64::try_from(REFUSED_MS).unwrap_or_default())).await;
                continue;
            }
            Failure::Incompatible(protocol, min_protocol) => {
                inner.set_state(&machine, MachineState::Incompatible { protocol, min_protocol });
                tokio::time::sleep(Duration::from_millis(LONGEST_WAIT_MS)).await;
                continue;
            }
            _ => inner.set_state(&machine, MachineState::Offline { since: now_ms() }),
        }
        tokio::time::sleep(backoff(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

enum Pending {
    Request { op: String, request: Option<String> },
    Earlier(String),
    Blob,
    Watch(String),
    Fleet { answer: oneshot::Sender<Answered>, stream: Option<String> },
}

struct Paging {
    request: u64,
    items: Vec<Value>,
    start: u64,
    ops: Vec<Value>,
}

struct Session<'a> {
    inner: &'a Arc<Inner>,
    machine: &'a str,
    name: String,
    writes: mpsc::UnboundedSender<Vec<Vec<u8>>>,
    pings: mpsc::UnboundedSender<Vec<Vec<u8>>>,
    next: u64,
    pending: HashMap<u64, Pending>,
    paging: HashMap<String, Paging>,
    pinged: Option<Instant>,
    epoch: Option<String>,
    seq: u64,
    streams: HashMap<String, mpsc::UnboundedSender<Piece>>,
}

struct Abort(tokio::task::AbortHandle);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn write(
    mut sink: Sink,
    channel: Arc<Channel>,
    mut writes: mpsc::UnboundedReceiver<Vec<Vec<u8>>>,
    mut pings: mpsc::UnboundedReceiver<Vec<Vec<u8>>>,
) {
    loop {
        let records = tokio::select! {
            biased;
            Some(records) = pings.recv() => records,
            records = writes.recv() => match records {
                Some(records) => records,
                None => break,
            },
        };
        if !send_records(&mut sink, &channel, records).await {
            return;
        }
    }
    sink.close().await.ok();
}

async fn run(inner: &Arc<Inner>, machine: &str, opened: Opened) -> End {
    let Opened { sink, mut stream, channel, mut joiner, hello, url } = opened;
    let channel = Arc::new(channel);
    let (writes, queued) = mpsc::unbounded_channel();
    let (pings, pinging) = mpsc::unbounded_channel();
    let mut outbound = tokio::spawn(write(sink, Arc::clone(&channel), queued, pinging));
    let _writer = Abort(outbound.abort_handle());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let Some(name) = inner.connected(machine, tx, &url, &hello) else {
        outbound.abort();
        return End::Forgotten;
    };
    let mut session = Session {
        inner,
        machine,
        name,
        writes,
        pings,
        next: 0,
        pending: HashMap::new(),
        paging: HashMap::new(),
        pinged: None,
        epoch: hello.get("epoch").and_then(Value::as_str).map(str::to_string),
        seq: hello.get("seq").and_then(Value::as_u64).unwrap_or_default(),
        streams: HashMap::new(),
    };
    for update in hello.get("snapshot").and_then(Value::as_array).cloned().unwrap_or_default() {
        session.update(update);
    }
    let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
    let silence = tokio::time::sleep(SILENT);
    tokio::pin!(silence);
    let mut unsent = None;
    let mut written = false;
    let ended = loop {
        tokio::select! {
            _ = &mut outbound => {
                written = true;
                break End::Lost;
            }
            arrived = next(&mut stream) => {
                silence.as_mut().reset(Instant::now() + SILENT);
                let bytes = match arrived {
                    Next::Text(_) => break End::Lost,
                    Next::Closed(code) => break End::Closed(code),
                    Next::Binary(bytes) => bytes,
                };
                let Ok(record) = channel.open(&bytes) else { break End::Lost };
                match joiner.feed(record) {
                    Err(_) => break End::Lost,
                    Ok(Some(Inbound::Json(text))) => {
                        if !session.frame(&text) {
                            break End::Lost;
                        }
                    }
                    Ok(_) => {}
                }
            }
            () = &mut silence => break End::Lost,
            out = rx.recv() => {
                let Some(out) = out else { break End::Forgotten };
                let request = match &out {
                    Out::Request { request, .. } => request.clone(),
                    Out::Watch(_) | Out::Unwatch(_) | Out::Fleet { .. } | Out::FleetCancel(_) => None,
                };
                if !session.out(out) {
                    unsent = request;
                    break End::Lost;
                }
            }
            _ = ping.tick() => {
                session.pinged = Some(Instant::now());
                let ping = noise::json_records(&json!({ "op": "ping" }).to_string());
                if session.pings.send(ping).is_err() {
                    break End::Lost;
                }
            }
        }
    };
    let resume = session.epoch.clone().map(|epoch| (epoch, session.seq));
    session.hang_up();
    if !written && !matches!(ended, End::Lost) {
        tokio::time::timeout(HANG_UP, &mut outbound).await.ok();
    }
    outbound.abort();
    rx.close();
    inner.disconnected(machine, resume);
    session.fail(unsent, &mut rx);
    ended
}

fn prefixed(machine: &str, id: &str) -> String {
    format!("@{machine}/{id}")
}

fn tag(value: &mut Value, key: &str, machine: &str) {
    if let Some(Value::String(id)) = value.get_mut(key) {
        *id = prefixed(machine, id);
    }
}

fn tag_rows(rows: &mut Value, machine: &str) {
    for row in rows.as_array_mut().into_iter().flatten() {
        for key in ["id", "project", "fanOut"] {
            tag(row, key, machine);
        }
        row["machine"] = Value::String(machine.to_string());
    }
}

fn rewrite(update: &mut Value, machine: &str) {
    tag(update, "thread", machine);
    tag(update, "project", machine);
    if let Some(info) = update.get_mut("info") {
        tag(info, "thread", machine);
        tag(info, "project", machine);
    }
    for hit in update.get_mut("hits").and_then(Value::as_array_mut).into_iter().flatten() {
        tag(hit, "thread", machine);
        tag(hit, "project", machine);
    }
    for key in ["threads", "projects"] {
        if let Some(rows) = update.get_mut(key) {
            tag_rows(rows, machine);
        }
    }
}

impl Session<'_> {
    fn fail(&mut self, unsent: Option<String>, rx: &mut mpsc::UnboundedReceiver<Out>) {
        let mut failed: Vec<String> = unsent.into_iter().collect();
        let pending = self.pending.drain().filter_map(|(_, pending)| match pending {
            Pending::Request { request, .. } => request,
            Pending::Earlier(_) | Pending::Blob | Pending::Watch(_) | Pending::Fleet { .. } => None,
        });
        let name = self.name.clone();
        let queued = std::iter::from_fn(|| rx.try_recv().ok()).filter_map(|out| match out {
            Out::Request { request, .. } => request,
            Out::Fleet { answer, .. } => {
                answer.send(Err(("unsent".to_string(), format!("{name} disconnected")))).ok();
                None
            }
            Out::Watch(_) | Out::Unwatch(_) | Out::FleetCancel(_) => None,
        });
        for request in pending.chain(queued).collect::<Vec<_>>() {
            if !failed.contains(&request) {
                failed.push(request);
            }
        }
        for request in failed {
            let message = format!("{} disconnected", self.name);
            self.inner.mailbox.push(Update::RequestFailed { request, message });
        }
    }

    fn hang_up(&mut self) {
        self.writes = mpsc::unbounded_channel().0;
        self.pings = mpsc::unbounded_channel().0;
    }

    fn send(&mut self, frame: &Value) -> bool {
        self.writes.send(noise::json_records(&frame.to_string())).is_ok()
    }

    fn ask(&mut self, op: &str, args: Value, pending: Pending) -> bool {
        self.next += 1;
        self.pending.insert(self.next, pending);
        let mut frame = json!({ "id": self.next, "op": op });
        frame["args"] = args;
        self.send(&frame)
    }

    fn load_earlier(&mut self, thread: &str, before: u64) -> bool {
        let args = json!({ "thread": thread, "before": before, "limit": PAGE });
        let sent = self.ask("load_earlier", args, Pending::Earlier(thread.to_string()));
        if let Some(paging) = self.paging.get_mut(thread) {
            paging.request = self.next;
        }
        sent
    }

    fn out(&mut self, out: Out) -> bool {
        match out {
            Out::Watch(thread) => self.ask("watch", json!({ "thread": thread }), Pending::Watch(thread.clone())),
            Out::Unwatch(thread) => {
                self.paging.remove(&thread);
                self.ask("unwatch", json!({ "thread": thread }), Pending::Watch(thread.clone()))
            }
            Out::Fleet { mut args, stream, answer } => {
                let stream = stream.map(|(id, pieces)| {
                    args["stream"] = Value::String(id.clone());
                    self.streams.insert(id.clone(), pieces);
                    id
                });
                self.ask("fleet_http", args, Pending::Fleet { answer, stream })
            }
            Out::FleetCancel(stream) => {
                self.streams.remove(&stream);
                self.next += 1;
                self.send(&json!({ "id": self.next, "op": "fleet_cancel", "args": { "stream": stream } }))
            }
            Out::Request { op, mut args, prompt, request } => {
                if let Some(prompt) = prompt {
                    let mut images = Vec::new();
                    for image in prompt.images {
                        let Ok(blob) = random::<BLOB_ID>() else { return false };
                        let put = json!({ "blob": hex(&blob), "size": image.data.len(), "mime": image.mime_type });
                        if !self.ask("blob_put", put, Pending::Blob) {
                            return false;
                        }
                        for record in noise::blob_records(&blob, &image.data) {
                            if self.writes.send(vec![record]).is_err() {
                                return false;
                            }
                        }
                        images.push(hex(&blob));
                    }
                    args["prompt"] = json!({ "text": prompt.text, "images": images, "files": prompt.files });
                }
                self.ask(&op.clone(), args, Pending::Request { op, request })
            }
        }
    }

    fn frame(&mut self, text: &str) -> bool {
        let Ok(frame) = serde_json::from_str::<Value>(text) else { return true };
        if let Some(id) = frame.get("re").and_then(Value::as_u64) {
            return self.answer(id, &frame);
        }
        match frame.get("op").and_then(Value::as_str) {
            Some("update") => {
                if let Some(seq) = frame.get("seq").and_then(Value::as_u64) {
                    self.seq = seq;
                }
                if let Some(update) = frame.get("update").cloned() {
                    return self.update(update);
                }
            }
            Some("fleet") => {
                if let Some(update) = frame.get("update") {
                    self.fleet(update);
                }
            }
            Some("pong") => {
                if let Some(sent) = self.pinged.take() {
                    let latency = u32::try_from(sent.elapsed().as_millis()).unwrap_or(u32::MAX);
                    self.inner.latency(self.machine, latency);
                }
            }
            Some("error") => {
                let message = frame.pointer("/error/message").and_then(Value::as_str).unwrap_or("an error");
                self.inner.mailbox.notice(NoticeLevel::Warning, format!("{}: {message}", self.name));
            }
            _ => {}
        }
        true
    }

    fn fleet(&mut self, update: &Value) {
        let Some(stream) = update.get("stream").and_then(Value::as_str) else { return };
        match update.get("type").and_then(Value::as_str) {
            Some("fleet_chunk") => {
                let Some(pieces) = self.streams.get(stream) else { return };
                let piece = match update.get("data").and_then(Value::as_str).map(|data| STANDARD.decode(data)) {
                    Some(Ok(data)) => Piece::Data(data),
                    _ => Piece::End(Some(format!("{} sent a fleet chunk that is not base64", self.name))),
                };
                let ended = matches!(piece, Piece::End(_));
                if pieces.send(piece).is_err() || ended {
                    self.streams.remove(stream);
                }
            }
            Some("fleet_end") => {
                if let Some(pieces) = self.streams.remove(stream) {
                    let error = update.get("error").and_then(Value::as_str).map(str::to_string);
                    pieces.send(Piece::End(error)).ok();
                }
            }
            _ => {}
        }
    }

    fn answer(&mut self, id: u64, frame: &Value) -> bool {
        let Some(pending) = self.pending.remove(&id) else { return true };
        if let Pending::Fleet { answer, stream } = pending {
            let answered = match frame.get("error") {
                Some(error) => {
                    let text = |key: &str| error.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                    Err((text("code"), text("message")))
                }
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            if let Some(stream) = stream {
                if !matches!(&answered, Ok(result) if result.get("stream").is_some()) {
                    self.streams.remove(&stream);
                }
            }
            answer.send(answered).ok();
            return true;
        }
        let error = frame.pointer("/error/message").and_then(Value::as_str).map(str::to_string);
        match pending {
            Pending::Earlier(thread) => {
                if self.paging.get(&thread).is_none_or(|paging| paging.request != id) {
                    return true;
                }
                if let Some(message) = error {
                    self.inner.mailbox.notice(
                        NoticeLevel::Warning,
                        format!("Could not load the start of a thread on {}: {message}", self.name),
                    );
                    return self.finish_paging(&thread);
                }
                let start = frame.pointer("/result/start").and_then(Value::as_u64).unwrap_or_default();
                let items = frame.pointer("/result/items").and_then(Value::as_array).cloned().unwrap_or_default();
                let Some(paging) = self.paging.get_mut(&thread) else { return true };
                let got = !items.is_empty();
                paging.items.splice(0..0, items);
                paging.start = start;
                if start > 0 && got {
                    return self.load_earlier(&thread, start);
                }
                self.finish_paging(&thread)
            }
            Pending::Request { op, request } => {
                if let Some(message) = error {
                    let message = format!("{}: {message}", self.name);
                    match request {
                        Some(request) => self.inner.mailbox.push(Update::RequestFailed { request, message }),
                        None => self.inner.mailbox.notice(NoticeLevel::Warning, format!("{op} on {message}")),
                    }
                }
                true
            }
            Pending::Blob => {
                if let Some(message) = error {
                    let message = format!("Could not upload an image to {}: {message}", self.name);
                    self.inner.mailbox.notice(NoticeLevel::Warning, message);
                }
                true
            }
            Pending::Watch(thread) => {
                if let Some(message) = error {
                    let message = format!("Could not follow thread {thread} on {}: {message}", self.name);
                    self.inner.mailbox.notice(NoticeLevel::Warning, message);
                }
                true
            }
            Pending::Fleet { .. } => true,
        }
    }

    fn finish_paging(&mut self, thread: &str) -> bool {
        let Some(paging) = self.paging.remove(thread) else { return true };
        self.emit(json!({ "type": "transcript", "thread": thread, "op": { "type": "reset", "items": paging.items } }));
        for op in paging.ops {
            self.emit(op);
        }
        true
    }

    fn update(&mut self, update: Value) -> bool {
        let kind = update.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        match kind.as_str() {
            "threads" => {
                let mut rows = update.get("threads").cloned().unwrap_or(Value::Null);
                tag_rows(&mut rows, self.machine);
                match serde_json::from_value::<Vec<ThreadRow>>(rows) {
                    Ok(rows) => self.inner.mailbox.remote_threads(self.machine, rows),
                    Err(error) => self.unreadable("threads", &error),
                }
            }
            "projects" => {
                let mut rows = update.get("projects").cloned().unwrap_or(Value::Null);
                tag_rows(&mut rows, self.machine);
                match serde_json::from_value::<Vec<Project>>(rows) {
                    Ok(rows) => self.inner.mailbox.remote_projects(self.machine, rows),
                    Err(error) => self.unreadable("projects", &error),
                }
            }
            "notice" => {
                let text = update.get("text").and_then(Value::as_str).unwrap_or_default();
                let mut update = update.clone();
                update["text"] = Value::String(format!("{}: {text}", self.name));
                self.emit(update);
            }
            "transcript" => {
                let Some(thread) = update.get("thread").and_then(Value::as_str).map(str::to_string) else {
                    return true;
                };
                if update.pointer("/op/type").and_then(Value::as_str) == Some("reset") {
                    self.paging.remove(&thread);
                    let earlier = update.pointer("/op/earlier").and_then(Value::as_u64).unwrap_or_default();
                    if earlier > 0 {
                        let items = update.pointer("/op/items").and_then(Value::as_array).cloned().unwrap_or_default();
                        self.paging
                            .insert(thread.clone(), Paging { request: 0, items, start: earlier, ops: Vec::new() });
                        return self.load_earlier(&thread, earlier);
                    }
                } else if let Some(paging) = self.paging.get_mut(&thread) {
                    paging.ops.push(update);
                    return true;
                }
                self.emit(update);
            }
            "thread"
            | "approvals"
            | "queue"
            | "subagents"
            | "notification"
            | "thread_started"
            | "composer_text"
            | "worktree_setup"
            | "git_status"
            | "request_done"
            | "request_failed"
            | "diff"
            | "pull_requests"
            | "pull_request_detail"
            | "thread_search"
            | "archived_threads"
            | "commands"
            | "skills"
            | "message_image" => self.emit(update),
            _ => {}
        }
        true
    }

    fn emit(&self, mut update: Value) {
        rewrite(&mut update, self.machine);
        if let Ok(update) = serde_json::from_value::<Update>(update) {
            self.inner.mailbox.push(update);
        }
    }

    fn unreadable(&self, what: &str, error: &serde_json::Error) {
        self.inner.mailbox.notice(NoticeLevel::Warning, format!("Could not read {}'s {what}: {error}", self.name));
    }
}
