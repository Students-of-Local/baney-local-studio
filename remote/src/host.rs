use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use local_studio_core::{NoticeLevel, Project, Studio, Subscription, Update};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{watch, Notify};

use crate::devices::{self, Devices};
use crate::hub::{self, Feed, Hub};
use crate::limits::Gate;
use crate::pairing::host_name;
use crate::server::{self, Hosts, Route, Server};
use crate::settings::{self, Effective, Settings};
use crate::{service, CHANNEL, VERSION};

const BIND_WAIT: Duration = Duration::from_secs(10);
const BIND_RETRY: Duration = Duration::from_millis(200);
const RELOAD_EVERY: Duration = Duration::from_secs(2);
const DRAIN_POLL: Duration = Duration::from_millis(20);
const STOP_WITHIN: Duration = Duration::from_secs(2);
const WAIT_EVERY: Duration = Duration::from_secs(1);
const REQUEST_EVERY: Duration = Duration::from_millis(500);
const ANSWER_WITHIN: Duration = Duration::from_secs(25);
const ANSWERS_KEPT: Duration = Duration::from_secs(600);
const FAILURES_KEPT: usize = 64;
const GOING_AWAY: u16 = 1001;
const RESTARTING: u16 = 1012;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Quit,
    HandOver,
}

pub(crate) struct Launch {
    pub studio: Arc<Studio>,
    pub feed: Feed,
    pub notify: Arc<Notify>,
    pub data_dir: PathBuf,
    pub flags: Settings,
    pub reload: Option<Arc<Notify>>,
}

pub(crate) fn configured(data_dir: &Path, flags: &Settings, default_name: &str) -> anyhow::Result<Effective> {
    let file = settings::load(data_dir)?.unwrap_or_default();
    let config = file.merged(flags).effective(default_name);
    anyhow::ensure!(
        config.tunnel_port != Some(config.port),
        "the tunnel port must differ from the local port {}: one listener per route",
        config.port
    );
    Ok(config)
}

async fn bind(port: u16, wait: Duration) -> anyhow::Result<TcpListener> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(BIND_RETRY).await;
            }
            Err(error) => return Err(error).with_context(|| format!("listen on 127.0.0.1:{port}")),
        }
    }
}

fn hosts_of(config: &Effective) -> Hosts {
    Hosts { allowed: config.allow_hosts.clone(), tunnels: config.allow_tunnel_hosts.clone() }
}

pub(crate) async fn run(launch: Launch, until: impl Future<Output = Stop>) -> anyhow::Result<Stop> {
    let Launch { studio, feed, notify, data_dir, flags, reload } = launch;
    let default_name = host_name();
    let config = configured(&data_dir, &flags, &default_name)?;
    let devices = Devices::new(&data_dir);
    let key = devices.static_key()?;
    let machine = json!({
        "id": devices.machine_id()?,
        "name": config.name,
        "os": std::env::consts::OS,
        "version": VERSION,
        "channel": CHANNEL,
        "urls": config.urls,
    });
    tokio::pin!(until);
    let local = tokio::select! {
        bound = bind(config.port, BIND_WAIT) => bound?,
        stop = &mut until => return Ok(stop),
    };
    let tunnel = match config.tunnel_port {
        Some(port) => tokio::select! {
            bound = bind(port, BIND_WAIT) => match bound {
                Ok(listener) => Some((listener, port)),
                Err(error) => {
                    eprintln!("local-studio-serve: could not start the tunnel listener, serving without it: {error:#}");
                    None
                }
            },
            stop = &mut until => return Ok(stop),
        },
        None => None,
    };
    let hub = Hub::new(Arc::clone(&studio), machine, config.hooks.clone())?;
    tokio::spawn(Arc::clone(&hub).pump(notify, feed));
    tokio::spawn(Arc::clone(&hub).reap());
    eprintln!(
        "local-studio-serve: serving {} on http://127.0.0.1:{}{} (pid {}, machine key {})",
        data_dir.display(),
        config.port,
        tunnel.as_ref().map(|(_, port)| format!(", tunnel on http://127.0.0.1:{port}")).unwrap_or_default(),
        std::process::id(),
        &devices::hex(&key.public)[..16]
    );
    let server = Arc::new(Server {
        hub: Arc::clone(&hub),
        devices,
        key,
        port: config.port,
        hosts: RwLock::new(hosts_of(&config)),
        gate: Gate::default(),
        sockets: server::Sockets::default(),
    });
    tokio::spawn(server::revocations(Arc::clone(&server)));
    let (stopping, stopped) = watch::channel(false);
    tokio::spawn(server::listen(local, Route::Local, Arc::clone(&server), stopped.clone()));
    let tunnel_port = tunnel.as_ref().map(|(_, port)| *port);
    if let Some((listener, _)) = tunnel {
        tokio::spawn(server::listen(listener, Route::Tunnel, Arc::clone(&server), stopped.clone()));
    }
    let answering = tokio::spawn(answer_requests(Arc::clone(&studio), data_dir.clone()));
    let watcher = Watcher { data_dir, flags, default_name, server: Arc::clone(&server), stopped, tunnel: tunnel_port };
    let watching = tokio::spawn(watcher.run(config, reload));
    let stop = until.await;
    watching.abort();
    answering.abort();
    stopping.send_replace(true);
    let (code, reason) = match stop {
        Stop::Quit => (GOING_AWAY, "serve stopped"),
        Stop::HandOver => (RESTARTING, "service restart"),
    };
    hub.close_all(code, reason);
    let deadline = tokio::time::Instant::now() + STOP_WITHIN;
    while server.sockets.open() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(DRAIN_POLL).await;
    }
    eprintln!("local-studio-serve: closed every remote socket with {code} ({reason})");
    Ok(stop)
}

struct Watcher {
    data_dir: PathBuf,
    flags: Settings,
    default_name: String,
    server: Arc<Server>,
    stopped: watch::Receiver<bool>,
    tunnel: Option<u16>,
}

impl Watcher {
    async fn run(mut self, mut current: Effective, reload: Option<Arc<Notify>>) {
        let mut broken = false;
        loop {
            match &reload {
                Some(reload) => {
                    tokio::select! {
                        () = tokio::time::sleep(RELOAD_EVERY) => {}
                        () = reload.notified() => eprintln!("local-studio-serve: SIGHUP: rereading serve.json"),
                    }
                }
                None => tokio::time::sleep(RELOAD_EVERY).await,
            }
            let next = match configured(&self.data_dir, &self.flags, &self.default_name) {
                Ok(next) => {
                    broken = false;
                    next
                }
                Err(error) => {
                    if !broken {
                        eprintln!("local-studio-serve: kept the running settings: {error:#}");
                    }
                    broken = true;
                    continue;
                }
            };
            self.start_tunnel(&next).await;
            if next == current {
                continue;
            }
            if next.port != current.port {
                eprintln!(
                    "local-studio-serve: serve.json asks for port {}; the local listener stays on {} until serve restarts",
                    next.port, current.port
                );
            }
            self.server.set_hosts(hosts_of(&next));
            self.server.hub.set_machine(&next.name, &next.urls);
            self.server.hub.set_hooks(next.hooks.clone());
            eprintln!(
                "local-studio-serve: reloaded serve.json: hosts {:?}, tunnel hosts {:?}, urls {:?}, {} notify hooks",
                next.allow_hosts,
                next.allow_tunnel_hosts,
                next.urls,
                next.hooks.len()
            );
            current = next;
        }
    }

    async fn start_tunnel(&mut self, config: &Effective) {
        if self.tunnel.is_some() || config.allow_tunnel_hosts.is_empty() {
            return;
        }
        let Some(port) = config.tunnel_port else { return };
        match bind(port, Duration::ZERO).await {
            Ok(listener) => {
                eprintln!("local-studio-serve: a tunnel host was added: tunnel on http://127.0.0.1:{port}");
                self.tunnel = Some(port);
                tokio::spawn(server::listen(listener, Route::Tunnel, Arc::clone(&self.server), self.stopped.clone()));
            }
            Err(error) => eprintln!("local-studio-serve: could not start the tunnel listener: {error:#}"),
        }
    }
}

#[derive(Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    applied: bool,
}

pub(crate) fn requests_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("remote").join("requests")
}

pub(crate) fn write_private(path: &Path, value: &Value) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let partial = path.with_extension("tmp");
    std::fs::remove_file(&partial).ok();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .with_context(|| format!("create {}", partial.display()))?;
    file.write_all(value.to_string().as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, path).with_context(|| format!("move {} into place", partial.display()))
}

fn pending(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return false };
    name.strip_suffix(".json").is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn private_to(path: &Path, me: u32) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.uid() == me && metadata.permissions().mode() & 0o777 == 0o600
    })
}

struct Requests {
    studio: Arc<Studio>,
    subscription: Subscription,
    notify: Arc<Notify>,
    projects: Vec<Project>,
    failures: VecDeque<String>,
}

impl Requests {
    fn drain(&mut self) {
        for update in self.subscription.tick() {
            match update {
                Update::Projects { projects } => {
                    self.projects = projects.into_iter().filter(|project| project.machine.is_none()).collect();
                }
                Update::Notice { level: NoticeLevel::Error, text } => {
                    if self.failures.len() == FAILURES_KEPT {
                        self.failures.pop_front();
                    }
                    self.failures.push_back(text);
                }
                _ => {}
            }
        }
    }

    fn failure(&mut self, fails: &impl Fn(&str) -> bool) -> Option<String> {
        let index = self.failures.iter().position(|text| fails(text))?;
        self.failures.remove(index)
    }

    fn forget(&mut self, fails: &impl Fn(&str) -> bool) {
        self.failures.retain(|text| !fails(text));
    }

    fn known(&self, project: &str) -> Option<&Project> {
        self.projects.iter().find(|known| known.id == project || known.path == project).or_else(|| {
            let wanted = std::fs::canonicalize(project).ok()?;
            self.projects.iter().find(|known| std::fs::canonicalize(&known.path).is_ok_and(|path| path == wanted))
        })
    }

    async fn until(
        &mut self,
        fails: &impl Fn(&str) -> bool,
        wait: Duration,
        done: impl Fn(&Self) -> bool,
    ) -> Option<Result<(), String>> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            self.drain();
            if done(self) {
                return Some(Ok(()));
            }
            if let Some(failure) = self.failure(fails) {
                return Some(Err(failure));
            }
            if tokio::time::timeout_at(deadline, self.notify.notified()).await.is_err() {
                return None;
            }
        }
    }

    async fn answer(&mut self, file: &Path, request: Request) -> Option<Value> {
        self.drain();
        let again = request.applied;
        let path = request.path.unwrap_or_default();
        let wait = if again { Duration::ZERO } else { ANSWER_WITHIN };
        let (ok, message) = match request.op.as_str() {
            "list_projects" => (true, format!("{} projects", self.projects.len())),
            "add_project" if self.known(&path).is_some() => {
                (true, if again { format!("added {path}") } else { format!("{path} is already a project") })
            }
            "add_project" => {
                let fails = |text: &str| {
                    text == format!("{path} is not a folder.") || text.starts_with(&format!("Cannot add {path}: "))
                };
                if !again {
                    self.forget(&fails);
                    mark_applied(file, &request.op, &path);
                    self.studio.add_project(path.clone());
                }
                match self.until(&fails, wait, |requests| requests.known(&path).is_some()).await? {
                    Ok(()) => (true, format!("added {path}")),
                    Err(message) => (false, message),
                }
            }
            "remove_project" if again && self.known(&path).is_none() => (true, format!("removed {path}")),
            "remove_project" => match self.known(&path).map(|known| known.id.clone()) {
                None => (false, format!("{path} is not a project")),
                Some(id) => {
                    let fails = |text: &str| text == format!("Unknown project {id}.");
                    if !again {
                        self.forget(&fails);
                        mark_applied(file, &request.op, &path);
                        self.studio.remove_project(id.clone());
                    }
                    match self.until(&fails, wait, |requests| requests.known(&id).is_none()).await? {
                        Ok(()) => (true, format!("removed {path}")),
                        Err(message) => (false, message),
                    }
                }
            },
            other => (false, format!("unknown request {other:?}")),
        };
        Some(json!({ "ok": ok, "message": message, "projects": self.projects }))
    }
}

fn mark_applied(file: &Path, op: &str, path: &str) {
    if let Err(error) = write_private(file, &json!({ "op": op, "path": path, "applied": true })) {
        eprintln!("local-studio-serve: could not mark {} applied: {error:#}", file.display());
    }
}

fn read_request(path: &Path) -> anyhow::Result<Request> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn answer_requests(studio: Arc<Studio>, data_dir: PathBuf) {
    let Some(me) = service::uid().ok().and_then(|uid| uid.parse::<u32>().ok()) else {
        eprintln!("local-studio-serve: could not read this user's id, so project requests go unanswered");
        return;
    };
    let folder = requests_dir(&data_dir);
    let (waker, notify) = hub::signal();
    let mut requests = Requests {
        subscription: studio.subscribe(waker),
        studio,
        notify,
        projects: Vec::new(),
        failures: VecDeque::new(),
    };
    loop {
        tokio::time::sleep(REQUEST_EVERY).await;
        requests.drain();
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        let files: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
        for done in files.iter().filter(|path| path.to_string_lossy().ends_with(".done.json")) {
            if modified(done).elapsed().is_ok_and(|age| age > ANSWERS_KEPT) {
                std::fs::remove_file(done).ok();
            }
        }
        let mut waiting: Vec<PathBuf> = files.into_iter().filter(|path| pending(path)).collect();
        waiting.sort_by_cached_key(|path| (modified(path), path.clone()));
        for path in waiting {
            if !private_to(&path, me) {
                continue;
            }
            let answer = match read_request(&path) {
                Ok(request) => {
                    let again = request.applied;
                    let Some(answer) = requests.answer(&path, request).await else {
                        if !again {
                            eprintln!(
                                "local-studio-serve: the core has not applied {} within {} s; waiting on it",
                                path.display(),
                                ANSWER_WITHIN.as_secs()
                            );
                        }
                        continue;
                    };
                    answer
                }
                Err(error) => {
                    json!({ "ok": false, "message": format!("unreadable request: {error:#}"), "projects": [] })
                }
            };
            match write_private(&path.with_extension("done.json"), &answer) {
                Ok(()) => {
                    std::fs::remove_file(&path).ok();
                }
                Err(error) => eprintln!("local-studio-serve: could not answer {}: {error:#}", path.display()),
            }
        }
    }
}

pub struct Handle {
    stop: Option<watch::Sender<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send_replace(true);
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.halt();
    }
}

enum Session {
    Stopped,
    Ended,
    Failed(String),
}

async fn settled(stop: &mut watch::Receiver<bool>, mut done: impl FnMut() -> bool) -> bool {
    loop {
        if *stop.borrow() {
            return false;
        }
        if done() {
            return true;
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            () = tokio::time::sleep(WAIT_EVERY) => {}
        }
    }
}

fn ports(config: &Effective) -> (u16, Option<u16>) {
    (config.port, config.tunnel_port)
}

async fn session(studio: &Arc<Studio>, data_dir: &Path, stop: &mut watch::Receiver<bool>) -> Session {
    if !settled(stop, || settings::exists(data_dir)).await {
        return Session::Stopped;
    }
    let name = host_name();
    let started = configured(data_dir, &Settings::default(), &name).ok().as_ref().map(ports);
    let (waker, notify) = hub::signal();
    let feed = Feed::Subscription(studio.subscribe(waker));
    let launch = Launch {
        studio: Arc::clone(studio),
        feed,
        notify,
        data_dir: data_dir.to_path_buf(),
        flags: Settings::default(),
        reload: None,
    };
    let served = run(launch, async {
        let mut gone = false;
        let changed = settled(stop, || {
            gone = !settings::exists(data_dir);
            gone || configured(data_dir, &Settings::default(), &name)
                .ok()
                .as_ref()
                .map(ports)
                .is_some_and(|now| Some(now) != started)
        })
        .await;
        if !changed {
            return Stop::HandOver;
        }
        if gone {
            eprintln!("local-studio-serve: serve.json was removed: stopping the in-process server");
            return Stop::Quit;
        }
        eprintln!("local-studio-serve: serve.json changed the port: restarting the in-process server");
        Stop::HandOver
    })
    .await;
    if *stop.borrow() {
        return Session::Stopped;
    }
    match served {
        Ok(_) => Session::Ended,
        Err(error) => {
            let message = format!("{error:#}");
            tokio::select! {
                _ = stop.changed() => {}
                () = tokio::time::sleep(WAIT_EVERY) => {}
            }
            Session::Failed(message)
        }
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("local-studio-remote")
        .enable_all()
        .build()
}

pub fn start(studio: Arc<Studio>, data_dir: Option<&Path>) -> anyhow::Result<Option<Handle>> {
    let explicit = data_dir.map(|dir| dir.to_string_lossy().into_owned());
    let data_dir = local_studio_core::data_dir(explicit.as_deref()).map_err(|message| anyhow::anyhow!(message))?;
    let mut first = Some(runtime()?);
    let (stop, mut stopped) = watch::channel(false);
    let thread = std::thread::Builder::new().name("local-studio-remote".to_string()).spawn(move || {
        let mut reported: Option<String> = None;
        loop {
            let runtime = match first.take().map_or_else(runtime, Ok) {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("local-studio-serve: the in-process server stopped: {error:#}");
                    return;
                }
            };
            let outcome = runtime.block_on(session(&studio, &data_dir, &mut stopped));
            runtime.shutdown_timeout(STOP_WITHIN);
            match outcome {
                Session::Stopped => return,
                Session::Ended => reported = None,
                Session::Failed(message) => {
                    if reported.as_ref() != Some(&message) {
                        eprintln!("local-studio-serve: the in-process server stopped: {message}");
                        reported = Some(message);
                    }
                }
            }
        }
    })?;
    Ok(Some(Handle { stop: Some(stop), thread: Some(thread) }))
}
