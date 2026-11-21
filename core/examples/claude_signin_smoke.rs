use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, ModelOption, PromptDraft, ProviderCredential, ProviderStatus, Studio,
    StudioConfig, ThreadSettings, Update, Waker, WorktreeChoice,
};
use serde_json::Map;

const STEP_TIMEOUT: Duration = Duration::from_secs(90);
const ANTHROPIC: &str = "anthropic";
const CLAUDE_CODE: &str = "claude-code";
const SIGNED_IN: &str = r#"{"anthropic":{"type":"oauth","refresh":"","access":"","expires":4102444800000}}"#;
const API_KEY: &str = r#"{"anthropic":{"type":"api_key","key":"sk-ant-local-studio-fake-not-real"}}"#;

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
    models: Vec<ModelOption>,
    providers: Option<Vec<ProviderStatus>>,
    errors: Vec<String>,
    failed: Vec<(String, String)>,
    started: Vec<String>,
}

impl View {
    fn eukhe_loaded(&self) -> bool {
        self.models.iter().any(|model| model.provider != CLAUDE_CODE && model.provider != "codex")
    }

    fn listed(&self, provider: &str) -> Vec<&ModelOption> {
        self.models.iter().filter(|model| model.provider == provider).collect()
    }

    fn anthropic(&self) -> Option<&ProviderStatus> {
        self.providers.as_ref()?.iter().find(|provider| provider.id == ANTHROPIC)
    }
}

struct Harness {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
}

impl Harness {
    fn pump_until(&mut self, step: &str, done: impl Fn(&View) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            for update in self.studio.tick() {
                self.apply(update);
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

    fn apply(&mut self, update: Update) {
        let view = &mut self.view;
        match update {
            Update::Connection { state } => view.connection = Some(state),
            Update::Models { models, .. } => view.models = models,
            Update::Providers { providers, errors, .. } => {
                view.providers = Some(providers);
                view.errors = errors;
            }
            Update::RequestFailed { request, message } => view.failed.push((request, message)),
            Update::ThreadStarted { draft, .. } => view.started.push(draft),
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            _ => {}
        }
    }
}

fn write_private(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?
        .write_all(content.as_bytes())?;
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

fn main() -> anyhow::Result<()> {
    let usage = "usage: claude_signin_smoke <eukhe> [scratch dir, default /tmp/ls-signin]";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let base = args.next().map_or_else(|| PathBuf::from("/tmp/ls-signin"), PathBuf::from);
    let root = base.join(format!("run-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(&root)?;
    std::env::set_var("LOCAL_STUDIO_CODEX_BIN", root.join("no-codex"));
    std::env::set_var("LOCAL_STUDIO_CLAUDE_BIN", root.join("no-claude"));
    run(&daemon, &root.join("oauth"), SIGNED_IN, signed_in)?;
    run(&daemon, &root.join("key"), API_KEY, api_key)?;
    println!("PASS claude signin smoke ({})", root.display());
    Ok(())
}

fn run(daemon: &Path, root: &Path, auth: &str, check: fn(&mut Harness) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let agent = root.join("agent");
    let home = root.join("home");
    std::fs::create_dir_all(&agent)?;
    std::fs::create_dir_all(&home)?;
    write_private(&agent.join("auth.json"), auth)?;
    write_private(&agent.join("models.json"), r#"{"providers":{}}"#)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    let socket = root.join("d.sock");
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
        controller_main: None,
    };
    let signal = Arc::new(Signal::default());
    let outcome =
        Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(anyhow::Error::from).and_then(|studio| {
            let mut harness = Harness { studio, signal, view: View::default() };
            let outcome = harness
                .pump_until("daemon launched and connected", |view| matches!(view.connection, Some(Connection::Ready)))
                .and_then(|()| {
                    harness.studio.load_models();
                    harness.studio.load_providers();
                    harness.pump_until("Eukhe's model catalog and the providers reached the GUI", |view| {
                        view.eukhe_loaded() && view.anthropic().is_some()
                    })
                })
                .and_then(|()| check(&mut harness));
            harness.studio.shutdown();
            outcome
        });
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped
}

fn signed_in(harness: &mut Harness) -> anyhow::Result<()> {
    println!("     auth.json marks anthropic as oauth with empty tokens");
    let view = &harness.view;
    let claude = view.listed(CLAUDE_CODE);
    ensure!(!claude.is_empty(), "the Claude Code provider is not listed");
    println!("ok   Claude Code stays listed: {:?}", claude.iter().map(|model| &model.id).collect::<Vec<_>>());
    let anthropic = view.listed(ANTHROPIC);
    ensure!(
        anthropic.is_empty(),
        "Eukhe offers {} anthropic models on a subscription sign-in: {:?}",
        anthropic.len(),
        anthropic.iter().map(|model| &model.id).collect::<Vec<_>>()
    );
    println!("ok   no Eukhe anthropic model is listed");
    let line = view
        .errors
        .iter()
        .find(|error| error.contains("Claude Code") && error.contains("claude CLI sign-in"))
        .context(format!("no line explains why and points to Claude Code: {:?}", view.errors))?;
    println!("ok   the providers list explains: {line}");
    let status = view.anthropic().context("anthropic status")?;
    ensure!(
        status.credential == ProviderCredential::SignedIn && !status.configured,
        "anthropic shows as ready: {status:?}"
    );
    println!("ok   the Anthropic row is not ready");
    harness.studio.start_thread(
        "anthropic-draft".to_string(),
        None,
        PromptDraft { text: "ping".to_string(), images: Vec::new(), files: Vec::new() },
        ThreadSettings {
            provider: Some(ANTHROPIC.to_string()),
            model: Some("claude-haiku-5-5".to_string()),
            thinking_level: None,
            access: AccessMode::Supervised,
            worktree: WorktreeChoice::Local,
        },
    );
    harness.pump_until("an Eukhe thread on anthropic is answered", |view| {
        view.failed.iter().any(|(id, _)| id == "anthropic-draft")
            || view.started.iter().any(|draft| draft == "anthropic-draft")
    })?;
    let refusal = harness
        .view
        .failed
        .iter()
        .find(|(id, _)| id == "anthropic-draft")
        .map(|(_, message)| message.clone())
        .context("Eukhe started a thread on the Claude subscription sign-in")?;
    ensure!(refusal.contains("Claude Code"), "refusal does not point to Claude Code: {refusal}");
    println!("ok   starting an Eukhe anthropic thread is refused: {refusal}");
    Ok(())
}

fn api_key(harness: &mut Harness) -> anyhow::Result<()> {
    println!("     auth.json holds a fake anthropic API key");
    let view = &harness.view;
    ensure!(!view.listed(CLAUDE_CODE).is_empty(), "the Claude Code provider is not listed");
    println!("ok   Claude Code stays listed");
    let anthropic = view.listed(ANTHROPIC);
    ensure!(
        !anthropic.is_empty() && anthropic.iter().all(|model| model.configured),
        "an anthropic API key does not list its models: {anthropic:?}"
    );
    println!("ok   {} anthropic models are listed on the API key", anthropic.len());
    ensure!(
        !view.errors.iter().any(|error| error.contains("Claude Code")),
        "an API key shows the subscription line: {:?}",
        view.errors
    );
    let status = view.anthropic().context("anthropic status")?;
    ensure!(status.credential == ProviderCredential::StoredKey && status.configured, "anthropic status {status:?}");
    println!("ok   the Anthropic row is ready with a saved key");
    Ok(())
}
