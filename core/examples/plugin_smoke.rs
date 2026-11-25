use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::plugins::{self as plugin_store, Capability, Catalog, Consent, Grants, Source};
use local_studio_core::{
    AccessMode, CommandKind, Connection, DaemonLaunch, Delivery, Item, ItemKind, ModelKey, Project, PromptDraft,
    SkillEntry, SlashCommand, Studio, StudioConfig, ThreadRow, ThreadSettings, ThreadStatus, ToolState, TranscriptOp,
    Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};

const PROVIDER: &str = "local-fleet";
const MID: &str = "hf.co/LiquidAI/LFM2.5-8B-A1B-GGUF:Q4_K_M";
const SMALL: &str = "hf.co/LiquidAI/LFM3-1.7B-GGUF:Q4_K_M";
const MAX_TOKENS: u64 = 16_384;
const STEP: Duration = Duration::from_secs(120);
const BUILD: Duration = Duration::from_mins(25);
const REQUEST: &str = "a fan guard for a 40 mm fan. Run the stone commands with the bash tool in the current directory and stop when stone says the part is ready.";

struct Target {
    id: String,
    command: String,
    model: String,
    request: String,
    expanded: String,
    skill: String,
    mark: String,
    check: Option<String>,
    step: Duration,
    build: Duration,
}

impl Target {
    fn from_env() -> anyhow::Result<Self> {
        let set = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let var = |name: &str, default: &str| set(name).unwrap_or_else(|| default.to_string());
        let secs = |name: &str, default: Duration| -> anyhow::Result<Duration> {
            set(name).map_or(Ok(default), |value| {
                value.trim().parse().map(Duration::from_secs).with_context(|| format!("{name} must be whole seconds"))
            })
        };
        Ok(Self {
            id: var("PLUGIN_SMOKE_ID", "smoothstone"),
            command: var("PLUGIN_SMOKE_COMMAND", "stone"),
            model: var("PLUGIN_SMOKE_MODEL", MID),
            request: var("PLUGIN_SMOKE_REQUEST", REQUEST),
            expanded: var("PLUGIN_SMOKE_EXPANDED", "stone status"),
            skill: var("PLUGIN_SMOKE_SKILL", "smoothstone"),
            mark: var("PLUGIN_SMOKE_MARK", "NEXT:"),
            check: set("PLUGIN_SMOKE_CHECK"),
            step: secs("PLUGIN_SMOKE_STEP_SECS", STEP)?,
            build: secs("PLUGIN_SMOKE_BUILD_SECS", BUILD)?,
        })
    }

    fn source(&self) -> String {
        format!("plugin:{}", self.id)
    }
}

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
    projects: Vec<Project>,
    threads: Vec<ThreadRow>,
    started: Vec<(String, String)>,
    transcripts: Vec<(String, Vec<Item>)>,
    notices: Vec<String>,
    commands: Option<(String, Vec<SlashCommand>)>,
    skills: Option<Vec<SkillEntry>>,
    refusals: Vec<(String, String, Option<ModelKey>)>,
    usage: Option<UsageReport>,
    composer: Vec<(String, String)>,
    other: Vec<String>,
}

impl View {
    fn thread(&self, draft: &str) -> Option<String> {
        self.started.iter().find(|(key, _)| key == draft).map(|(_, thread)| thread.clone())
    }

    fn items(&self, thread: &str) -> &[Item] {
        self.transcripts.iter().find(|(key, _)| key == thread).map_or(&[], |(_, items)| items.as_slice())
    }

    fn idle(&self, thread: &str) -> bool {
        self.threads
            .iter()
            .find(|row| row.id == thread)
            .is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }

    fn commands(&self, request: &str) -> Option<&[SlashCommand]> {
        self.commands.as_ref().filter(|(key, _)| key == request).map(|(_, commands)| commands.as_slice())
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
    step: Duration,
}

impl Harness {
    fn pump_until(&mut self, step: &str, limit: Duration, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + limit;
        loop {
            for update in self.studio.tick() {
                self.apply(update)?;
            }
            if done(&self.view) {
                println!("ok   {step}");
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("timed out waiting for: {step}");
            };
            self.signal.wait(left.min(Duration::from_millis(500)));
        }
    }

    fn apply(&mut self, update: Update) -> anyhow::Result<()> {
        match update {
            Update::Connection { state } => self.view.connection = Some(state),
            Update::Projects { projects } => self.view.projects = projects,
            Update::Threads { threads } => self.view.threads = threads,
            Update::ThreadStarted { draft, thread } => self.view.started.push((draft, thread)),
            Update::Notice { level, text } => {
                println!("     notice {level:?}: {text}");
                self.view.notices.push(text);
            }
            Update::RequestFailed { request, message } => {
                println!("     request {request} failed: {message}");
                self.view.other.push(format!("RequestFailed {request}: {message}"));
            }
            Update::Commands { request, commands } => self.view.commands = Some((request, commands)),
            Update::Skills { skills, .. } => self.view.skills = Some(skills),
            Update::Transcript { thread, op } => self.transcript(&thread, op)?,
            Update::LocalOnly { thread, plugin, message, local } => {
                println!("     local-only refusal for {plugin}: {message}");
                self.view.refusals.push((thread, message, local));
            }
            Update::Usage { report, .. } => self.view.usage = Some(report),
            Update::ComposerText { thread, text } => {
                self.view.composer.retain(|(key, _)| *key != thread);
                self.view.composer.push((thread, text));
            }
            other => self.view.other.push(format!("{other:?}")),
        }
        Ok(())
    }

    fn transcript(&mut self, thread: &str, op: TranscriptOp) -> anyhow::Result<()> {
        if !self.view.transcripts.iter().any(|(key, _)| key == thread) {
            self.view.transcripts.push((thread.to_string(), Vec::new()));
        }
        let items = &mut self.view.transcripts.iter_mut().find(|(key, _)| key == thread).context("transcript")?.1;
        match op {
            TranscriptOp::Reset { items: fresh } => *items = fresh,
            TranscriptOp::Insert { index, item } => {
                let index = usize::try_from(index)?;
                ensure!(index <= items.len(), "insert at {index} past {} items", items.len());
                items.insert(index, item);
            }
            TranscriptOp::Update { index, item } => {
                let index = usize::try_from(index)?;
                *items.get_mut(index).context("update past the end")? = item;
            }
        }
        Ok(())
    }

    fn list_commands(&mut self, request: &str) -> anyhow::Result<Vec<SlashCommand>> {
        self.studio.list_commands(request.to_string(), None);
        self.pump_until(&format!("command list {request}"), self.step, |view| view.commands(request).is_some())?;
        Ok(self.view.commands(request).unwrap_or_default().to_vec())
    }
}

fn settings(model: &str) -> ThreadSettings {
    ThreadSettings {
        provider: Some(PROVIDER.to_string()),
        model: Some(model.to_string()),
        thinking_level: None,
        access: Some(AccessMode::FullAccess),
        worktree: WorktreeChoice::Local,
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn user_text(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn plugin_calls(items: &[Item], mark: &str) -> usize {
    items
        .iter()
        .filter(|item| {
            matches!(&item.kind, ItemKind::Tool { state: ToolState::Succeeded | ToolState::Failed, output, .. } if output.contains(mark))
        })
        .count()
}

fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(data)?;
    Ok(())
}

fn agent_dir(root: &Path, models: &Path, model: &str) -> anyhow::Result<PathBuf> {
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent)?;
    let mut catalog: Value = serde_json::from_slice(&std::fs::read(models)?)?;
    let provider = catalog
        .get_mut("providers")
        .and_then(|providers| providers.get_mut(PROVIDER))
        .with_context(|| format!("{} has no {PROVIDER} provider", models.display()))?
        .clone();
    let mut provider = provider;
    for model in provider.get_mut("models").and_then(Value::as_array_mut).context("models")? {
        model["maxTokens"] = json!(MAX_TOKENS);
    }
    let catalog = json!({ "providers": { PROVIDER: provider } });
    write_private(&agent.join("models.json"), &serde_json::to_vec_pretty(&catalog)?)?;
    write_private(
        &agent.join("settings.json"),
        &serde_json::to_vec_pretty(&json!({ "defaultProvider": PROVIDER, "defaultModel": model }))?,
    )?;
    write_private(&agent.join("auth.json"), b"{}")?;
    println!("     scratch agent dir {} with only {PROVIDER} (auth.json is {{}})", agent.display());
    Ok(agent)
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: [PLUGIN_SMOKE_ID=smoothstone PLUGIN_SMOKE_COMMAND=stone PLUGIN_SMOKE_MODEL=<model> PLUGIN_SMOKE_REQUEST=<text after /command> PLUGIN_SMOKE_EXPANDED='stone status' PLUGIN_SMOKE_SKILL=smoothstone PLUGIN_SMOKE_MARK='NEXT:' PLUGIN_SMOKE_CHECK=<sh command run in the work dir> PLUGIN_SMOKE_STEP_SECS=120 PLUGIN_SMOKE_BUILD_SECS=1500] plugin_smoke <eukhe> <scratch dir> <models.json> <plugin dir | catalog id> | plugin_smoke --stop <socket>";
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.first().is_some_and(|arg| arg.as_os_str() == "--stop") {
        return stop_daemon(args.get(1).context(usage)?);
    }
    if args.first().is_some_and(|arg| arg.as_os_str() == "--local-only") {
        let [_, daemon, root] = args.as_slice() else { bail!(LOCAL_USAGE) };
        return local_only::run(daemon, root);
    }
    if args.first().is_some_and(|arg| arg.as_os_str() == "--mentions") {
        let [_, daemon, root] = args.as_slice() else { bail!("usage: plugin_smoke --mentions <eukhe> <scratch dir>") };
        return local_only::mentions(daemon, root);
    }
    if args.first().is_some_and(|arg| arg.as_os_str() == "--hub") {
        let [_, root] = args.as_slice() else { bail!(hub::USAGE) };
        return hub::run(root);
    }
    let [daemon, root, models, plugin] = args.as_slice() else { bail!(usage) };
    let target = Target::from_env()?;
    ensure!(
        target.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "PLUGIN_SMOKE_ID must be a plain plugin id, not a path: {:?}",
        target.id
    );
    let agent = agent_dir(root, models, &target.model)?;
    let data = root.join("data");
    let plugins = data.join("plugins");
    if plugin.is_dir() {
        copy_dir(plugin, &plugins.join(&target.id))?;
    } else {
        install_from_catalog(&plugins, &plugin.to_string_lossy(), &target)?;
    }
    std::fs::create_dir_all(plugins.join("broken"))?;
    std::fs::write(plugins.join("broken").join("plugin.toml"), "id = \"broken\"\nversion = 1\n")?;
    let work = root.join("work");
    std::fs::create_dir_all(&work)?;
    Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    let socket = root.join("d.sock");
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        controller_main: None,
        take_over_serve: false,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
    };
    let signal = Arc::new(Signal::default());
    let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
    let mut harness = Harness { studio, signal, view: View::default(), step: target.step };
    let outcome = drive(&mut harness, &work, &plugins, &target).and_then(|()| no_plugin_servers(&agent));
    harness.studio.shutdown();
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped?;
    println!("PASS plugin smoke");
    Ok(())
}

fn install_from_catalog(plugins: &Path, id: &str, target: &Target) -> anyhow::Result<()> {
    ensure!(id == target.id, "catalog id {id} is not PLUGIN_SMOKE_ID {}", target.id);
    let catalog = Catalog::bundled().map_err(anyhow::Error::msg)?;
    let entry = catalog.entry(id).with_context(|| format!("{id} is not in the bundled catalog"))?;
    let installed = plugin_store::install(plugins, &Source::Catalog(id.to_string()), &catalog)
        .map_err(|errors| anyhow::anyhow!("catalog install refused: {}", errors.join("; ")))?;
    ensure!(installed.tree == entry.tree, "installed tree {} is not the pinned {}", installed.tree, entry.tree);
    ensure!(installed.plugin.dir == plugins.join(id), "installed into {}", installed.plugin.dir.display());
    ensure!(installed.consent == Consent::Ready, "{id} needs consent: {:?}", installed.consent);
    println!(
        "ok   check 1: {id} {} installed through the catalog at {} ({} files, {})",
        installed.plugin.version,
        &entry.rev[..12],
        installed.files,
        installed.tree
    );
    Ok(())
}

fn no_plugin_servers(agent: &Path) -> anyhow::Result<()> {
    let settings: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
    let servers = settings.get("mcpServers").and_then(Value::as_object);
    ensure!(
        servers.is_none_or(|servers| !servers.keys().any(|name| name.starts_with(plugin_store::MCP_PREFIX))),
        "a plugin without the mcp-server capability got an mcpServers entry: {servers:?}"
    );
    println!("ok   no plugin MCP server registered for a plugin that asks for none");
    Ok(())
}

fn drive(harness: &mut Harness, work: &Path, plugins: &Path, target: &Target) -> anyhow::Result<()> {
    let studio = Arc::clone(&harness.studio);
    let source = target.source();
    let name = &target.command;
    let step = target.step;
    let gone_prompt = format!("/{name} Reply with only the word gone.");
    harness.pump_until("daemon connected", step, |view| matches!(view.connection, Some(Connection::Ready)))?;
    studio.add_project(work.to_string_lossy().into_owned());
    harness.pump_until("project added", step, |view| view.projects.len() == 1)?;
    let project = harness.view.projects[0].id.clone();

    let commands = harness.list_commands("before")?;
    let listed = commands
        .iter()
        .find(|command| command.name == *name)
        .with_context(|| format!("/{name} is not in the palette"))?;
    ensure!(listed.source == source && listed.kind == CommandKind::Plugin, "/{name} has the wrong source: {listed:?}");
    println!("ok   check 2 core: /{name} listed, source {}, kind {:?}", listed.source, listed.kind);
    let skill = format!("skill:{}", target.skill);
    ensure!(
        commands.iter().any(|command| command.name == skill && command.source == source),
        "the plugin skill {skill} is not in the palette"
    );
    ensure!(commands.iter().any(|command| command.name == "compact"), "session commands are missing");
    ensure!(
        harness.view.notices.iter().any(|notice| notice.contains("broken")),
        "the broken plugin raised no notice: {:?}",
        harness.view.notices
    );
    println!("ok   check 5: broken plugin reported in a notice, {} and session commands still listed", target.id);

    studio.start_thread(
        "stone".to_string(),
        Some(project.clone()),
        prompt(&format!("/{name} {}", target.request)),
        settings(&target.model),
    );
    harness.pump_until("plugin thread started", step, |view| view.thread("stone").is_some())?;
    let thread = harness.view.thread("stone").context("no plugin thread")?;
    studio.select_thread(Some(thread.clone()));
    harness.pump_until(&format!("the /{name} prompt reached the model expanded"), step, |view| {
        user_text(view.items(&thread))
            .iter()
            .any(|text| text.contains(target.expanded.as_str()) && text.contains(target.request.as_str()))
    })?;
    let started = Instant::now();
    harness.pump_until(&format!("the /{name} turn finished on {}", target.model), target.build, |view| {
        view.idle(&thread)
    })?;
    studio.select_thread(Some(thread.clone()));
    harness.pump_until("plugin transcript reloaded", step, |view| !view.items(&thread).is_empty())?;
    let calls = plugin_calls(harness.view.items(&thread), &target.mark);
    println!("     {calls} plugin tool calls in {:?}", started.elapsed());
    for item in harness.view.items(&thread).iter().rev().take(2) {
        println!("     last item {:.400}", format!("{:?}", item.kind));
    }
    ensure!(calls > 0, "the /{name} turn ended without a plugin tool call");
    match &target.check {
        Some(check) => run_check(work, check)?,
        None => check_part(work)?,
    }
    std::fs::remove_dir_all(plugins.join(&target.id))?;
    let commands = harness.list_commands("after")?;
    ensure!(!commands.iter().any(|command| command.source == source), "the removed plugin is still listed");
    ensure!(commands.iter().any(|command| command.name == "compact"), "session commands vanished with the plugin");
    studio.start_thread("gone".to_string(), Some(project), prompt(&gone_prompt), settings(SMALL));
    harness.pump_until("gone thread started", step, |view| view.thread("gone").is_some())?;
    let gone = harness.view.thread("gone").context("no gone thread")?;
    studio.select_thread(Some(gone.clone()));
    harness.pump_until("the next turn after removal finished", step * 3, |view| {
        view.idle(&gone) && !user_text(view.items(&gone)).is_empty()
    })?;
    let sent = user_text(harness.view.items(&gone));
    ensure!(sent.iter().all(|text| text.trim() == gone_prompt), "/{name} still expanded after removal: {sent:?}");
    println!("ok   check 4: plugin folder removed, /{name} gone from the palette and sent as plain text on the next turn, no restart");
    Ok(())
}

fn run_check(work: &Path, check: &str) -> anyhow::Result<()> {
    let output = Command::new("sh").arg("-c").arg(check).current_dir(work).output().context("run the plugin check")?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    ensure!(output.status.success(), "the plugin check failed: {}", output.status);
    println!("ok   check 3: the plugin check passed: {check}");
    Ok(())
}

fn check_part(work: &Path) -> anyhow::Result<()> {
    let job = work.join("stone-job");
    let status: Value = serde_json::from_slice(&std::fs::read(job.join("status.json")).context("no stone job")?)?;
    println!("     stone status: {}", status.get("last_result").unwrap_or(&Value::Null));
    let stl = job.join("fan_guard.stl");
    ensure!(stl.is_file(), "no fan_guard.stl in {}", job.display());
    let output = Command::new("stone").arg("check").arg(&stl).output().context("run stone check")?;
    let text = String::from_utf8_lossy(&output.stdout);
    print!("{text}");
    ensure!(output.status.success() && !text.contains("FAIL"), "stone check rejected {}", stl.display());
    println!("ok   check 3: {} passes stone check", stl.display());
    Ok(())
}

fn stop_daemon(socket: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(socket).await?;
        client
            .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
            .await?;
        println!("ok   scratch daemon stopped");
        anyhow::Ok(())
    })
}

const LOCAL_USAGE: &str = "usage: plugin_smoke --local-only <eukhe> <scratch dir> (needs a local model: LOCAL_STUDIO_SMOKE_MODEL=<provider>/<model> at LOCAL_STUDIO_SMOKE_BASE_URL, default the LFM2.5 8B model on 127.0.0.1:11434)";

#[path = "support/held.rs"]
mod held;

mod local_only {
    use super::held::held;
    use super::{
        ensure, json, plugin_store, prompt, stop_daemon, user_text, write_private, Arc, AtomicUsize, Capability,
        Command, Connection, Context, DaemonClient, DaemonCommand, DaemonLaunch, Delivery, Duration, Grants, Harness,
        Instant, Map, OpenOptionsExt, Ordering, Path, PathBuf, Read, Signal, Studio, StudioConfig, TcpListener,
        ThreadSettings, ThreadStatus, UsagePeriod, UsageReport, Value, View, Waker, Write, MAX_TOKENS, MID, STEP,
    };
    use local_studio_core::{ConsentAnswer, ImageAttachment, PromptDraft};
    use std::sync::OnceLock;

    const LOCAL: &str = "ollama-local";
    const DRIFT: &str = "drift-stub";
    const LEDGER_ENV: &str = "LOCAL_STUDIO_PLUGIN_LEDGER__OUT";
    const CASES: [(&str, &str, bool); 12] = [
        ("case-v6-loopback", "http://[::1]:11434/v1", true),
        ("case-v6-mapped", "http://[::ffff:127.0.0.1]:11434/v1", true),
        ("case-v6-ula", "http://[fd00::1]:11434/v1", true),
        ("case-172-15", "http://172.15.0.1:11434/v1", false),
        ("case-172-16", "http://172.16.0.1:11434/v1", true),
        ("case-100-63", "http://100.63.0.1:11434/v1", false),
        ("case-100-64", "http://100.64.0.1:11434/v1", true),
        ("case-100-128", "http://100.128.0.1:11434/v1", false),
        ("case-localhost-dot", "http://localhost.:11434/v1", false),
        ("openai", "http://127.0.0.1:11434/v1", false),
        ("case-ftp", "ftp://127.0.0.1:11434/v1", false),
        ("case-no-base-url", "", false),
    ];
    const CASE_MODEL: &str = "case-1";
    static LOCAL_MODEL: OnceLock<(String, String)> = OnceLock::new();

    fn mid() -> &'static str {
        LOCAL_MODEL.get().map_or(MID, |(model, _)| model.as_str())
    }

    fn base() -> &'static str {
        LOCAL_MODEL.get().map_or("http://127.0.0.1:11434/v1", |(_, base)| base.as_str())
    }

    fn pick_model() -> anyhow::Result<()> {
        let model = match std::env::var("LOCAL_STUDIO_SMOKE_MODEL").ok().filter(|value| !value.is_empty()) {
            Some(value) => value
                .split_once('/')
                .map(|(_, model)| model.to_string())
                .context("LOCAL_STUDIO_SMOKE_MODEL is not <provider>/<model>")?,
            None => MID.to_string(),
        };
        let base = std::env::var("LOCAL_STUDIO_SMOKE_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
        let _ = LOCAL_MODEL.set((model, base));
        Ok(())
    }
    const CLOUD: &str = "cloud-stub";
    const CLOUD_MODEL: &str = "stub-1";
    const CLOUD_HOST: &str = "localtest.me";
    const MARK: &str = "LEDGER-SKILL-7Q";
    const ASK: &str = "/ledger File today's entry. You must call the python tool before you answer.";
    const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
name = sys.argv[1]
out = os.environ.get("LEDGER_OUT") or sys.argv[2]
with open(out, "a") as f:
    f.write(json.dumps({"server": name, "pid": os.getpid(), "LOCAL_STUDIO_LOCAL_ONLY": os.environ.get("LOCAL_STUDIO_LOCAL_ONLY")}) + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": name.replace("-", "_"), "description": "Fixture tool for " + name, "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "ok"}]}
    elif method == "ping":
        result = {}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": str(method)}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#;

    fn executable(path: &Path, text: &str) -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(path)?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }

    fn listen() -> anyhow::Result<(u16, Arc<AtomicUsize>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buffer = vec![0_u8; 65_536];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..read]);
                let line = head.lines().next().unwrap_or_default();
                let listing = line.starts_with("GET ") && line.contains("/models");
                if !listing {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                println!("     stub {port} got {line:?}{}", if listing { " (model list, not counted)" } else { "" });
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        Ok((port, hits))
    }

    fn stub_provider(name: &str, base: &str, model: &str) -> Value {
        let mut entry = json!({
            "name": name,
            "api": "openai-completions",
            "apiKey": "none",
            "models": [{ "id": model, "name": model, "contextWindow": 32768, "maxTokens": 1024 }],
        });
        if !base.is_empty() {
            entry["baseUrl"] = json!(base);
        }
        entry
    }

    fn write_models(agent: &Path, port: u16, drift: &str) -> anyhow::Result<()> {
        let mut providers = Map::new();
        providers.insert(
            LOCAL.to_string(),
            json!({
                "name": "Ollama on this machine",
                "baseUrl": base(),
                "api": "openai-completions",
                "apiKey": "ollama",
                "models": [{ "id": mid(), "name": mid(), "contextWindow": 32768, "maxTokens": MAX_TOKENS }],
            }),
        );
        providers.insert(
            CLOUD.to_string(),
            stub_provider("Cloud stub", &format!("http://{CLOUD_HOST}:{port}/v1"), CLOUD_MODEL),
        );
        providers.insert(DRIFT.to_string(), stub_provider("Drift stub", drift, CLOUD_MODEL));
        for (id, base, _) in CASES {
            providers.insert(id.to_string(), stub_provider(id, base, CASE_MODEL));
        }
        let models = json!({ "providers": providers });
        write_private(&agent.join("models.json"), &serde_json::to_vec_pretty(&models)?)?;
        Ok(())
    }

    fn agent(root: &Path, port: u16, drift: &str, web: &Path) -> anyhow::Result<PathBuf> {
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent)?;
        write_models(&agent, port, drift)?;
        let settings = json!({
            "defaultProvider": LOCAL,
            "defaultModel": mid(),
            "mcpServers": { "web-search": {
                "type": "stdio",
                "command": web.join("server.py").to_string_lossy(),
                "args": ["web-search", web.join("started.jsonl").to_string_lossy()],
            }},
        });
        write_private(&agent.join("settings.json"), &serde_json::to_vec_pretty(&settings)?)?;
        write_private(&agent.join("auth.json"), b"{}")?;
        println!(
            "     scratch agent dir {}: {LOCAL} ({}) at {}, {CLOUD} at {CLOUD_HOST}:{port}, auth.json is {{}}",
            agent.display(),
            mid(),
            base()
        );
        Ok(agent)
    }

    fn put_server(agent: &Path, name: &str, server: Option<Value>) -> anyhow::Result<()> {
        let path = agent.join("settings.json");
        let mut settings: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let servers = settings.get_mut("mcpServers").and_then(Value::as_object_mut).context("mcpServers")?;
        match server {
            Some(server) => servers.insert(name.to_string(), server),
            None => servers.remove(name),
        };
        write_private(&path, &serde_json::to_vec_pretty(&settings)?)?;
        Ok(())
    }

    fn vault(plugins: &Path, out: &Path) -> anyhow::Result<()> {
        let dir = plugins.join("vault");
        std::fs::create_dir_all(dir.join("skills").join("vault"))?;
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"vault\"\nversion = \"0.1.0\"\ndescription = \"Second local-only fixture\"\nlocal_only = true\ncapabilities = [\"mcp-server\"]\n\n[[commands]]\nname = \"vault\"\ndescription = \"Open the vault\"\nskill = \"vault\"\n\n[mcp]\ncommand = \"server.py\"\nargs = [\"plugin-vault\", {}]\n",
                serde_json::to_string(&out.to_string_lossy())?
            ),
        )?;
        std::fs::write(
            dir.join("skills").join("vault").join("SKILL.md"),
            "---\nname: vault\ndescription: Open the vault.\n---\n\n# vault\n\nReply with only the word open.\n",
        )?;
        executable(&dir.join("server.py"), SERVER)?;
        grant(plugins, &dir)
    }

    fn grant(plugins: &Path, dir: &Path) -> anyhow::Result<()> {
        let loaded = plugin_store::load(dir).map_err(|errors| anyhow::anyhow!(errors.join("; ")))?;
        ensure!(loaded.local_only, "the fixture {} did not load as local_only", loaded.id);
        let mut grants = Grants::load(plugins).map_err(anyhow::Error::msg)?;
        grants.grant(&loaded, &[Capability::McpServer], None).map_err(anyhow::Error::msg)?;
        grants.save(plugins).map_err(anyhow::Error::msg)?;
        Ok(())
    }

    fn plugin(plugins: &Path) -> anyhow::Result<()> {
        let dir = plugins.join("ledger");
        std::fs::create_dir_all(dir.join("skills").join("ledger"))?;
        std::fs::create_dir_all(dir.join("prompts"))?;
        std::fs::write(
            dir.join("prompts").join("private.md"),
            format!("# private {MARK}\n\nReply with only the word done.\n"),
        )?;
        std::fs::write(
            dir.join("plugin.toml"),
            r#"id = "ledger"
version = "0.1.0"
description = "Fixture plugin for the local-only smoke"
local_only = true
capabilities = ["mcp-server"]
prompts = "prompts"

[[commands]]
name = "ledger"
description = "File a private ledger entry"
skill = "ledger"

[mcp]
command = "server.py"
args = ["plugin-ledger"]

[mcp.env]
LEDGER_OUT = "LOCAL_STUDIO_PLUGIN_LEDGER__OUT"
"#,
        )?;
        std::fs::write(
            dir.join("skills").join("ledger").join("SKILL.md"),
            format!("---\nname: ledger\ndescription: File a private ledger entry.\n---\n\n# ledger {MARK}\n\nRun this once in the Python tool: `print(await mcp.call_tool(\"plugin-ledger\", \"plugin_ledger\"))`. Then reply with only the word filed.\n"),
        )?;
        executable(&dir.join("server.py"), SERVER)?;
        grant(plugins, &dir)
    }

    fn settings(provider: &str, model: &str) -> ThreadSettings {
        ThreadSettings { provider: Some(provider.to_string()), ..super::settings(model) }
    }

    fn server_starts(path: &Path, server: &str) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|row| row.get("server").and_then(Value::as_str) == Some(server))
            .collect()
    }

    fn connections(socket: &Path, thread: &str) -> anyhow::Result<Vec<(String, bool)>> {
        session(socket, thread, |_| None)
    }

    fn session(
        socket: &Path,
        thread: &str,
        then: impl FnOnce(&str) -> Option<DaemonCommand>,
    ) -> anyhow::Result<Vec<(String, bool)>> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        runtime.block_on(async {
            let (client, _events) = DaemonClient::connect(socket).await?;
            let list = client
                .request_ok(DaemonCommand::List {
                    id: None,
                    all: Some(true),
                    cwd: None,
                    session_dir: None,
                    include_client_owned: Some(true),
                    rest: Map::default(),
                })
                .await?;
            let sessions =
                list.as_array().or_else(|| list.get("sessions").and_then(Value::as_array)).context("list")?;
            let summary = sessions
                .iter()
                .map(|session| session.get("summary").unwrap_or(session))
                .find(|summary| summary.get("sessionId").and_then(Value::as_str) == Some(thread))
                .with_context(|| format!("thread {thread} is not a live session: {list}"))?;
            let active = summary
                .get("activeSessionId")
                .or_else(|| summary.get("id"))
                .and_then(Value::as_str)
                .with_context(|| format!("no active session id in {summary}"))?
                .to_string();
            if let Some(command) = then(&active) {
                client.request_ok(command).await?;
            }
            let data = client
                .request_ok(DaemonCommand::GetMcpConnections {
                    id: None,
                    active_session_id: active,
                    rest: Map::default(),
                })
                .await?;
            let rows = data.get("connections").and_then(Value::as_array).cloned().unwrap_or_default();
            anyhow::Ok(
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("server")?.as_str()?.to_string(),
                            row.get("connected").and_then(Value::as_bool) == Some(true),
                        ))
                    })
                    .collect(),
            )
        })
    }

    fn usage(harness: &mut Harness, request: &str) -> anyhow::Result<UsageReport> {
        harness.view.usage = None;
        harness.studio.load_usage(request.to_string(), UsagePeriod::Day);
        harness.pump_until(&format!("usage report {request}"), harness.step, |view| view.usage.is_some())?;
        let report = harness.view.usage.clone().context("usage")?;
        for row in &report.by_model {
            println!("     usage {}/{}: {} tokens, {} requests", row.provider, row.model, row.tokens, row.requests);
        }
        Ok(report)
    }

    fn fence(socket: &Path, thread: &str, on: bool) -> anyhow::Result<()> {
        let server = json!({ "type": "stdio", "name": "intruder", "command": "/usr/bin/true", "cwd": "/" });
        let lists = if on {
            vec![("local-studio-ask", Vec::new()), ("smoke-intruder", vec![server])]
        } else {
            vec![("smoke-intruder", Vec::new())]
        };
        for (owner, servers) in lists {
            session(socket, thread, |active| {
                Some(DaemonCommand::ReplaceAcpMcpServers {
                    id: None,
                    active_session_id: active.to_string(),
                    owner_id: owner.to_string(),
                    servers: Value::Array(servers),
                    rest: Map::default(),
                })
            })?;
        }
        Ok(())
    }

    fn refused(
        harness: &mut Harness,
        key: &str,
        text: &str,
        on: ThreadSettings,
        hits: &AtomicUsize,
    ) -> anyhow::Result<String> {
        refused_draft(harness, key, prompt(text), on, hits)
    }

    fn refused_draft(
        harness: &mut Harness,
        key: &str,
        draft: PromptDraft,
        on: ThreadSettings,
        hits: &AtomicUsize,
    ) -> anyhow::Result<String> {
        let text = draft.text.clone();
        let studio = Arc::clone(&harness.studio);
        let (start, before) = (hits.load(Ordering::SeqCst), cloud_tokens(&usage(harness, &format!("{key}-before"))?));
        let project = harness.view.projects[0].id.clone();
        studio.start_thread(key.to_string(), Some(project), draft, on);
        harness.pump_until(&format!("{key} thread started"), STEP, |view| view.thread(key).is_some())?;
        let thread = harness.view.thread(key).context("thread")?;
        let refused = harness.pump_until(&format!("{key} refused"), Duration::from_secs(20), |view| {
            view.refusals.iter().any(|(id, _, _)| *id == thread)
        });
        std::thread::sleep(Duration::from_secs(3));
        let after = cloud_tokens(&usage(harness, &format!("{key}-after"))?);
        let sent = hits.load(Ordering::SeqCst) - start;
        studio.abort(thread.clone());
        harness.pump_until(&format!("{key} thread stopped"), STEP, |view| view.idle(&thread))?;
        let message = harness.view.refusals.iter().rev().find(|(id, _, _)| *id == thread).map(|(_, text, _)| text);
        ensure!(
            refused.is_ok() && sent == 0 && after == before,
            "{text:?}: refusal {message:?}, {sent} requests to {CLOUD_HOST}, {CLOUD} usage {before:?} -> {after:?}"
        );
        println!("     {key}: {message:?}");
        Ok(message.cloned().unwrap_or_default())
    }

    fn naming(result: anyhow::Result<String>, needle: &str) -> anyhow::Result<()> {
        let message = result?;
        ensure!(message.contains(needle), "the refusal does not name {needle}: {message}");
        Ok(())
    }

    fn image_draft(text: &str) -> PromptDraft {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
        PromptDraft {
            text: text.to_string(),
            images: vec![ImageAttachment { mime_type: "image/png".to_string(), data: png }],
            files: vec!["notes.txt".to_string()],
        }
    }

    fn draft_returned(harness: &mut Harness, hits: &AtomicUsize) -> anyhow::Result<()> {
        let before = harness.view.other.len();
        refused_draft(harness, "draft", image_draft(ASK), settings(CLOUD, CLOUD_MODEL), hits)?;
        let thread = harness.view.thread("draft").context("draft thread")?;
        let back = harness.view.other[before..].iter().find(|update| {
            update.starts_with("ComposerDraft")
                && update.contains(&format!("{thread:?}"))
                && update.contains("image/png")
                && update.contains("notes.txt")
        });
        ensure!(
            back.is_some(),
            "the refused draft's image and file did not come back to the composer; composer text {:?}",
            harness.view.composer.iter().find(|(key, _)| *key == thread)
        );
        Ok(())
    }

    fn drifted(
        harness: &mut Harness,
        agent: &Path,
        port: u16,
        drift_port: u16,
        drift_hits: &AtomicUsize,
    ) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let project = harness.view.projects[0].id.clone();
        studio.start_thread("drift".to_string(), Some(project), prompt(ASK), settings(DRIFT, CLOUD_MODEL));
        harness.pump_until("drift thread started", STEP, |view| view.thread("drift").is_some())?;
        let thread = harness.view.thread("drift").context("drift thread")?;
        harness.pump_until("drift refused while models.json lists it as cloud", Duration::from_secs(20), |view| {
            view.refusals.iter().any(|(id, _, _)| *id == thread)
        })?;
        write_models(agent, port, &format!("http://127.0.0.1:{drift_port}/v1"))?;
        println!("     models.json now lists {DRIFT} at 127.0.0.1:{drift_port}; the session still has {CLOUD_HOST}");
        studio.send("drift-again".to_string(), thread.clone(), prompt(ASK), Delivery::Queue);
        let second =
            harness.pump_until("drift refused on the session's own provider", Duration::from_secs(20), |view| {
                view.refusals.iter().filter(|(id, _, _)| *id == thread).count() == 2
            });
        std::thread::sleep(Duration::from_secs(3));
        for update in studio.tick() {
            harness.apply(update)?;
        }
        studio.abort(thread.clone());
        harness.pump_until("drift thread stopped", STEP, |view| view.idle(&thread))?;
        write_models(agent, port, &format!("http://{CLOUD_HOST}:{drift_port}/v1"))?;
        let message =
            harness.view.refusals.iter().rev().find(|(id, _, _)| *id == thread).map(|(_, text, _)| text.clone());
        let sent = drift_hits.load(Ordering::SeqCst);
        ensure!(
            second.is_ok() && sent == 0 && message.as_deref().is_some_and(|text| text.contains(CLOUD_HOST)),
            "after models.json moved {DRIFT} to a local address: refusal {message:?}, {sent} requests to the drift stub"
        );
        println!("     drift: {message:?}");
        Ok(())
    }

    fn url_cases(harness: &mut Harness, thread: &str) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let mut wrong = Vec::new();
        for (id, base, local) in CASES {
            let notices = harness.view.notices.len();
            studio.set_model(thread.to_string(), id.to_string(), CASE_MODEL.to_string());
            let refusal = format!("Start a new thread to use {id}.");
            let outcome = harness.pump_until(&format!("{id} ({base:?}) decided"), Duration::from_secs(20), |view| {
                view.notices[notices..].iter().any(|notice| notice.contains(&refusal))
                    || view.threads.iter().any(|row| row.id == thread && row.provider.as_deref() == Some(id))
            });
            let switched =
                harness.view.threads.iter().any(|row| row.id == thread && row.provider.as_deref() == Some(id));
            if outcome.is_err() || switched != local {
                wrong.push(format!(
                    "{id} {base:?}: expected {}, got {}",
                    if local { "local" } else { "cloud" },
                    if switched { "local" } else { "cloud" }
                ));
            }
            println!(
                "     {id} {base:?}: {}",
                if switched { "local, the thread switched" } else { "cloud, the switch was refused" }
            );
            if switched {
                studio.set_model(thread.to_string(), LOCAL.to_string(), mid().to_string());
                harness.pump_until("back on the local model", Duration::from_secs(20), |view| {
                    view.threads.iter().any(|row| row.id == thread && row.provider.as_deref() == Some(LOCAL))
                })?;
            }
        }
        ensure!(wrong.is_empty(), "misclassified: {}", wrong.join("; "));
        Ok(())
    }

    fn queued_during_run(harness: &mut Harness, socket: &Path, ledger_out: &Path) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let project = harness.view.projects[0].id.clone();
        let before = server_starts(ledger_out, "plugin-ledger").len();
        let long = "Write a 700-word story about a lighthouse keeper. Do not call any tools.";
        studio.start_thread("busy".to_string(), Some(project), prompt(long), settings(LOCAL, mid()));
        harness.pump_until("busy thread started", STEP, |view| view.thread("busy").is_some())?;
        let thread = harness.view.thread("busy").context("busy thread")?;
        studio.select_thread(Some(thread.clone()));
        harness.pump_until("busy thread working", STEP, |view| !view.idle(&thread))?;
        studio.abort(thread.clone());
        harness.pump_until("busy thread stopped", STEP, |view| view.idle(&thread))?;
        ensure!(held(socket, &thread)?, "the busy thread was not offered Local Studio's own tools");
        studio.send("long".to_string(), thread.clone(), prompt(long), Delivery::Queue);
        harness.pump_until("busy thread working again", STEP, |view| !view.idle(&thread))?;
        studio.send("queued".to_string(), thread.clone(), prompt(ASK), Delivery::Queue);
        harness
            .pump_until("still working after the queued /ledger", Duration::from_secs(2), |view| !view.idle(&thread))?;
        harness.pump_until("the queued /ledger ran", STEP * 5, |view| {
            view.idle(&thread) && user_text(view.items(&thread)).iter().any(|text| text.contains(MARK))
        })?;
        let starts = server_starts(ledger_out, "plugin-ledger").split_off(before);
        ensure!(
            !starts.is_empty()
                && starts.iter().all(|row| row.get("LOCAL_STUDIO_LOCAL_ONLY").and_then(Value::as_str) == Some("1")),
            "plugin-ledger did not start for the /ledger queued during a local turn: {starts:?}"
        );
        ensure!(!held(socket, &thread)?, "Local Studio's own tools are still in the queued /ledger session");
        Ok(())
    }

    fn held_across_restart(harness: &mut Harness, config: &StudioConfig, data: &Path) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        let project = harness.view.projects[0].id.clone();
        let long = "Write a 700-word story about a lighthouse keeper. Do not call any tools.";
        studio.start_thread("restart".to_string(), Some(project), prompt(long), settings(LOCAL, mid()));
        harness.pump_until("restart thread started", STEP, |view| view.thread("restart").is_some())?;
        let thread = harness.view.thread("restart").context("restart thread")?;
        studio.select_thread(Some(thread.clone()));
        harness.pump_until("restart thread working", STEP, |view| !view.idle(&thread))?;
        let notices = harness.view.notices.len();
        studio.send("held".to_string(), thread.clone(), prompt(ASK), Delivery::Queue);
        harness.pump_until("the /ledger is held behind the turn", Duration::from_secs(20), |view| {
            view.notices[notices..].iter().any(|notice| notice.contains("waits for the current turn"))
        })?;
        ensure!(!harness.view.idle(&thread), "the turn ended before the app quit, so nothing was held across it");
        harness.studio.shutdown();
        println!("ok   the app quit with the /ledger held and the turn still running");
        harness.studio = Studio::new(config.clone(), Arc::clone(&harness.signal) as Arc<dyn Waker>)?;
        harness.view = View::default();
        harness
            .pump_until("reopened app connected", STEP, |view| matches!(view.connection, Some(Connection::Ready)))?;
        harness.pump_until("reopened app lists the thread", STEP, |view| {
            view.threads.iter().any(|row| row.id == thread)
        })?;
        let studio = Arc::clone(&harness.studio);
        studio.select_thread(Some(thread.clone()));
        let after = "Reply with only the word after.";
        studio.send("after".to_string(), thread.clone(), prompt(after), Delivery::Queue);
        harness.pump_until("the held /ledger ran before the later message", STEP * 5, |view| {
            let sent = user_text(view.items(&thread));
            let ledger = sent.iter().position(|text| text.contains(MARK));
            let later = sent.iter().position(|text| text.contains(after));
            view.idle(&thread) && ledger.zip(later).is_some_and(|(ledger, later)| ledger < later)
        })?;
        let state: Value = serde_json::from_slice(&std::fs::read(data.join("state.json"))?)?;
        let left = state.pointer(&format!("/threads/{thread}/held")).cloned();
        ensure!(left.is_none(), "state.json still holds {left:?} after the daemon took the messages");
        Ok(())
    }

    fn cloud_tokens(report: &UsageReport) -> (u64, u64) {
        report
            .by_model
            .iter()
            .filter(|row| row.provider == CLOUD)
            .fold((0, 0), |(tokens, requests), row| (tokens + row.tokens, requests + row.requests))
    }

    pub(super) fn run(daemon: &Path, root: &Path) -> anyhow::Result<()> {
        pick_model()?;
        let (port, hits) = listen()?;
        let (drift_port, drift_hits) = listen()?;
        let resolved: Vec<std::net::SocketAddr> =
            std::net::ToSocketAddrs::to_socket_addrs(&(CLOUD_HOST, port)).map(Iterator::collect).unwrap_or_default();
        ensure!(
            resolved.iter().any(|addr| addr.ip().is_loopback()),
            "{CLOUD_HOST} must resolve to loopback for the cloud stub to be observable: {resolved:?}"
        );
        let web = root.join("web");
        std::fs::create_dir_all(&web)?;
        executable(&web.join("server.py"), SERVER)?;
        let agent = agent(root, port, &format!("http://{CLOUD_HOST}:{drift_port}/v1"), &web)?;
        let data = root.join("data");
        let plugins = data.join("plugins");
        let ledger_out = root.join("ledger.jsonl");
        plugin(&plugins)?;
        vault(&plugins, &ledger_out)?;
        let work = root.join("work");
        std::fs::create_dir_all(&work)?;
        Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&work).status()?;
        std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
        std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
        std::env::set_var(LEDGER_ENV, &ledger_out);
        let tools = web.join("local-studio-browser");
        let (stub, started) = (web.join("server.py"), root.join("tools.jsonl"));
        executable(&tools, &format!("#!/bin/sh\nexec {} local-studio {}\n", stub.display(), started.display()))?;
        std::env::set_var("LOCAL_STUDIO_BROWSER_BIN", &tools);
        std::fs::write(data.join("settings.json"), r#"{"browserUse":true,"computerUse":true}"#)?;
        let socket = root.join("d.sock");
        let config = StudioConfig {
            socket_path: Some(socket.to_string_lossy().into_owned()),
            session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
            data_dir: None,
            controller_main: None,
            launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
            take_over_serve: false,
        };
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config.clone(), Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut harness = Harness { studio, signal, view: View::default(), step: STEP };
        let stubs = Stubs { port, hits: &hits, drift_port, drift_hits: &drift_hits };
        let outcome = drive(&mut harness, &work, &agent, &socket, &ledger_out, &stubs).and_then(|mut failed| {
            match held_across_restart(&mut harness, &config, &data) {
                Ok(()) => println!("ok   scenario restart"),
                Err(error) => {
                    println!("FAIL scenario restart: {error:#}");
                    failed.push("restart".to_string());
                }
            }
            ensure!(failed.is_empty(), "failed scenarios: {}", failed.join(", "));
            Ok(())
        });
        harness.studio.shutdown();
        let stopped = stop_daemon(&socket);
        outcome?;
        stopped?;
        println!("PASS local-only plugin smoke");
        Ok(())
    }

    struct Stubs<'a> {
        port: u16,
        hits: &'a AtomicUsize,
        drift_port: u16,
        drift_hits: &'a AtomicUsize,
    }

    #[allow(clippy::too_many_lines)]
    fn drive(
        harness: &mut Harness,
        work: &Path,
        agent: &Path,
        socket: &Path,
        ledger_out: &Path,
        stubs: &Stubs,
    ) -> anyhow::Result<Vec<String>> {
        let hits = stubs.hits;
        let studio = Arc::clone(&harness.studio);
        let step = STEP;
        let ask = ASK;
        harness.pump_until("daemon connected", step, |view| matches!(view.connection, Some(Connection::Ready)))?;
        studio.add_project(work.to_string_lossy().into_owned());
        harness.pump_until("project added", step, |view| view.projects.len() == 1)?;
        let project = harness.view.projects[0].id.clone();

        let commands = harness.list_commands("label")?;
        let listed = commands.iter().find(|command| command.name == "ledger").context("/ledger is not listed")?;
        ensure!(
            listed.description.contains(plugin_store::LOCAL_ONLY_LABEL),
            "/ledger is listed without the label: {listed:?}"
        );
        println!("ok   label: /ledger listed as {:?}", listed.description);
        let settings_json: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
        ensure!(
            settings_json.pointer("/mcpServers/plugin-ledger").is_none(),
            "the local-only server was written to the global settings.json"
        );
        println!("ok   the local-only plugin's MCP server is not in the global settings.json");

        studio.start_thread("web".to_string(), Some(project.clone()), prompt(ask), settings(LOCAL, mid()));
        harness.pump_until("local thread with web-search started", step, |view| view.thread("web").is_some())?;
        let web_thread = harness.view.thread("web").context("web thread")?;
        harness.pump_until("refused while web-search is connected", step, |view| {
            view.refusals.iter().any(|(thread, message, _)| *thread == web_thread && message.contains("web-search"))
        })?;
        println!("ok   check 4a: a local thread with a connected web-search MCP server refuses /ledger and names it");
        put_server(agent, "web-search", None)?;

        studio.start_thread("cloud".to_string(), Some(project.clone()), prompt(ask), settings(CLOUD, CLOUD_MODEL));
        harness.pump_until("cloud thread started", step, |view| view.thread("cloud").is_some())?;
        let cloud = harness.view.thread("cloud").context("cloud thread")?;
        harness.pump_until("refused on the cloud provider", step, |view| {
            view.refusals.iter().any(|(thread, _, _)| *thread == cloud)
        })?;
        let (_, message, local) =
            harness.view.refusals.iter().find(|(thread, _, _)| *thread == cloud).cloned().context("refusal")?;
        let local = local.context("the refusal offered no local model")?;
        ensure!(local.provider == LOCAL && local.model == mid(), "offered {local:?}");
        ensure!(message.contains("runs only on local models"), "plain refusal text missing: {message}");
        studio.select_thread(Some(cloud.clone()));
        harness.pump_until("cloud thread selected", step, |view| view.threads.iter().any(|row| row.id == cloud))?;
        std::thread::sleep(Duration::from_secs(3));
        for update in studio.tick() {
            harness.apply(update)?;
        }
        let sent = user_text(harness.view.items(&cloud));
        ensure!(sent.is_empty(), "the refused prompt reached the session: {sent:?}");
        let report = usage(harness, "after-refusal")?;
        let (tokens, requests) = cloud_tokens(&report);
        ensure!(tokens == 0 && requests == 0, "{CLOUD} shows {tokens} tokens, {requests} requests");
        ensure!(hits.load(Ordering::SeqCst) == 0, "the cloud endpoint got a request");
        println!(
            "ok   check 1: /ledger on {CLOUD} refused ({message}); transcript has no user message, usage shows {CLOUD} 0 tokens 0 requests, the {CLOUD_HOST} endpoint saw 0 requests; offered {}",
            local.model
        );

        let mut failed: Vec<String> = Vec::new();
        let mut record = |name: &str, result: anyhow::Result<()>| match result {
            Ok(()) => println!("ok   scenario {name}"),
            Err(error) => {
                println!("FAIL scenario {name}: {error:#}");
                failed.push(name.to_string());
            }
        };
        fence(socket, &cloud, true)?;
        studio.run_local(cloud.clone(), local.clone());
        let fenced = harness.pump_until("RunLocal failed on the fenced session", step, |view| {
            view.notices.iter().any(|notice| notice.contains("Could not start the plugin's tool server"))
        });
        fence(socket, &cloud, false)?;
        harness.pump_until("composer settled", Duration::from_secs(3), |_| false).ok();
        let text = harness.view.composer.iter().find(|(key, _)| *key == cloud).map(|(_, text)| text.clone());
        let draft = harness
            .view
            .other
            .iter()
            .rev()
            .find(|update| update.starts_with("ComposerDraft") && update.contains(&format!("{cloud:?}")));
        let holds =
            text.as_deref() == Some(ask) || draft.is_some_and(|draft| draft.contains(&format!("text: {ask:?}")));
        record(
            "run-local-error",
            fenced.and_then(|()| {
                ensure!(holds, "after the failed RunLocal the composer holds {text:?}, draft {draft:?}");
                Ok(())
            }),
        );
        for (key, text) in [
            ("private", "/private"),
            ("bang", "Please use $ledger! now."),
            ("quoted", "Use \"$ledger\" for this."),
            ("apostrophe", "Check $ledger's last entry."),
        ] {
            record(key, refused(harness, key, text, settings(CLOUD, CLOUD_MODEL), hits).map(|_| ()));
        }
        let off = json!({ "type": "stdio", "command": "/usr/bin/true", "enabled": false });
        put_server(agent, "web-off", Some(off))?;
        record("server-off", refused(harness, "off", ask, settings(LOCAL, mid()), hits).map(|_| ()));
        put_server(agent, "web-off", None)?;
        let clash = json!({ "type": "stdio", "command": "/usr/bin/true" });
        put_server(agent, "plugin-ledger", Some(clash))?;
        record("clash", naming(refused(harness, "clash", ask, settings(LOCAL, mid()), hits), "plugin-ledger"));
        put_server(agent, "plugin-ledger", None)?;
        std::env::remove_var(LEDGER_ENV);
        record("env", naming(refused(harness, "env", ask, settings(LOCAL, mid()), hits), LEDGER_ENV));
        std::env::set_var(LEDGER_ENV, ledger_out);
        record("draft", draft_returned(harness, hits));
        record("drift", drifted(harness, agent, stubs.port, stubs.drift_port, stubs.drift_hits));
        record("queued", queued_during_run(harness, socket, ledger_out));
        studio.run_local(cloud.clone(), local.clone());
        harness.pump_until("switched to the local model and running", step, |view| {
            view.threads.iter().any(|row| {
                row.id == cloud && row.model.as_deref() == Some(mid()) && matches!(row.status, ThreadStatus::Working)
            })
        })?;
        studio.set_model(cloud.clone(), CLOUD.to_string(), CLOUD_MODEL.to_string());
        harness.pump_until("switch to cloud refused during the run", step, |view| {
            view.notices.iter().any(|notice| notice.contains("stays on local models"))
        })?;
        println!("ok   check 3: switching the running /ledger thread to {CLOUD} was refused");
        harness.pump_until("the /ledger turn finished locally", STEP * 5, |view| view.idle(&cloud))?;
        studio.select_thread(Some(cloud.clone()));
        harness.pump_until("ledger transcript reloaded", step, |view| !user_text(view.items(&cloud)).is_empty())?;
        let sent = user_text(harness.view.items(&cloud));
        ensure!(sent.iter().any(|text| text.contains(MARK)), "/ledger did not expand: {sent:?}");
        let row = harness.view.threads.iter().find(|row| row.id == cloud).context("row")?;
        ensure!(row.provider.as_deref() == Some(LOCAL) && row.model.as_deref() == Some(mid()), "model moved: {row:?}");
        for item in harness.view.items(&cloud).iter().rev().take(2) {
            println!("     last item {:.300}", format!("{:?}", item.kind));
        }
        let report = usage(harness, "after-local")?;
        let local_tokens: u64 = report.by_model.iter().filter(|row| row.provider == LOCAL).map(|row| row.tokens).sum();
        ensure!(local_tokens > 0, "no local tokens counted");
        ensure!(cloud_tokens(&report) == (0, 0), "{CLOUD} usage after the local run");
        ensure!(hits.load(Ordering::SeqCst) == 0, "the cloud endpoint got a request");
        println!("ok   check 2: the same /ledger completed on {LOCAL}/{} ({local_tokens} tokens), still on it", mid());

        let vault = server_starts(ledger_out, "plugin-vault");
        record(
            "only-invoked",
            if vault.is_empty() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("plugin-vault started in sessions that never ran /vault: {vault:?}"))
            },
        );
        record("url-cases", url_cases(harness, &cloud));

        let ledger = server_starts(ledger_out, "plugin-ledger");
        ensure!(
            !ledger.is_empty()
                && ledger.iter().all(|row| row.get("LOCAL_STUDIO_LOCAL_ONLY").and_then(Value::as_str) == Some("1")),
            "the plugin server did not start with LOCAL_STUDIO_LOCAL_ONLY=1: {ledger:?}"
        );
        let servers = connections(socket, &cloud)?;
        let connected: Vec<&str> =
            servers.iter().filter(|(name, up)| *up && name != "plugin-ledger").map(|(name, _)| name.as_str()).collect();
        ensure!(connected.is_empty(), "other MCP servers are connected in the /ledger session: {connected:?}");
        ensure!(!servers.iter().any(|(name, _)| name == "web-search"), "web-search is still in the /ledger session");
        ensure!(!held(socket, &cloud)?, "Local Studio's own tools are still in the /ledger session");
        println!(
            "ok   check 4b: the /ledger session lists {} MCP connections, none connected besides the plugin and no web-search; plugin-ledger started {} time(s), each with LOCAL_STUDIO_LOCAL_ONLY=1",
            servers.len(),
            ledger.len()
        );

        let refusals = harness.view.refusals.len();
        let ledgers = |view: &View| user_text(view.items(&cloud)).iter().filter(|text| text.contains(MARK)).count();
        let before = ledgers(&harness.view);
        studio.send("again".to_string(), cloud.clone(), prompt(ASK), Delivery::Queue);
        harness.pump_until("the second /ledger in the same thread ran or was refused", STEP * 5, |view| {
            view.refusals.len() > refusals || (ledgers(view) > before && view.idle(&cloud))
        })?;
        let refused: Vec<&String> = harness.view.refusals[refusals..]
            .iter()
            .filter(|(id, _, _)| *id == cloud)
            .map(|(_, text, _)| text)
            .collect();
        ensure!(refused.is_empty(), "a second /ledger after plugin-ledger connected was refused: {refused:?}");
        harness.pump_until("the second /ledger finished", STEP * 5, |view| view.idle(&cloud))?;
        println!("ok   check 4c: a second /ledger in the same thread ran after plugin-ledger connected");

        studio.start_thread(
            "control".to_string(),
            Some(project),
            prompt("Reply with only the word hi."),
            settings(CLOUD, CLOUD_MODEL),
        );
        harness.pump_until("control thread started", step, |view| view.thread("control").is_some())?;
        let control = harness.view.thread("control").context("control")?;
        let deadline = Instant::now() + step;
        while hits.load(Ordering::SeqCst) == 0 {
            ensure!(Instant::now() < deadline, "a plain prompt on {CLOUD} never reached {CLOUD_HOST}");
            for update in studio.tick() {
                harness.apply(update)?;
            }
            harness.signal.wait(Duration::from_millis(200));
        }
        studio.abort(control);
        println!(
            "ok   control: a plain prompt on {CLOUD} reached the endpoint ({} connections), so the refusal's 0 means nothing was sent",
            hits.load(Ordering::SeqCst)
        );
        Ok(failed)
    }

    const WAITS: &str = "Gantrel is still installing. Answer its sheet, then send again.";

    fn put(path: &Path, text: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(path.parent().context("parent")?)?;
        Ok(std::fs::write(path, text)?)
    }

    fn skill(path: &Path, name: &str, body: &str) -> anyhow::Result<()> {
        put(&path.join(name).join("SKILL.md"), &format!("---\nname: {name}\ndescription: About {name}\n---\n{body}\n"))
    }

    fn manifest(dir: &Path, name: &str, more: &str) -> anyhow::Result<()> {
        let id = dir.file_name().and_then(|id| id.to_str()).context("id")?;
        let head = format!("id = {id:?}\nname = {name:?}\nversion = \"0.1.0\"\ndescription = \"All of {id}\"\n");
        put(&dir.join("plugin.toml"), &format!("{head}{more}"))
    }

    pub(super) fn mentions(daemon: &Path, root: &Path) -> anyhow::Result<()> {
        let ((port, hits), (local, _)) = (listen()?, listen()?);
        let base = format!("http://127.0.0.1:{local}/v1");
        ensure!(LOCAL_MODEL.set((MID.to_string(), base.clone())).is_ok(), "a model was picked already");
        let agent = agent(root, port, &base, root)?;
        put_server(&agent, "web-search", None)?;
        let (data, work) = (root.join("data"), root.join("work"));
        let plugins = data.join("plugins");
        plugin(&plugins)?;
        for (id, name, skills) in [("solo", "Solo Tools", &["alone"][..]), ("kit", "Solo", &["hammer", "saw"])] {
            let command =
                "capabilities = []\n[[commands]]\nname = \"build\"\ndescription = \"Build it\"\nskill = \"saw\"\n";
            manifest(&plugins.join(id), name, if id == "kit" { command } else { "capabilities = []\n" })?;
            skills
                .iter()
                .try_for_each(|name| skill(&plugins.join(id).join("skills"), name, &format!("{name} steps")))?;
        }
        manifest(
            &root.join("gantrel"),
            "Gantrel",
            "capabilities = [\"mcp-server\"]\n[mcp]\ncommand = \"bin/run.sh\"\n",
        )?;
        std::fs::create_dir_all(root.join("gantrel/bin"))?;
        executable(&root.join("gantrel/bin/run.sh"), "#!/bin/sh\nexec cat\n")?;
        skill(&data.join("skills"), "marmalade", "orange steps")?;
        skill(&data.join("skills"), "marmalade-pot", "pot steps")?;
        skill(&data.join("skills"), "create", "carpentry steps")?;
        skill(&data.join("skills"), "ledger", "your own ledger")?;
        let shipped = root.join("app-skills");
        skill(&shipped, "anchor", "anchor steps")?;
        skill(&shipped, "solo", "shipped steps")?;
        for id in ["alpha", "beta"] {
            manifest(&plugins.join(id), &id.to_uppercase(), "capabilities = []\n")?;
            skill(&plugins.join(id).join("skills"), "setup", &format!("{id} steps"))?;
        }
        put(&work.join("marmalade 2026.csv"), "a,b\n")?;
        let cold = root.join("cold");
        for (dir, name) in [(&work, "marmot"), (&root.join("plain"), "badger"), (&cold, "otter")] {
            skill(&dir.join(".agents/skills"), name, &format!("{name} steps"))?;
            Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(dir).status()?;
        }
        std::fs::create_dir_all(root.join("plain/marmalade"))?;
        let trusted = [work.canonicalize()?, cold.canonicalize()?];
        put(&data.join("state.json"), &json!({ "version": 1, "projects": [], "trusted": trusted }).to_string())?;
        std::fs::create_dir_all(root.join("bin"))?;
        let path = std::env::var("PATH")?;
        let (bin, hang) = (root.join("bin").canonicalize()?, root.canonicalize()?.join("hang"));
        let hold = format!("while [ -e '{}' ]; do sleep 0.1; done; exit 1", hang.display());
        let git =
            format!("#!/bin/sh\ncase \"$*\" in *baney75/Gantrel*) {hold};; esac\nPATH={path:?} exec git \"$@\"\n");
        executable(&bin.join("git"), &git)?;
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        std::env::set_var("HOME", root.join("home"));
        std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
        std::env::set_var("LOCAL_STUDIO_SKILLS_DIR", &shipped);
        std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
        let socket = root.join("d.sock");
        let config = StudioConfig {
            socket_path: Some(socket.to_string_lossy().into_owned()),
            session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
            data_dir: None,
            controller_main: None,
            launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
            take_over_serve: false,
        };
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut harness = Harness { studio, signal, view: View::default(), step: STEP };
        let outcome = sends(&mut harness, root, &hits).and_then(|()| installs(&mut harness, root, &hang));
        harness.studio.shutdown();
        let stopped = stop_daemon(&socket);
        outcome?;
        stopped?;
        println!("PASS plugin smoke: mentions");
        Ok(())
    }

    fn add(harness: &mut Harness, dir: &Path) -> anyhow::Result<String> {
        let path = dir.canonicalize()?.to_string_lossy().into_owned();
        harness.studio.add_project(path.clone());
        harness.pump_until("project added", STEP, |view| view.projects.iter().any(|project| project.id == path))?;
        if path.ends_with("cold") {
            return Ok(path);
        }
        harness.studio.mentions(path.clone(), Some(path.clone()), "m".to_string(), 30);
        let seen = format!("Mentions {{ request: {path:?}");
        harness.pump_until("its @ list", STEP, |view| view.other.iter().any(|update| update.starts_with(&seen)))?;
        Ok(path)
    }

    fn kept(root: &Path, thread: &str) -> Option<String> {
        let file = std::fs::read_to_string(root.join("agent/sessions").join(thread).join("main.jsonl")).ok()?;
        let lines = file.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok());
        let entry = |line: Value| {
            let writes = line["writes"].as_array()?.clone();
            writes.into_iter().find(|write| write["value"]["kind"] == "pi.user")
        };
        let user = lines.into_iter().find_map(entry)?;
        user["value"]["model"][0]["content"][0]["text"].as_str().map(str::to_string)
    }

    fn sent(harness: &mut Harness, root: &Path, key: &str, project: &str, text: &str) -> anyhow::Result<String> {
        let studio = Arc::clone(&harness.studio);
        studio.start_thread(key.to_string(), Some(project.to_string()), prompt(text), settings(LOCAL, mid()));
        harness.pump_until(&format!("{key} started"), STEP, |view| view.thread(key).is_some())?;
        let thread = harness.view.thread(key).context("thread")?;
        studio.select_thread(Some(thread.clone()));
        harness.pump_until(&format!("{key} sent"), STEP, |view| !user_text(view.items(&thread)).is_empty())?;
        studio.abort(thread.clone());
        let shown = user_text(harness.view.items(&thread));
        ensure!(shown == [text], "{key}: {text:?} is shown as {shown:?}");
        harness.pump_until(&format!("{key} kept"), STEP, |_| kept(root, &thread).is_some())?;
        kept(root, &thread).context("the session's user text")
    }

    fn sends(harness: &mut Harness, root: &Path, hits: &AtomicUsize) -> anyhow::Result<()> {
        harness.pump_until("daemon connected", STEP, |view| matches!(view.connection, Some(Connection::Ready)))?;
        let [work, plain, cold] = ["work", "plain", "cold"].map(|name| add(harness, &root.join(name)));
        let (work, plain, cold) = (work?, plain?, cold?);
        let lock = std::fs::File::open(root.join("data/skills.lock"))?;
        for (key, project, text, blocks) in [
            ("skill", &work, "@marmalade make some", &["orange steps"][..]),
            ("twice", &work, "(@marmalade) and @marmalade.", &["orange steps"]),
            ("longest", &work, "in @marmalade-pot", &["pot steps"]),
            ("project", &work, "@marmot", &["marmot steps"]),
            ("untrusted", &plain, "@badger", &[]),
            ("folder", &plain, "@marmalade make some", &[]),
            ("mail", &work, "mail a@marmalade, a@marmalade.com or @marmalade.com", &[]),
            ("other", &work, "@marmalade-x @marmalade/x @Marmalade @hammers", &[]),
            ("solo", &work, "@solo then @Solo Tools, $alone, @alone", &["alone steps"]),
            (
                "kit",
                &work,
                "@kit and @Solo",
                &["All of kit", "- skill hammer: About hammer (", "- command /build: Build it"],
            ),
            ("shipped", &work, "@anchor", &["anchor steps"]),
            ("absent", &work, "@gantrel", &[]),
            ("own create", &work, "ask @create", &["carpentry steps"]),
            ("spaced", &work, "@marmalade 2026.csv please", &[]),
            ("cold", &cold, "@otter", &["otter steps"]),
        ] {
            if key == "skill" {
                lock.lock()?;
            }
            let got = sent(harness, root, key, project, text)?;
            lock.unlock()?;
            let once = blocks.iter().all(|block| got.matches(block).count() == 1);
            let tail = if blocks.is_empty() { got == text } else { got.ends_with(&format!("\n\n{text}")) };
            let shown: usize = ["<skill name=", "<plugin name="].iter().map(|tag| got.matches(tag).count()).sum();
            ensure!(once && tail && shown == usize::from(!blocks.is_empty()), "{key}: {text:?} was sent as {got:?}");
            println!("ok   {key}: {text:?} sent with {blocks:?}");
        }
        let got = sent(harness, root, "create", &work, "@create from @marmalade")?;
        let block = got.starts_with("Turn what you did") && got.ends_with("\n\nfrom @marmalade");
        let named = got.contains("orange steps") && !got.contains("carpentry steps");
        ensure!(block && named, "@create was sent as {got:?}");
        println!("ok   a first word @create is the command, never a skill of that name");
        ensure!(!root.join("data/plugins/gantrel").exists(), "a name in a message installed its plugin");
        let got = sent(harness, root, "both", &work, "@alpha then @beta")?;
        ensure!(["alpha steps", "beta steps"].iter().all(|steps| got.contains(steps)), "both: sent as {got:?}");
        let marmot = root.join("work/.agents/skills/marmot/SKILL.md");
        put(&root.join("elsewhere.md"), "---\nname: marmot\ndescription: About marmot\n---\nelsewhere\n")?;
        std::fs::remove_file(&marmot)?;
        std::os::unix::fs::symlink(root.join("elsewhere.md"), &marmot)?;
        lock.lock()?;
        let got = sent(harness, root, "swapped", &work, "@marmot")?;
        lock.unlock()?;
        ensure!(got == "@marmot", "a project's skill swapped for a link was sent as {got:?}");
        harness.studio.remove_project(plain);
        harness.studio.remove_project(cold);
        harness.pump_until("projects removed", STEP, |view| view.projects.len() == 1)?;
        for (key, text) in [("gate", "File it with @ledger"), ("slash", "/review the @Ledger entries")] {
            naming(refused(harness, key, text, settings(CLOUD, CLOUD_MODEL), hits), "runs only on local models")?;
        }
        Ok(())
    }

    fn waits(harness: &mut Harness, key: &str) -> anyhow::Result<()> {
        let thread = harness.view.thread("absent").context("thread")?;
        for (request, text) in [(format!("{key}-id"), "use @gantrel"), (format!("{key}-name"), "@Gantrel, please")] {
            harness.studio.send(request.clone(), thread.clone(), prompt(text), Delivery::Queue);
            let failed = format!("RequestFailed {request}: {WAITS}");
            harness.pump_until(&failed, STEP, |view| view.other.contains(&failed))?;
        }
        let project = harness.view.projects[0].id.clone();
        harness.studio.start_thread(key.to_string(), Some(project), image_draft("@gantrel"), settings(LOCAL, mid()));
        harness.pump_until(&format!("{key}: the draft comes back"), STEP, |view| {
            let back = |update: &String| {
                ["ComposerDraft", "\"@gantrel\"", "image/png", "notes.txt"].iter().all(|part| update.contains(part))
            };
            view.thread(key)
                .is_some_and(|thread| view.other.iter().any(|update| back(update) && update.contains(&thread)))
        })
    }

    fn installs(harness: &mut Harness, root: &Path, hang: &Path) -> anyhow::Result<()> {
        let studio = Arc::clone(&harness.studio);
        std::fs::write(hang, "")?;
        studio.install_plugin("running".to_string(), "gantrel".to_string());
        let waited = waits(harness, "while-it-runs");
        std::fs::remove_file(hang)?;
        waited?;
        harness.pump_until("the install failed", STEP, |view| {
            view.other.iter().any(|update| update.starts_with("RequestFailed running"))
        })?;
        let work = harness.view.projects[0].id.clone();
        let text = sent(harness, root, "after-failure", &work, "@gantrel")?;
        ensure!(text == "@gantrel", "after a failed install the message became {text:?}");
        studio.install_plugin("sheet".to_string(), root.join("gantrel").to_string_lossy().into_owned());
        harness.pump_until("its sheet is open", STEP, |view| {
            view.other.iter().any(|update| update.starts_with("Consents") && update.contains("\"gantrel\""))
        })?;
        waits(harness, "while-it-asks")?;
        studio.answer_consent("sheet".to_string(), ConsentAnswer::Cancel);
        let text = sent(harness, root, "after-cancel", &work, "@gantrel")?;
        ensure!(text == "@gantrel", "after a declined install the message became {text:?}");
        let folder = root.join("gantrel").to_string_lossy().into_owned();
        let seen = harness.view.other.len();
        studio.install_plugin("kept".to_string(), folder);
        harness.pump_until("its sheet is open again", STEP, |view| {
            view.other[seen..].iter().any(|update| update.starts_with("Consents") && update.contains("\"gantrel\""))
        })?;
        studio.answer_consent("kept".to_string(), ConsentAnswer::Install { sensitive: Vec::new() });
        harness.pump_until("it is installed", STEP, |view| {
            view.other.iter().any(|update| update.contains("RequestDone { request: \"kept\""))
        })?;
        std::fs::write(hang, "")?;
        studio.install_plugin("again".to_string(), "gantrel".to_string());
        let text = sent(harness, root, "while-it-updates", &work, "@gantrel");
        let running = !harness.view.other.iter().any(|update| update.starts_with("RequestFailed again"));
        std::fs::remove_file(hang)?;
        let text = text?;
        ensure!(running, "the update ended before the message was sent");
        ensure!(text.ends_with("@gantrel"), "an installed plugin's update makes the message {text:?}");
        harness.pump_until("the update failed", STEP, |view| {
            view.other.iter().any(|update| update.starts_with("RequestFailed again"))
        })?;
        Ok(())
    }
}

mod hub {
    use super::{
        ensure, plugin_store, Arc, Command, Context, Duration, Instant, Path, Signal, Studio, StudioConfig, Update,
        Value, Waker,
    };
    use local_studio_core::{CatalogPlugin, ConsentAnswer, ConsentRequest, PluginRow, PluginState};

    pub const USAGE: &str =
        "usage: plugin_smoke --hub <scratch dir> (needs git and network access to github.com; no daemon or model)";
    const ID: &str = "beacon";
    const CATALOG: &str = "smoothstone";
    const PLAIN: &str = "plainfolder";
    const STEP: Duration = Duration::from_secs(180);

    #[derive(Default)]
    struct Seen {
        plugins: Vec<PluginRow>,
        catalog: Vec<CatalogPlugin>,
        channel: String,
        checked: Option<i64>,
        consents: Vec<ConsentRequest>,
        sheets: usize,
        done: Vec<(String, Result<String, String>)>,
    }

    impl Seen {
        fn row(&self, id: &str) -> Option<&PluginRow> {
            self.plugins.iter().find(|row| row.id == id)
        }

        fn outcome(&self, request: &str) -> Option<&Result<String, String>> {
            self.done.iter().find(|(key, _)| key == request).map(|(_, outcome)| outcome)
        }

        fn sheet(&self, request: &str) -> Option<&ConsentRequest> {
            self.consents.iter().find(|consent| consent.request == request)
        }
    }

    struct Hub {
        studio: Arc<Studio>,
        signal: Arc<Signal>,
        seen: Seen,
    }

    impl Hub {
        fn pump(&mut self, step: &str, done: impl Fn(&Seen) -> bool) -> anyhow::Result<()> {
            let deadline = Instant::now() + STEP;
            loop {
                for update in self.studio.tick() {
                    match update {
                        Update::Plugins { plugins, catalog, channel, checked } => {
                            self.seen.plugins = plugins;
                            self.seen.catalog = catalog;
                            self.seen.channel = channel;
                            self.seen.checked = checked;
                        }
                        Update::Consents { consents } => {
                            if consents.len() > self.seen.consents.len() {
                                self.seen.sheets += 1;
                            }
                            self.seen.consents = consents;
                        }
                        Update::RequestDone { request, message } => {
                            println!("     {request}: {message}");
                            self.seen.done.push((request, Ok(message)));
                        }
                        Update::RequestFailed { request, message } => {
                            println!("     {request} failed: {message}");
                            self.seen.done.push((request, Err(message)));
                        }
                        _ => {}
                    }
                }
                if let Some((request, Err(message))) = self.seen.done.last() {
                    anyhow::bail!("{step}: {request} failed: {message}");
                }
                if done(&self.seen) {
                    println!("ok   {step}");
                    return Ok(());
                }
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    anyhow::bail!("timed out waiting for: {step}");
                };
                self.signal.wait(left.min(Duration::from_millis(500)));
            }
        }

        fn sheet(&mut self, step: &str, request: &str) -> anyhow::Result<ConsentRequest> {
            self.pump(step, |seen| seen.sheet(request).is_some())?;
            self.seen.sheet(request).cloned().context("sheet")
        }

        fn answer(&mut self, step: &str, request: &str, sensitive: &[&str]) -> anyhow::Result<()> {
            let sensitive = sensitive.iter().map(|name| (*name).to_string()).collect();
            self.studio.answer_consent(request.to_string(), ConsentAnswer::Install { sensitive });
            self.pump(step, |seen| seen.outcome(request).is_some())
        }
    }

    fn git(repo: &Path, args: &[&str]) -> anyhow::Result<()> {
        let status = Command::new("git")
            .args(["-c", "user.name=Plugin Smoke", "-c", "user.email=smoke@localhost", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(repo)
            .status()?;
        ensure!(status.success(), "git {} failed", args.join(" "));
        Ok(())
    }

    fn release(repo: &Path, version: &str, capabilities: &[&str]) -> anyhow::Result<()> {
        let listed: Vec<String> = capabilities.iter().map(|name| format!("{name:?}")).collect();
        let gates: Vec<String> = capabilities
            .iter()
            .filter(|name| plugin_store::Capability::parse(name).is_some_and(plugin_store::Capability::sensitive))
            .map(|name| format!("{name} = \"{name} for the fixture\"\n"))
            .collect();
        let gates = gates.join("");
        std::fs::write(
            repo.join("plugin.toml"),
            format!(
                "id = \"{ID}\"\nversion = \"{version}\"\ndescription = \"Fixture plugin for the hub smoke\"\ncapabilities = [{}]\n\n[mcp]\ncommand = \"bin/server.sh\"\nargs = [\"serve\", \"--granted={}\"]\n\n[gates]\n{gates}",
                listed.join(", "),
                plugin_store::GRANTED
            ),
        )?;
        git(repo, &["add", "-A"])?;
        git(repo, &["commit", "-q", "-m", version])
    }

    fn args(agent: &Path) -> anyhow::Result<Option<Vec<String>>> {
        let settings: Value = serde_json::from_slice(&std::fs::read(agent.join("settings.json"))?)?;
        Ok(settings
            .pointer(&format!("/mcpServers/{}{ID}/args", plugin_store::MCP_PREFIX))
            .and_then(Value::as_array)
            .map(|args| args.iter().filter_map(Value::as_str).map(str::to_string).collect()))
    }

    pub fn run(root: &Path) -> anyhow::Result<()> {
        ensure!(!root.exists(), "{} exists; give a fresh scratch dir", root.display());
        let (data, agent, repo) = (root.join("data"), root.join("agent"), root.join(ID));
        for dir in [&data, &agent, &repo] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(agent.join("settings.json"), "{}\n")?;
        std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
        std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
        git(&repo, &["init", "-q", "-b", "main"])?;
        git(&repo, &["config", "uploadpack.allowFilter", "true"])?;
        git(&repo, &["config", "uploadpack.allowAnySHA1InWant", "true"])?;
        std::fs::create_dir_all(repo.join("bin"))?;
        std::fs::write(repo.join("bin").join("server.sh"), "#!/bin/sh\nexec cat\n")?;
        Command::new("chmod").arg("755").arg(repo.join("bin").join("server.sh")).status()?;
        release(&repo, "1.0.0", &["mcp-server", "screen-capture"])?;
        let url = format!("file://{}", repo.canonicalize()?.display());
        let config = StudioConfig {
            socket_path: Some(root.join("absent.sock").to_string_lossy().into_owned()),
            session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
            data_dir: Some(data.to_string_lossy().into_owned()),
            controller_main: None,
            take_over_serve: false,
            launch: None,
        };
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut hub = Hub { studio, signal, seen: Seen::default() };
        let outcome = drive(&mut hub, &data.join("plugins"), &agent, &repo, &url);
        hub.studio.shutdown();
        outcome?;
        println!("PASS plugin smoke: hub");
        Ok(())
    }

    fn catalog_pin(hub: &mut Hub, plugins: &Path) -> anyhow::Result<()> {
        let catalog = plugin_store::Catalog::bundled().map_err(anyhow::Error::msg)?;
        let pin = catalog.entry(CATALOG).context("catalog entry")?.rev.clone();
        let path = plugins.join(plugin_store::ORIGINS);
        let recorded = std::fs::read_to_string(&path)?;
        ensure!(recorded.contains(&pin), "origins.json does not hold the catalog pin {pin}");
        std::fs::write(&path, recorded.replace(&pin, &"0".repeat(40)))?;
        let sheets = hub.seen.sheets;
        hub.studio.check_plugin_updates("hub-check-catalog".into());
        hub.pump("a catalog plugin behind the catalog's pin is offered the pin", |seen| {
            seen.outcome("hub-check-catalog").is_some() && seen.row(CATALOG).is_some_and(|row| row.update.is_some())
        })?;
        let update = hub.seen.row(CATALOG).and_then(|row| row.update.clone()).context("catalog update")?;
        ensure!(pin.starts_with(&update.revision) && update.asks.is_empty(), "catalog update: {update:?}");
        hub.studio.update_plugin("hub-update-catalog".into(), CATALOG.into());
        hub.pump("the catalog update reinstalls through the pinned catalog entry", |seen| {
            seen.outcome("hub-update-catalog").is_some()
                && seen.row(CATALOG).is_some_and(|row| row.update.is_none() && row.pinned)
        })?;
        ensure!(
            matches!(hub.seen.outcome("hub-update-catalog"), Some(Ok(_))),
            "catalog update: {:?}",
            hub.seen.outcome("hub-update-catalog")
        );
        ensure!(hub.seen.sheets == sheets, "a catalog update with nothing new opened a sheet");
        ensure!(std::fs::read_to_string(&path)?.contains(&pin), "origins.json did not record the catalog pin again");
        Ok(())
    }

    fn whole_catalog(hub: &mut Hub, plugins: &Path) -> anyhow::Result<()> {
        let entries: Vec<CatalogPlugin> =
            hub.seen.catalog.iter().filter(|entry| entry.id != CATALOG).cloned().collect();
        ensure!(!entries.is_empty(), "the bundled catalog lists only {CATALOG}");
        for entry in entries {
            let id = entry.id.clone();
            let request = format!("hub-catalog-{id}");
            let listed: Vec<String> = entry.grants.iter().map(|grant| grant.id.clone()).collect();
            hub.studio.install_plugin(request.clone(), id.clone());
            hub.pump(&format!("{id} installs from its pinned catalog tree"), |seen| {
                seen.outcome(&request).is_some() || seen.sheet(&request).is_some()
            })?;
            if let Some(sheet) = hub.seen.sheet(&request).cloned() {
                let ids: Vec<String> = sheet.rows.iter().map(|row| row.id.clone()).collect();
                ensure!(ids == listed, "{id} sheet asks for {ids:?}, the catalog lists {listed:?}");
                hub.answer(&format!("{id} installs with what the catalog lists"), &request, &[])?;
            } else {
                ensure!(listed.is_empty(), "{id} lists {listed:?} in the catalog but opened no sheet");
            }
            ensure!(
                matches!(hub.seen.outcome(&request), Some(Ok(_))),
                "{id} catalog install: {:?}",
                hub.seen.outcome(&request)
            );
            let row = hub.seen.row(&id).context("catalog row")?.clone();
            let granted: Vec<String> = row.grants.iter().map(|grant| grant.id.clone()).collect();
            ensure!(
                row.pinned && row.state != PluginState::Failed && granted == listed,
                "{id} does not match its catalog entry: {row:?}"
            );
            let remove = format!("hub-remove-{id}");
            hub.studio.remove_plugin(remove.clone(), id.clone());
            hub.pump(&format!("remove {id}"), |seen| seen.outcome(&remove).is_some() && seen.row(&id).is_none())?;
            ensure!(!plugins.join(&id).exists(), "{id} folder is still there");
        }
        Ok(())
    }

    fn untold(hub: &mut Hub, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir.join("bin"))?;
        std::fs::write(dir.join("bin").join("server.sh"), "#!/bin/sh\nexec cat\n")?;
        Command::new("chmod").arg("755").arg(dir.join("bin").join("server.sh")).status()?;
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"{PLAIN}\"\nversion = \"1.0.0\"\ndescription = \"Fixture plugin that is not told\"\ncapabilities = [\"mcp-server\", \"screen-capture\"]\n\n[mcp]\ncommand = \"bin/server.sh\"\nargs = [\"serve\"]\n"
            ),
        )?;
        hub.studio.install_plugin("hub-plain".into(), dir.to_string_lossy().into_owned());
        hub.sheet("a folder install asks for consent", "hub-plain")?;
        hub.answer("install it with the sensitive switch off", "hub-plain", &[])?;
        hub.pump("a plugin that is not told waits for its sensitive permission", |seen| {
            seen.row(PLAIN).is_some_and(|row| row.state == PluginState::Waiting && !row.told)
        })?;
        hub.studio.grant_plugin("hub-plain-allow".into(), PLAIN.into(), Vec::new());
        let sheet = hub.sheet("Allow opens a sheet with what it still needs", "hub-plain-allow")?;
        let ids: Vec<&str> = sheet.rows.iter().map(|row| row.id.as_str()).collect();
        ensure!(ids == ["screen-capture"] && sheet.preset.is_empty(), "allow sheet: {sheet:?}");
        hub.answer("allow screen capture", "hub-plain-allow", &["screen-capture"])?;
        hub.pump("then it runs", |seen| seen.row(PLAIN).is_some_and(|row| row.state == PluginState::Running))
    }

    fn updates(hub: &mut Hub, repo: &Path) -> anyhow::Result<()> {
        release(repo, "1.1.0", &["mcp-server", "screen-capture", "synthetic-input"])?;
        hub.studio.check_plugin_updates("hub-check".into());
        hub.pump("the update check finds 1.1.0 on the channel", |seen| {
            seen.outcome("hub-check").is_some() && seen.row(ID).is_some_and(|row| row.update.is_some())
        })?;
        let update = hub.seen.row(ID).and_then(|row| row.update.clone()).context("update")?;
        ensure!(update.version == "1.1.0" && update.asks == ["synthetic-input"], "update: {update:?}");
        ensure!(hub.seen.checked.is_some(), "no check time");
        ensure!(
            hub.seen.row(CATALOG).is_some_and(|row| row.update.is_none()),
            "the pinned catalog plugin shows an update"
        );

        hub.studio.update_plugin("hub-update".into(), ID.into());
        let sheet = hub.sheet("an update that asks for more asks consent again", "hub-update")?;
        let ids: Vec<&str> = sheet.rows.iter().map(|row| row.id.as_str()).collect();
        ensure!(ids == ["synthetic-input"] && sheet.update_from.as_deref() == Some("1.0.0"), "update sheet: {sheet:?}");
        hub.answer("update with the new switch off", "hub-update", &[])?;
        hub.pump("the plugin runs 1.1.0 and the update badge clears", |seen| {
            seen.row(ID)
                .is_some_and(|row| row.version == "1.1.0" && row.update.is_none() && row.state == PluginState::Running)
        })?;

        release(repo, "1.2.0", &["mcp-server", "screen-capture", "synthetic-input"])?;
        let sheets = hub.seen.sheets;
        hub.studio.check_plugin_updates("hub-check-2".into());
        hub.pump("the next check finds 1.2.0", |seen| {
            seen.outcome("hub-check-2").is_some() && seen.row(ID).is_some_and(|row| row.update.is_some())
        })?;
        hub.studio.update_plugin("hub-update-2".into(), ID.into());
        hub.pump("an update that asks for nothing new installs without a sheet", |seen| {
            seen.outcome("hub-update-2").is_some() && seen.row(ID).is_some_and(|row| row.version == "1.2.0")
        })?;
        ensure!(hub.seen.sheets == sheets, "an update with the same capabilities opened a sheet");
        Ok(())
    }

    fn drive(hub: &mut Hub, plugins: &Path, agent: &Path, repo: &Path, url: &str) -> anyhow::Result<()> {
        hub.studio.load_plugins();
        hub.pump("settings lists the bundled catalog", |seen| seen.catalog.iter().any(|entry| entry.id == CATALOG))?;
        let entry = hub.seen.catalog.iter().find(|entry| entry.id == CATALOG).context("catalog")?.clone();
        ensure!(entry.installed.is_none() && !entry.description.is_empty(), "catalog row: {entry:?}");
        ensure!(hub.seen.channel == local_studio_core::CHANNEL, "channel {}", hub.seen.channel);

        hub.studio.install_plugin("hub-catalog".into(), CATALOG.into());
        hub.pump("catalog install in one click", |seen| {
            seen.outcome("hub-catalog").is_some()
                && seen.row(CATALOG).is_some_and(|row| row.state == PluginState::Running)
        })?;
        let row = hub.seen.row(CATALOG).context("row")?.clone();
        ensure!(row.pinned && row.source == "Local Studio catalog", "catalog plugin row: {row:?}");
        ensure!(!row.commands.is_empty() && row.revision.is_some(), "catalog plugin row: {row:?}");
        ensure!(
            hub.seen
                .catalog
                .iter()
                .any(|entry| entry.id == CATALOG && entry.installed.as_deref() == Some(&row.version)),
            "the catalog does not mark {CATALOG} installed"
        );
        ensure!(hub.seen.sheets == 0, "a plugin that asks for nothing opened a sheet");

        hub.studio.install_plugin("hub-git".into(), url.to_string());
        let sheet = hub.sheet("install from a git URL asks for consent", "hub-git")?;
        ensure!(sheet.unpinned && sheet.told && sheet.install, "git sheet flags: {sheet:?}");
        ensure!(
            sheet.rows.len() == 2 && sheet.pin.as_deref().is_some_and(|pin| pin.len() == 7),
            "git sheet: {sheet:?}"
        );
        hub.answer("install with the sensitive switch off", "hub-git", &[])?;
        hub.pump("a told plugin runs with its sensitive capability off", |seen| {
            seen.row(ID).is_some_and(|row| {
                row.state == PluginState::Running
                    && row.told
                    && !row.pinned
                    && row.grants.iter().any(|grant| grant.id == "screen-capture" && !grant.granted)
            })
        })?;
        ensure!(
            args(agent)?.as_deref() == Some(&["serve".to_string(), "--granted=".to_string()][..]),
            "args {:?}",
            args(agent)?
        );
        println!("ok   its server starts with --granted= (nothing sensitive)");

        hub.studio.grant_plugin("hub-grant".into(), ID.into(), vec!["screen-capture".into()]);
        let sheet = hub.sheet("a Settings switch opens the consent sheet", "hub-grant")?;
        ensure!(sheet.preset == ["screen-capture"] && !sheet.install, "grant sheet: {sheet:?}");
        hub.answer("allow screen capture", "hub-grant", &["screen-capture"])?;
        hub.pump("the grant shows in Settings", |seen| {
            seen.row(ID).is_some_and(|row| row.grants.iter().any(|grant| grant.id == "screen-capture" && grant.granted))
        })?;
        ensure!(args(agent)?.is_some_and(|args| args[1] == "--granted=screen-capture"), "args {:?}", args(agent)?);
        println!("ok   its server now starts with --granted=screen-capture");

        hub.studio.revoke_plugin("hub-revoke".into(), ID.into(), "screen-capture".into());
        hub.pump("turning the switch off revokes at once", |seen| {
            seen.outcome("hub-revoke").is_some()
                && seen
                    .row(ID)
                    .is_some_and(|row| row.grants.iter().any(|grant| grant.id == "screen-capture" && !grant.granted))
        })?;
        ensure!(args(agent)?.is_some_and(|args| args[1] == "--granted="), "args {:?}", args(agent)?);

        updates(hub, repo)?;
        catalog_pin(hub, plugins)?;
        let plain = repo.parent().context("scratch root")?.join(PLAIN);
        untold(hub, &plain)?;
        whole_catalog(hub, plugins)?;

        for (request, id) in [("hub-remove", ID), ("hub-remove-catalog", CATALOG), ("hub-remove-plain", PLAIN)] {
            hub.studio.remove_plugin(request.into(), id.into());
            hub.pump(&format!("remove {id}"), |seen| seen.outcome(request).is_some() && seen.row(id).is_none())?;
            ensure!(!plugins.join(id).exists(), "{id} folder is still there");
        }
        ensure!(args(agent)?.is_none(), "the removed plugin's server is still in settings.json");
        let grants = std::fs::read_to_string(plugins.join("grants.json")).unwrap_or_default();
        let origins = std::fs::read_to_string(plugins.join(plugin_store::ORIGINS)).unwrap_or_default();
        ensure!(
            !grants.contains(ID) && !origins.contains(ID) && !origins.contains(CATALOG),
            "grants or origins remember a removed plugin"
        );
        ensure!(
            hub.seen.catalog.iter().all(|entry| entry.installed.is_none()),
            "the catalog still marks a removed plugin installed"
        );
        Ok(())
    }
}
