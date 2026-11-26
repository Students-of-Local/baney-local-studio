use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Block, Connection, CustomProvider, DaemonLaunch, Delivery, EndpointPreset, Item, ItemKind, LocalEngine,
    ModelOption, PromptDraft, ProviderApi, ProviderCredential, ProviderKey, ProviderKind, ProviderModel,
    ProviderStatus, Studio, StudioConfig, ThreadRow, ThreadSettings, ThreadStatus, TranscriptOp, Update, UsagePeriod,
    UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};

#[path = "support/real_model.rs"]
mod real_model;

const STEP_TIMEOUT: Duration = Duration::from_mins(15);
const KEY: &str = "sk-stub-local-smoke-0001";
const WRONG_KEY: &str = "sk-stub-wrong-0002";
const OPENAI_KEY: &str = "sk-openai-smoke-not-real";

#[derive(Default)]
struct Signal {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Waker for Signal {
    fn wake(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.ready.notify_one();
    }
}

impl Signal {
    fn wait(&self, timeout: Duration) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
}

#[derive(Default)]
struct View {
    connection: Option<Connection>,
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    started: HashMap<String, String>,
    transcripts: HashMap<String, Vec<Item>>,
    models: Vec<ModelOption>,
    providers: Option<Vec<ProviderStatus>>,
    engines: Vec<LocalEngine>,
    errors: Vec<String>,
    discovered: Vec<(String, String, Vec<ProviderModel>)>,
    thread_models: HashMap<String, String>,
    local: Vec<String>,
    done: Vec<(String, String)>,
    failed: Vec<(String, String)>,
    usage: Option<UsageReport>,
}

struct Original {
    providers: Value,
    keys: BTreeSet<String>,
}

impl View {
    fn answered(&self, request: &str) -> bool {
        self.failure(request).is_some()
            || self.done.iter().any(|(id, _)| id == request)
            || self.discovered.iter().any(|(id, _, _)| id == request)
            || self.local.iter().any(|id| id == request)
    }

    fn failure(&self, request: &str) -> Option<&str> {
        self.failed.iter().find(|(id, _)| id == request).map(|(_, message)| message.as_str())
    }

    fn provider(&self, id: &str) -> Option<&ProviderStatus> {
        self.providers.as_ref()?.iter().find(|provider| provider.id == id)
    }

    fn has_model(&self, provider: &str, model: &str) -> bool {
        self.models.iter().any(|option| option.provider == provider && option.id == model && option.configured)
    }

    fn answer(&self, thread: &str) -> String {
        self.transcripts
            .get(thread)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match &item.kind {
                        ItemKind::Assistant { blocks, .. } => Some(blocks),
                        ItemKind::User { .. }
                        | ItemKind::Tool { .. }
                        | ItemKind::TurnSummary { .. }
                        | ItemKind::Notice { .. }
                        | ItemKind::Compaction { .. } => None,
                    })
                    .flatten()
                    .filter_map(|block| match block {
                        Block::Text { text } => Some(text.as_str()),
                        Block::Thinking { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn idle(&self, thread: &str) -> bool {
        self.threads
            .iter()
            .any(|row| row.id == thread && !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    next: u32,
}

impl Harness {
    fn new(config: StudioConfig) -> anyhow::Result<Self> {
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        Ok(Self { studio, signal, view: View::default(), next: 0 })
    }

    fn request(&mut self) -> String {
        self.next += 1;
        format!("providers-{}", self.next)
    }

    fn pump_until(&mut self, step: &str, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            for update in self.studio.tick() {
                self.apply(update)?;
            }
            if done(&self.view) {
                println!("ok   {step}");
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                for (thread, items) in &self.view.transcripts {
                    println!("     {thread} on {:?}", self.view.thread_models.get(thread));
                    for item in items {
                        println!("     {thread}: {:?}", item.kind);
                    }
                }
                bail!("timed out waiting for: {step}");
            };
            self.signal.wait(left.min(Duration::from_millis(500)));
        }
    }

    fn answer(&mut self, step: &str, request: &str) -> anyhow::Result<()> {
        let id = request.to_string();
        self.pump_until(step, move |view| view.answered(&id))?;
        if let Some(message) = self.view.failure(request) {
            bail!("{step} failed: {message}");
        }
        Ok(())
    }

    fn settle(&mut self, period: Duration) -> anyhow::Result<()> {
        let until = Instant::now() + period;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            for update in self.studio.tick() {
                self.apply(update)?;
            }
            self.signal.wait(left.min(Duration::from_millis(250)));
        }
        Ok(())
    }

    fn failure(&mut self, step: &str, request: &str, expected: &str) -> anyhow::Result<()> {
        let id = request.to_string();
        self.pump_until(step, move |view| view.answered(&id))?;
        let message = self.view.failure(request).context(format!("{step}: expected a failure"))?.to_string();
        ensure!(message.contains(expected), "{step}: unexpected message {message:?}");
        ensure!(!message.contains(KEY) && !message.contains(WRONG_KEY), "{step}: the error leaked a key");
        println!("     surfaced: {message}");
        Ok(())
    }

    fn start(&mut self, draft: &str, project: &str, text: &str, settings: ThreadSettings) -> anyhow::Result<String> {
        self.studio.start_thread(draft.to_string(), Some(project.to_string()), prompt(text), settings);
        let key = draft.to_string();
        self.pump_until(&format!("{draft} started"), move |view| view.started.contains_key(&key))?;
        let thread = self.view.started.get(draft).cloned().context("thread")?;
        self.studio.select_thread(Some(thread.clone()));
        Ok(thread)
    }

    fn apply(&mut self, update: Update) -> anyhow::Result<()> {
        let view = &mut self.view;
        match update {
            Update::Connection { state } => view.connection = Some(state),
            Update::Projects { projects } => view.projects = projects.into_iter().map(|project| project.id).collect(),
            Update::Threads { threads } => view.threads = threads,
            Update::ThreadStarted { draft, thread } => {
                view.started.insert(draft, thread);
            }
            Update::Transcript { thread, op } => {
                let items = view.transcripts.entry(thread).or_default();
                match op {
                    TranscriptOp::Reset { items: fresh } => *items = fresh,
                    TranscriptOp::Insert { index, item } => {
                        let index = usize::try_from(index)?.min(items.len());
                        items.insert(index, item);
                    }
                    TranscriptOp::Update { index, item } => {
                        if let Some(slot) = items.get_mut(usize::try_from(index)?) {
                            *slot = item;
                        }
                    }
                }
            }
            Update::Models { models, .. } => view.models = models,
            Update::Usage { report, .. } => view.usage = Some(report),
            Update::Providers { providers, engines, errors } => {
                view.providers = Some(providers);
                view.engines = engines;
                view.errors = errors;
            }
            Update::ProviderModels { request, base_url, models } => view.discovered.push((request, base_url, models)),
            Update::Local { request, .. } => view.local.push(request),
            Update::RequestDone { request, message } => {
                println!("     done {request}: {message}");
                view.done.push((request, message));
            }
            Update::RequestFailed { request, message } => view.failed.push((request, message)),
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::Thread { thread, info } => {
                if let Some(model) = info.model {
                    view.thread_models.insert(thread, format!("{}/{}", model.provider, model.id));
                }
            }
            Update::ArchivedThreads { .. }
            | Update::ThreadSearch { .. }
            | Update::Approvals { .. }
            | Update::Questions { .. }
            | Update::QuestionsSettled { .. }
            | Update::Queue { .. }
            | Update::ComposerText { .. }
            | Update::ComposerDraft { .. }
            | Update::LocalOnly { .. }
            | Update::SkillDraft { .. }
            | Update::SkillTry { .. }
            | Update::Commands { .. }
            | Update::Skills { .. }
            | Update::Subagents { .. }
            | Update::MessageImage { .. }
            | Update::Notification { .. }
            | Update::GitStatus { .. }
            | Update::Branches { .. }
            | Update::Diff { .. }
            | Update::Files { .. }
            | Update::FileContent { .. }
            | Update::SearchResults { .. }
            | Update::Mentions { .. }
            | Update::PullRequests { .. }
            | Update::PullRequestDetail { .. }
            | Update::Devices { .. }
            | Update::TerminalOpened { .. }
            | Update::TerminalFrame { .. }
            | Update::TerminalText { .. }
            | Update::TerminalExited { .. }
            | Update::Settings { .. }
            | Update::Keybindings { .. }
            | Update::T3Sync { .. }
            | Update::ProjectScripts { .. }
            | Update::ProjectSettings { .. }
            | Update::WorktreeSetup { .. }
            | Update::DaemonDefault { .. }
            | Update::Machines { .. }
            | Update::Pairing { .. }
            | Update::PairedDevices { .. }
            | Update::Consents { .. }
            | Update::Plugins { .. }
            | Update::Daemon { .. }
            | Update::Kernel { .. }
            | Update::AppUpdate { .. }
            | Update::LocalDashboard { .. } => {}
        }
        Ok(())
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn settings(provider: &str, model: &str) -> ThreadSettings {
    ThreadSettings {
        provider: Some(provider.to_string()),
        model: Some(model.to_string()),
        thinking_level: None,
        access: Some(AccessMode::FullAccess),
        worktree: WorktreeChoice::Local,
    }
}

fn free_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn which(program: &str) -> bool {
    Command::new("/usr/bin/which").arg(program).output().is_ok_and(|output| output.status.success())
        || std::env::var_os("HOME").is_some_and(|home| Path::new(&home).join(".bun/bin").join(program).is_file())
}

fn start_stub() -> anyhow::Result<(Child, u16)> {
    let binary = std::env::current_exe()?.with_file_name("provider_stub");
    ensure!(binary.is_file(), "build the provider_stub example first ({} is missing)", binary.display());
    let mut child = Command::new(&binary).arg(KEY).stdin(Stdio::null()).stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().context("stub stdout")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    let port = line.trim().parse().context("stub port")?;
    println!("ok   stub endpoint pid {} on 127.0.0.1:{port}", child.id());
    Ok((child, port))
}

fn stop_stub(mut child: Child) -> anyhow::Result<()> {
    let pid = child.id();
    child.kill()?;
    child.wait()?;
    println!("ok   stub endpoint pid {pid} stopped");
    Ok(())
}

fn stop_daemon(socket: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(socket).await?;
        client
            .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
            .await?;
        println!("ok   daemon stopped");
        anyhow::Ok(())
    })
}

fn mode(path: &Path) -> anyhow::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn auth_keys(agent: &Path) -> anyhow::Result<BTreeSet<String>> {
    Ok(read_json(&agent.join("auth.json"))?.as_object().map(|map| map.keys().cloned().collect()).unwrap_or_default())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: providers_smoke <eukhe> [scratch dir, default /tmp/ls-providers]";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let base = args.next().map_or_else(|| PathBuf::from("/tmp/ls-providers"), PathBuf::from);
    let root = base.join(format!("run-{}", std::process::id()));
    let real = root.join("real");
    real_model::install(&real.join("agent"))?;
    run(&daemon, &real, None)?;
    let custom = root.join("custom");
    let agent = custom.join("agent");
    std::fs::create_dir_all(&agent)?;
    for (name, content) in [("auth.json", "{}"), ("models.json", r#"{"providers":{}}"#)] {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(agent.join(name))?
            .write_all(content.as_bytes())?;
    }
    local_studio_core::check_scratch_agent(&agent).map_err(anyhow::Error::msg)?;
    let (stub, port) = start_stub()?;
    let outcome = run(&daemon, &custom, Some(port));
    let stopped_stub = stop_stub(stub);
    outcome?;
    stopped_stub?;
    println!("PASS providers smoke ({})", root.display());
    Ok(())
}

fn run(daemon: &Path, root: &Path, stub: Option<u16>) -> anyhow::Result<()> {
    let agent = root.join("agent");
    let work = root.join("work");
    std::fs::create_dir_all(&work)?;
    let models = agent.join("models.json");
    let original = Original {
        providers: if models.exists() { read_json(&models)?["providers"].clone() } else { Value::Null },
        keys: auth_keys(&agent)?,
    };
    if let Some(port) = stub {
        let controller = root.join("data/controller");
        std::fs::create_dir_all(&controller)?;
        let named = json!({
            "fleetKey": format!("providers-smoke-{}", std::process::id()),
            "enginePorts": [port],
            "registry": { "url": "", "dir": root.join("registry"), "ref": "none" },
            "modelsDir": root.join("models"),
        });
        std::fs::write(controller.join("config.json"), named.to_string())?;
    }
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    std::env::set_var("LOCAL_STUDIO_CONTROLLER_URL", format!("http://127.0.0.1:{}", free_port()?));
    let socket = root.join("d.sock");
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
        controller_main: Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../controller/src/main.ts").to_string()),
        take_over_serve: false,
    };
    let outcome = Harness::new(config).and_then(|mut harness| {
        let outcome = drive(&mut harness, &agent, &work, stub, &original);
        harness.studio.shutdown();
        outcome
    });
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped
}

fn drive(
    harness: &mut Harness,
    agent: &Path,
    work: &Path,
    stub: Option<u16>,
    original: &Original,
) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    harness.pump_until("daemon launched and connected", |view| matches!(view.connection, Some(Connection::Ready)))?;
    studio.add_project(work.to_string_lossy().into_owned());
    harness.pump_until("project added", |view| view.projects.len() == 1)?;
    let project = harness.view.projects[0].clone();
    let Some(port) = stub else {
        real_provider(harness, &project)?;
        let request = harness.request();
        harness.studio.load_usage(request, UsagePeriod::Day);
        harness.pump_until("usage report loaded", |view| view.usage.is_some())?;
        return real_model::check_usage(harness.view.usage.as_ref().context("usage")?);
    };
    discovery(harness, port)?;
    local_servers(harness, agent, port)?;
    server_windows(harness, agent)?;
    add_and_chat(harness, agent, port, &project)?;
    edit_and_keys(harness, agent, port)?;
    local_fleet(harness, port, &project)?;
    cleanup(harness, agent, original)
}

fn real_provider(harness: &mut Harness, project: &str) -> anyhow::Result<()> {
    let (provider, model) = (real_model::provider(), real_model::model());
    harness.studio.load_models();
    harness.pump_until(
        "the local model's keyless provider is listed as needing no key and its model is configured",
        |view| {
            view.provider(provider).is_some_and(|status| {
                status.kind == ProviderKind::Custom
                    && status.credential == ProviderCredential::NotRequired
                    && status.configured
            }) && view.has_model(provider, model)
        },
    )?;
    let thread = harness.start(
        "real-draft",
        project,
        "Reply with only the word pong.",
        real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local),
    )?;
    let watched = thread.clone();
    let expected = format!("{provider}/{model}");
    harness.pump_until("a thread on the local model gets a streamed reply", move |view| {
        !view.answer(&watched).trim().is_empty()
            && view.idle(&watched)
            && view.thread_models.get(&watched) == Some(&expected)
    })?;
    println!("     {provider}/{model} replied: {}", harness.view.answer(&thread).trim());
    Ok(())
}

fn discovery(harness: &mut Harness, port: u16) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    studio.load_providers();
    harness.pump_until("built-in providers listed with key state", |view| {
        view.provider("openai").is_some_and(|provider| {
            provider.kind == ProviderKind::BuiltIn
                && provider.credential == ProviderCredential::Missing
                && provider.env_var.as_deref() == Some("OPENAI_API_KEY")
        }) && view
            .providers
            .as_ref()
            .is_some_and(|providers| providers.iter().all(|provider| !provider.id.starts_with("stub-")))
    })?;

    let closed = free_port()?;
    let request = harness.request();
    studio.discover_provider_models(
        request.clone(),
        format!("http://127.0.0.1:{closed}/v1"),
        ProviderApi::OpenAiCompatible,
        None,
        None,
    );
    harness.failure("discovery against a closed port surfaces the error", &request, "Could not connect")?;

    let request = harness.request();
    studio.discover_provider_models(
        request.clone(),
        format!("http://127.0.0.1:{port}"),
        ProviderApi::OpenAiCompatible,
        Some(WRONG_KEY.to_string()),
        None,
    );
    harness.failure("discovery with a wrong key surfaces the rejection", &request, "rejected the API key")?;

    let request = harness.request();
    studio.discover_provider_models(
        request.clone(),
        format!("http://127.0.0.1:{port}/"),
        ProviderApi::OpenAiCompatible,
        Some(KEY.to_string()),
        None,
    );
    harness.answer("discovery lists the endpoint's models", &request)?;
    let (_, base_url, models) =
        harness.view.discovered.iter().find(|(id, _, _)| *id == request).cloned().context("discovered")?;
    ensure!(base_url == format!("http://127.0.0.1:{port}/v1"), "discovery did not settle on /v1: {base_url}");
    ensure!(
        models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>() == ["stub-1", "stub-2"],
        "models {models:?}"
    );
    ensure!(models.iter().all(|model| model.context_window == 32768), "context windows {models:?}");
    println!("     discovered {base_url} {:?}", models.iter().map(|model| &model.id).collect::<Vec<_>>());
    Ok(())
}

fn canned(answer: Option<String>, counted: fn(&str) -> bool) -> anyhow::Result<(u16, Arc<AtomicUsize>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hits);
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut stream in listener.incoming().flatten() {
            if let Some(answer) = &answer {
                let mut lines = BufReader::new(&stream).lines().map_while(Result::ok);
                if counted(&lines.next().unwrap_or_default()) {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                lines.take_while(|line| !line.is_empty()).for_each(drop);
                let _ = stream.write_all(answer.as_bytes());
            } else {
                count.fetch_add(1, Ordering::SeqCst);
                held.push(stream);
            }
        }
    });
    Ok((port, hits))
}

fn engine_like(routes: Vec<(&'static str, String)>) -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut first = String::new();
            let mut length = 0;
            let mut line = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0; length];
            if std::io::Read::read_exact(&mut reader, &mut body).is_err() {
                continue;
            }
            let route = first.split(' ').take(2).collect::<Vec<_>>().join(" ");
            let model =
                serde_json::from_slice::<Value>(&body).ok().and_then(|body| body["model"].as_str().map(str::to_string));
            let named = model.map(|model| format!("{route} {model}"));
            let answer = routes
                .iter()
                .find(|(known, _)| Some(*known) == named.as_deref())
                .or_else(|| routes.iter().find(|(known, _)| *known == route))
                .map(|(_, body)| body.as_str());
            let (head, body) = answer.map_or(("404 Not Found", "{}"), |body| ("200 OK", body));
            let reply = format!("HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Ok(port)
}

fn server_windows(harness: &mut Harness, agent: &Path) -> anyhow::Result<()> {
    let list = |ids: &[&str]| {
        json!({ "object": "list", "data": ids.iter().map(|id| json!({ "id": id, "object": "model" })).collect::<Vec<_>>() }).to_string()
    };
    let set = json!({
        "parameters": "num_ctx                        16384\nstop                           \"<|im_end|>\"",
        "model_info": { "lfm3.context_length": 262_144 },
    })
    .to_string();
    let card =
        json!({ "parameters": "stop \"<|im_end|>\"", "model_info": { "lfm3.context_length": 262_144 } }).to_string();
    let ollama = engine_like(vec![
        ("GET /v1/models", list(&["tiny:latest", "card:latest"])),
        ("GET /api/ps", r#"{"models":[]}"#.to_string()),
        ("POST /api/show tiny:latest", set),
        ("POST /api/show card:latest", card),
    ])?;
    let llama = engine_like(vec![
        ("GET /v1/models", list(&["tiny:latest"])),
        ("GET /props", r#"{"default_generation_settings":{"n_ctx":8192}}"#.to_string()),
    ])?;
    let studio = json!({ "data": [
        { "id": "warm", "loaded_context_length": 4096, "max_context_length": 131_072 },
        { "id": "cold", "max_context_length": 131_072 },
    ] })
    .to_string();
    let lm = engine_like(vec![("GET /v1/models", list(&["warm", "cold"])), ("GET /api/v0/models", studio)])?;
    let cases: [(u16, &[(&str, Option<u64>)]); 3] = [
        (ollama, &[("tiny:latest", Some(16_384)), ("card:latest", None)]),
        (llama, &[("tiny:latest", Some(8_192))]),
        (lm, &[("warm", Some(4_096)), ("cold", None)]),
    ];
    for (port, windows) in cases {
        let base = format!("http://127.0.0.1:{port}/v1");
        harness.studio.find_local_servers(vec![found_here(base.clone())]);
        let listed = base.clone();
        harness.pump_until("an engine whose model list has no context size is found", move |view| {
            view.engines.iter().any(|engine| engine.id == listed)
        })?;
        let request = harness.request();
        harness.studio.use_local_engine(request.clone(), base.clone());
        harness.answer("that engine is used", &request)?;
        let providers = read_json(&agent.join("models.json"))?["providers"].clone();
        let (id, entry) = providers
            .as_object()
            .and_then(|providers| providers.iter().find(|(_, entry)| entry["baseUrl"] == base.as_str()))
            .context("the used engine is in models.json")?;
        for (model, window) in windows {
            let written = entry["models"]
                .as_array()
                .and_then(|models| models.iter().find(|written| written["id"] == *model))
                .with_context(|| format!("{base}: {model} is in models.json: {entry}"))?;
            ensure!(written["contextWindow"].as_u64() == *window, "{base}: {model}: {written}");
            match window {
                Some(window) => {
                    println!("ok   {base} {model} is added with the context size the engine runs ({window})")
                }
                None => println!("ok   {base} {model} is added with no context size, not its card's maximum"),
            }
        }
        let request = harness.request();
        harness.studio.remove_custom_provider(request.clone(), id.clone());
        harness.answer("that engine is removed", &request)?;
    }
    harness.studio.find_local_servers(Vec::new());
    harness.pump_until("those engines leave the list", |view| view.engines.is_empty())
}

fn found_here(base_url: String) -> EndpointPreset {
    let api = ProviderApi::OpenAiCompatible;
    EndpointPreset { name: "Found Here".to_string(), base_url, api, needs_key: false }
}

fn gone_server(harness: &mut Harness) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let once = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        BufReader::new(&stream).lines().map_while(Result::ok).take_while(|line| !line.is_empty()).for_each(drop);
        let body = r#"{"data":[{"id":"brief"}]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    });
    harness.studio.find_local_servers(vec![found_here(base.clone())]);
    let listed = base.clone();
    harness.pump_until("a server that answers once is listed", move |view| {
        view.engines.iter().any(|engine| engine.id == listed)
    })?;
    once.join().map_err(|_| anyhow::anyhow!("the brief server panicked"))??;
    let request = harness.request();
    harness.studio.use_local_engine(request.clone(), base.clone());
    let expected = format!("Found Here no longer answers at {base}.");
    harness.failure("Use on a server that stopped saves nothing", &request, &expected)
}

fn local_servers(harness: &mut Harness, agent: &Path, port: u16) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    let reply = |head: &str, body: &str| {
        Some(format!("HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()))
    };
    let list = r#"{"data":[{"id":"other"}]}"#;
    let big = format!(r#"{{"data":[{{"id":"{}"}}]}}"#, "x".repeat(2 << 20));
    let (elsewhere, elsewhere_hits) = canned(reply("200 OK", list), |line| line.contains("/studio-only/"))?;
    let (proxy, proxy_hits) =
        canned(reply("200 OK", list), |line| line.split(' ').nth(1).is_some_and(|target| target.starts_with("http")))?;
    let bad = [
        None,
        reply("401 Unauthorized", list),
        reply("500 Internal Server Error", list),
        reply("200 OK", "<html></html>"),
        reply("200 OK", r#"{"data":[]}"#),
        reply("200 OK", r#"{"data":[{"id":"text-embedding-nomic-embed-text-v1.5"}]}"#),
        reply("200 OK", &big),
        reply(&format!("302 Found\r\nLocation: http://127.0.0.1:{elsewhere}/studio-only/v1/models"), ""),
    ];
    let mut urls = vec![
        format!("http://[::ffff:127.0.0.1]:{elsewhere}/studio-only/v1"),
        format!("http://127.0.0.1:{}/v1", free_port()?),
    ];
    for answer in bad {
        urls.push(format!("http://127.0.0.1:{}/v1", canned(answer, |_| true)?.0));
    }
    let base = format!("http://127.0.0.1:{port}/v1");
    urls.push(format!("{base}/"));
    let presets = urls.into_iter().map(found_here);
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"));
    let started = Instant::now();
    studio.find_local_servers(presets.collect());
    harness.pump_until("a model server on this computer is found", |view| !view.engines.is_empty())?;
    std::env::remove_var("HTTP_PROXY");
    let took = started.elapsed();
    ensure!(took < Duration::from_millis(2500), "a silent port held the search for {took:?}");
    let [engine] = harness.view.engines.as_slice() else { bail!("engines {:?}", harness.view.engines) };
    let engine = engine.clone();
    let named = engine.controller == "Found Here" && engine.port == port && engine.base_url == base;
    ensure!(named && engine.models == ["stub-1", "stub-2"] && !engine.added, "engine {engine:?}");
    ensure!(elsewhere_hits.load(Ordering::SeqCst) == 0, "a redirect or an address that is not loopback was asked");
    ensure!(proxy_hits.load(Ordering::SeqCst) == 0, "the search went through HTTP_PROXY");
    println!("ok   silent, refused, broken, oversized, redirecting and proxied answers are left out ({took:?})");
    for step in ["used", "used again"] {
        let request = harness.request();
        studio.use_local_engine(request.clone(), engine.id.clone());
        harness.answer(&format!("the found server is {step}"), &request)?;
        harness.pump_until("its models reach the picker and its row says added", |view| {
            view.has_model("found-here", "stub-2") && view.engines.iter().all(|engine| engine.added)
        })?;
    }
    let file = agent.join("models.json");
    let mut models = read_json(&file)?;
    let providers = models["providers"].clone();
    ensure!(providers.as_object().is_some_and(|providers| providers.len() == 1), "providers {providers}");
    let entry = &providers["found-here"];
    ensure!(entry["apiKey"] == "none" && entry["models"][0]["contextWindow"] == 32768, "entry {entry}");
    let written = models["providers"]["found-here"]["models"].clone();
    models["providers"]["found-here"]["models"] = Value::Array(Vec::new());
    std::fs::write(&file, models.to_string())?;
    studio.load_providers();
    harness
        .pump_until("an entry with no models does not count", |view| view.engines.iter().all(|engine| !engine.added))?;
    models["providers"]["found-here"]["models"] = written;
    models["providers"]["found-here"]["baseUrl"] = format!("http://localhost:{port}/v1/").into();
    std::fs::write(&file, models.to_string())?;
    studio.load_providers();
    harness.pump_until("localhost and a trailing slash are the same server", |view| {
        view.engines.iter().all(|engine| engine.added)
    })?;
    let request = harness.request();
    studio.remove_custom_provider(request.clone(), "found-here".to_string());
    harness.answer("the found server is removed", &request)?;
    gone_server(harness)?;
    studio.find_local_servers(Vec::new());
    harness.pump_until("a server that is gone leaves the list", |view| view.engines.is_empty())
}

fn add_and_chat(harness: &mut Harness, agent: &Path, port: u16, project: &str) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    let request = harness.request();
    studio.add_custom_provider(
        request.clone(),
        CustomProvider {
            name: "Stub Endpoint".to_string(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api: ProviderApi::OpenAiCompatible,
            key: ProviderKey::Set { key: KEY.to_string() },
            models: Some(vec![ProviderModel {
                id: "stub-1".to_string(),
                name: "stub-1".to_string(),
                context_window: 32768,
            }]),
        },
    );
    harness.answer("custom endpoint added with one chosen model", &request)?;
    harness.pump_until("the endpoint's chosen model reaches the model picker", |view| {
        view.has_model("stub-endpoint", "stub-1")
            && !view.has_model("stub-endpoint", "stub-2")
            && view.provider("stub-endpoint").is_some_and(|provider| {
                provider.kind == ProviderKind::Custom
                    && provider.credential == ProviderCredential::StoredKey
                    && provider.model_count == 1
                    && provider.base_url.as_deref() == Some(&format!("http://127.0.0.1:{port}/v1"))
            })
    })?;
    check_files(agent, "stub-endpoint", 1)?;

    let thread = harness.start("stub-draft", project, "ping", settings("stub-endpoint", "stub-1"))?;
    let watched = thread.clone();
    harness
        .pump_until("a new thread on the endpoint gets the stub's streamed reply with the saved key", move |view| {
            view.answer(&watched).contains("Stub reply from stub-1 (key ok).") && view.idle(&watched)
        })?;
    println!("     transcript: {}", harness.view.answer(&thread));

    let request = harness.request();
    studio.refresh_provider_models(request.clone(), "stub-endpoint".to_string());
    harness.answer("refresh lists the endpoint's other model", &request)?;
    harness.pump_until("the refreshed model reaches the picker", |view| {
        view.has_model("stub-endpoint", "stub-2")
            && view.provider("stub-endpoint").is_some_and(|provider| provider.model_count == 2)
    })?;

    studio.set_model(thread.clone(), "stub-endpoint".to_string(), "stub-2".to_string());
    studio.send(harness.request(), thread.clone(), prompt("ping again"), Delivery::Default);
    let watched = thread.clone();
    harness.pump_until("a thread opened before the model existed switches to it and gets the reply", move |view| {
        view.answer(&watched).contains("Stub reply from stub-2 (key ok).")
            && view.thread_models.get(&watched).map(String::as_str) == Some("stub-endpoint/stub-2")
    })?;

    let request = harness.request();
    studio.add_custom_provider(
        request.clone(),
        CustomProvider {
            name: "Stub Two".to_string(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: ProviderApi::OpenAiCompatible,
            key: ProviderKey::Set { key: KEY.to_string() },
            models: None,
        },
    );
    harness.answer("a second endpoint added by discovery alone", &request)?;
    harness.pump_until("every discovered model of the second endpoint reaches the picker", |view| {
        view.has_model("stub-two", "stub-1")
            && view.has_model("stub-two", "stub-2")
            && view.provider("stub-two").is_some_and(|provider| {
                provider.base_url.as_deref() == Some(&format!("http://127.0.0.1:{port}/v1"))
                    && provider.models.iter().all(|model| model.context_window == 32768)
            })
    })?;
    Ok(())
}

fn edit_and_keys(harness: &mut Harness, agent: &Path, port: u16) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    let request = harness.request();
    studio.update_custom_provider(
        request.clone(),
        "stub-endpoint".to_string(),
        CustomProvider {
            name: "Stub Renamed".to_string(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api: ProviderApi::OpenAiCompatible,
            key: ProviderKey::Keep,
            models: Some(vec![ProviderModel {
                id: "stub-1".to_string(),
                name: "Stub One".to_string(),
                context_window: 32768,
            }]),
        },
    );
    harness.answer("endpoint renamed and narrowed to one model", &request)?;
    harness.pump_until("rename and model choice reach the picker", |view| {
        view.provider("stub-endpoint")
            .is_some_and(|provider| provider.name == "Stub Renamed" && provider.models.len() == 1)
            && view.has_model("stub-endpoint", "stub-1")
            && !view.has_model("stub-endpoint", "stub-2")
            && view.models.iter().any(|model| model.provider == "stub-endpoint" && model.name == "Stub One")
    })?;
    ensure!(
        read_json(&agent.join("auth.json"))?["stub-endpoint"]["key"].as_str() == Some(KEY),
        "Keep dropped the stored key"
    );

    let request = harness.request();
    studio.refresh_provider_models(request.clone(), "stub-endpoint".to_string());
    harness.answer("refresh re-lists the endpoint's models", &request)?;
    harness.pump_until("refreshed models reach the picker", |view| {
        view.has_model("stub-endpoint", "stub-2")
            && view.provider("stub-endpoint").is_some_and(|provider| provider.models.len() == 2)
    })?;

    let request = harness.request();
    studio.set_provider_key(request.clone(), "openai".to_string(), Some(OPENAI_KEY.to_string()));
    harness.answer("built-in key saved", &request)?;
    harness.pump_until("built-in shows a stored key and its models become available", |view| {
        view.provider("openai")
            .is_some_and(|provider| provider.credential == ProviderCredential::StoredKey && provider.configured)
            && view.models.iter().any(|model| model.provider == "openai" && model.configured)
    })?;
    ensure!(mode(&agent.join("auth.json"))? == 0o600, "auth.json is not 0600");
    let request = harness.request();
    studio.set_provider_key(request.clone(), "openai".to_string(), None);
    harness.answer("built-in key removed", &request)?;
    harness.pump_until("built-in back to missing", |view| {
        view.provider("openai").is_some_and(|provider| provider.credential == ProviderCredential::Missing)
    })?;
    ensure!(read_json(&agent.join("auth.json"))?.get("openai").is_none(), "openai key left in auth.json");
    Ok(())
}

fn cleanup(harness: &mut Harness, agent: &Path, original: &Original) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    for id in ["stub-endpoint", "stub-two"] {
        let request = harness.request();
        studio.remove_custom_provider(request.clone(), id.to_string());
        harness.answer(&format!("{id} removed"), &request)?;
        harness.pump_until("the removed endpoint leaves the picker and the pane", move |view| {
            !view.models.iter().any(|model| model.provider == id) && view.provider(id).is_none()
        })?;
    }
    let models = read_json(&agent.join("models.json"))?;
    ensure!(
        models["providers"] == original.providers,
        "models.json providers changed: {} (was {})",
        models["providers"],
        original.providers
    );
    let keys = auth_keys(agent)?;
    ensure!(keys == original.keys, "auth.json providers changed: {keys:?} (was {:?})", original.keys);
    ensure!(harness.view.errors.is_empty(), "models.json errors: {:?}", harness.view.errors);
    println!("ok   models.json and auth.json back to the providers they started with");
    Ok(())
}

fn check_files(agent: &Path, id: &str, count: usize) -> anyhow::Result<()> {
    let models_path = agent.join("models.json");
    let auth_path = agent.join("auth.json");
    ensure!(mode(&models_path)? == 0o600, "models.json is not 0600");
    ensure!(mode(&auth_path)? == 0o600, "auth.json is not 0600");
    ensure!(!std::fs::read_to_string(&models_path)?.contains(KEY), "the key landed in models.json");
    let models = read_json(&models_path)?;
    let entry = &models["providers"][id];
    ensure!(entry["api"] == "openai-completions" && entry["apiKey"] == "none", "entry {entry}");
    ensure!(entry["models"].as_array().is_some_and(|list| list.len() == count), "entry models {entry}");
    let auth = read_json(&auth_path)?;
    ensure!(
        auth[id]["type"] == "api_key" && auth[id]["key"].as_str() == Some(KEY),
        "auth.json entry for {id} is wrong"
    );
    ensure!(!agent.join("auth.json.lock").exists(), "auth.json.lock left behind");
    println!("ok   models.json and auth.json are 0600, the key lives only in auth.json");
    Ok(())
}

fn local_fleet(harness: &mut Harness, port: u16, project: &str) -> anyhow::Result<()> {
    if !which("bun") {
        println!("skip Local fleet engines: bun is not installed");
        return Ok(());
    }
    let studio = Arc::clone(&harness.studio);
    let request = harness.request();
    studio.local_snapshot(request.clone());
    harness.answer("Local controller started", &request)?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    while !harness.view.engines.iter().any(|engine| engine.port == port) {
        ensure!(Instant::now() < deadline, "the controller never listed the stub engine");
        studio.load_providers();
        harness.settle(Duration::from_secs(2))?;
    }
    println!("ok   the controller lists the stub as a fleet engine");
    let engine = harness.view.engines.iter().find(|engine| engine.port == port).cloned().context("engine")?;
    println!("     engine {} via {} models {:?}", engine.id, engine.base_url, engine.models);
    ensure!(!engine.added, "engine already added");
    studio.find_local_servers(vec![found_here(format!("http://127.0.0.1:{port}/v1"))]);
    harness.pump_until("the controller's row stands for the found server and its endpoint", move |view| {
        let mut rows = view.engines.iter().filter(|engine| engine.port == port);
        rows.next().is_some_and(|row| !row.provider.is_empty() && row.added) && rows.next().is_none()
    })?;
    studio.find_local_servers(Vec::new());
    harness.pump_until("the controller's own provider is not added yet", move |view| {
        view.engines.iter().all(|engine| engine.port != port || !engine.added)
    })?;
    let request = harness.request();
    studio.use_local_engine(request.clone(), engine.id.clone());
    harness.answer("Use this engine", &request)?;
    let provider = engine.provider.clone();
    let shown = provider.clone();
    harness.pump_until("engine provider reaches the picker", move |view| {
        view.has_model(&shown, "stub-1") && view.engines.iter().any(|engine| engine.provider == shown && engine.added)
    })?;
    let thread = harness.start("engine-draft", project, "ping", settings(&provider, "stub-1"))?;
    harness.pump_until("a thread on the fleet engine gets the stub's reply through the controller", move |view| {
        view.answer(&thread).contains("Stub reply from stub-1")
    })?;
    let request = harness.request();
    studio.remove_custom_provider(request.clone(), provider);
    harness.answer("engine provider removed", &request)?;
    Ok(())
}
