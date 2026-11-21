use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Approval, ApprovalAnswer, Block, Connection, DaemonLaunch, Delivery, Harness, Item, ItemKind,
    ModelOption, NoticeLevel, PromptDraft, Studio, StudioConfig, ThreadRow, ThreadSettings, ThreadStatus, TranscriptOp,
    Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::Map;

const STEP_TIMEOUT: Duration = Duration::from_secs(240);
const CHECK_TIMEOUT: Duration = Duration::from_secs(45);
const WORD: &str = "zeppelin";
const QUEUED: &str = "marmalade";
const PROOF: &str = "echo ls-proof-4711 > proof.txt";
const STOPPED: &str = "echo ls-stop-4711 > stopped.txt";
const LONG: &str = "Write a 3000 word short story about a lighthouse keeper on a northern island, in plain prose.";

pub struct Target {
    pub name: &'static str,
    pub harness: Harness,
    pub provider: &'static str,
    pub model: fn(&[ModelOption]) -> Option<String>,
    pub thinking: Option<&'static str>,
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
struct Watch {
    connection: Option<Connection>,
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    models: Vec<ModelOption>,
    thread: Option<String>,
    busy: bool,
    finished: u32,
    approvals: Vec<Approval>,
    answers: BTreeMap<u64, String>,
    users: BTreeMap<u64, String>,
    latest: Option<String>,
    streamed: bool,
    failure: Option<String>,
    resets: u32,
    errors: Vec<String>,
    usage: Option<UsageReport>,
    resources: u32,
    misses: Vec<String>,
}

impl Watch {
    fn row(&self) -> Option<&ThreadRow> {
        let thread = self.thread.as_deref()?;
        self.threads.iter().find(|row| row.id == thread)
    }

    fn begin(&mut self) -> u32 {
        self.latest = None;
        self.streamed = false;
        self.failure = None;
        self.finished
    }

    fn miss(&mut self, check: String) {
        println!("MISS {check}");
        self.misses.push(check);
    }

    fn reply(&self, step: &str) -> anyhow::Result<String> {
        if let Some(failure) = &self.failure {
            bail!("{step}: the reply ended with an error: {failure}");
        }
        let reply = self.latest.clone().unwrap_or_default();
        println!("     {step} reply: {}", reply.trim());
        Ok(reply)
    }
}

fn observe(watch: &mut Watch, item: &Item, live: bool) {
    match &item.kind {
        ItemKind::Assistant { blocks, streaming, error, .. } => {
            let text: String = blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(text.as_str()),
                    Block::Thinking { .. } => None,
                })
                .collect();
            let said = !text.trim().is_empty();
            if *streaming {
                watch.streamed |= live && said;
                return;
            }
            if let (true, Some(error)) = (live, error) {
                println!("     assistant error: {error}");
                watch.failure = Some(error.clone());
            }
            if said {
                watch.answers.insert(item.id, text.clone());
                if live {
                    watch.latest = Some(text);
                }
            }
        }
        ItemKind::User { text, .. } => {
            watch.users.insert(item.id, text.clone());
        }
        ItemKind::Tool { .. }
        | ItemKind::TurnSummary { .. }
        | ItemKind::Notice { .. }
        | ItemKind::Compaction { .. } => {}
    }
}

fn apply(watch: &mut Watch, update: Update) {
    match update {
        Update::Transcript { thread, op } if watch.thread.as_deref() == Some(thread.as_str()) => match op {
            TranscriptOp::Reset { items } => {
                watch.resets += 1;
                watch.answers.clear();
                watch.users.clear();
                for item in &items {
                    observe(watch, item, false);
                }
            }
            TranscriptOp::Insert { item, .. } | TranscriptOp::Update { item, .. } => observe(watch, &item, true),
        },
        Update::Thread { thread, info } if watch.thread.as_deref() == Some(thread.as_str()) => {
            if watch.busy && !info.busy {
                watch.finished += 1;
            }
            watch.busy = info.busy;
        }
        Update::Approvals { thread, approvals } if watch.thread.as_deref() == Some(thread.as_str()) => {
            watch.approvals = approvals;
        }
        Update::Usage { report, .. } => watch.usage = Some(report),
        Update::Commands { .. } | Update::Skills { .. } => watch.resources += 1,
        Update::Connection { state } => watch.connection = Some(state),
        Update::Projects { projects } => watch.projects = projects.into_iter().map(|project| project.id).collect(),
        Update::Threads { threads } => watch.threads = threads,
        Update::Models { models, .. } => watch.models = models,
        Update::ThreadStarted { thread, .. } => watch.thread = Some(thread),
        Update::Notice { level, text } => {
            println!("     notice {level:?}: {text}");
            if level == NoticeLevel::Error {
                watch.errors.push(text);
            }
        }
        Update::RequestFailed { message, .. } => {
            println!("     request failed: {message}");
            watch.errors.push(message);
        }
        Update::Transcript { .. }
        | Update::Thread { .. }
        | Update::Approvals { .. }
        | Update::ArchivedThreads { .. }
        | Update::ThreadSearch { .. }
        | Update::Queue { .. }
        | Update::ComposerText { .. }
        | Update::DaemonDefault { .. }
        | Update::Subagents { .. }
        | Update::MessageImage { .. }
        | Update::Notification { .. }
        | Update::GitStatus { .. }
        | Update::Branches { .. }
        | Update::Diff { .. }
        | Update::Files { .. }
        | Update::FileContent { .. }
        | Update::SearchResults { .. }
        | Update::PullRequests { .. }
        | Update::PullRequestDetail { .. }
        | Update::Devices { .. }
        | Update::TerminalOpened { .. }
        | Update::TerminalFrame { .. }
        | Update::TerminalText { .. }
        | Update::TerminalExited { .. }
        | Update::Local { .. }
        | Update::LocalDashboard { .. }
        | Update::Providers { .. }
        | Update::ProviderModels { .. }
        | Update::Settings { .. }
        | Update::Keybindings { .. }
        | Update::ProjectScripts { .. }
        | Update::ProjectSettings { .. }
        | Update::WorktreeSetup { .. }
        | Update::Daemon { .. }
        | Update::Kernel { .. }
        | Update::RequestDone { .. } => {}
    }
}

fn pump(
    studio: &Studio,
    signal: &Signal,
    watch: &mut Watch,
    step: &str,
    done: impl Fn(&Watch) -> bool,
) -> anyhow::Result<()> {
    pump_within(studio, signal, watch, step, STEP_TIMEOUT, done)
}

fn pump_within(
    studio: &Studio,
    signal: &Signal,
    watch: &mut Watch,
    step: &str,
    timeout: Duration,
    done: impl Fn(&Watch) -> bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let errors = watch.errors.len();
    loop {
        for update in studio.tick() {
            apply(watch, update);
        }
        if done(watch) {
            println!("ok   {step}");
            return Ok(());
        }
        if let Some(error) = watch.errors.get(errors) {
            bail!("{step}: {error}");
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            bail!("timed out waiting for: {step}");
        };
        signal.wait(left.min(Duration::from_millis(100)));
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn turn(
    studio: &Studio,
    signal: &Signal,
    watch: &mut Watch,
    thread: &str,
    text: &str,
    step: &str,
) -> anyhow::Result<String> {
    let finished = watch.begin();
    studio.send(format!("send-{finished}"), thread.to_string(), prompt(text), Delivery::Queue);
    pump(studio, signal, watch, &format!("{step} finished"), |watch| watch.finished > finished)?;
    watch.reply(step)
}

fn write_prompt(file: &str) -> String {
    format!(
        "Create a file named {file} in the current directory containing the single line hello. Use a file writing tool, not a shell command, and do not ask me first."
    )
}

fn shell_prompt(command: &str) -> String {
    format!("Run exactly this shell command with your shell tool, unchanged, and do not ask me first: {command}")
}

fn approval_turn(
    studio: &Studio,
    signal: &Signal,
    watch: &mut Watch,
    thread: &str,
    (label, text): (&str, &str),
    answer: ApprovalAnswer,
) -> anyhow::Result<Vec<Approval>> {
    let finished = watch.finished;
    studio.send(format!("send-{label}"), thread.to_string(), prompt(text), Delivery::Queue);
    let mut asked = Vec::new();
    loop {
        pump(studio, signal, watch, &format!("{label}: approval asked or turn ended"), |watch| {
            !watch.approvals.is_empty() || watch.finished > finished
        })?;
        let Some(approval) = watch.approvals.first().cloned() else { break };
        println!("     approval: {} {:?}", approval.command, approval.effects);
        studio.answer_approval(thread.to_string(), approval.request_id.clone(), answer);
        let request = approval.request_id.clone();
        asked.push(approval);
        pump(studio, signal, watch, &format!("{label}: approval {} settled", asked.len()), |watch| {
            watch.approvals.iter().all(|known| known.request_id != request)
        })?;
    }
    Ok(asked)
}

fn card_shows_command(
    studio: &Studio,
    signal: &Signal,
    watch: &mut Watch,
    thread: &str,
    work: &Path,
) -> anyhow::Result<()> {
    let asked = approval_turn(studio, signal, watch, thread, ("proof", &shell_prompt(PROOF)), ApprovalAnswer::Deny)?;
    ensure!(!asked.is_empty(), "Supervised did not ask before running a shell command");
    ensure!(!work.join("proof.txt").exists(), "proof.txt was written after a deny");
    let probe = PROOF.split(" > ").next().unwrap_or(PROOF);
    match asked.iter().find(|approval| approval.command.contains("ls-proof-4711")) {
        Some(card) if card.command.contains(probe) => {
            println!("ok   the shell approval card shows the command: {}", card.command.replace('\n', " | "));
        }
        Some(card) => watch.miss(format!("the shell approval card hides the command `{probe}`: {:?}", card.command)),
        None => watch.miss(format!(
            "no shell approval card shows the command `{probe}`: {:?}",
            asked.iter().map(|approval| approval.command.clone()).collect::<Vec<_>>()
        )),
    }
    Ok(())
}

fn stop_settles_approval(studio: &Studio, signal: &Signal, watch: &mut Watch, thread: &str) -> anyhow::Result<()> {
    let finished = watch.finished;
    studio.send("send-stop-approval".to_string(), thread.to_string(), prompt(&shell_prompt(STOPPED)), Delivery::Queue);
    pump(studio, signal, watch, "stop-approval: approval asked", |watch| {
        !watch.approvals.is_empty() || watch.finished > finished
    })?;
    ensure!(!watch.approvals.is_empty(), "Supervised did not ask before running {STOPPED}");
    println!("     approval left unanswered: {}", watch.approvals[0].command);
    studio.abort(thread.to_string());
    let stopped = Instant::now();
    let settled = pump_within(studio, signal, watch, "Stop settled the unanswered approval", CHECK_TIMEOUT, |watch| {
        watch.approvals.is_empty() && !watch.busy && watch.row().is_some_and(|row| row.status != ThreadStatus::Working)
    });
    if let Err(error) = settled {
        watch.miss(format!("{error} (approvals {}, busy {})", watch.approvals.len(), watch.busy));
        for approval in watch.approvals.clone() {
            studio.answer_approval(thread.to_string(), approval.request_id, ApprovalAnswer::Deny);
        }
        pump(studio, signal, watch, "stale approvals cleared after the miss", |watch| watch.approvals.is_empty())?;
        if watch.busy {
            studio.abort(thread.to_string());
            pump(studio, signal, watch, "the thread left Working after the miss", |watch| !watch.busy)?;
        }
    } else {
        println!("     settled in {} ms", stopped.elapsed().as_millis());
    }
    Ok(())
}

fn slash_command_settles(studio: &Studio, signal: &Signal, watch: &mut Watch, thread: &str) -> anyhow::Result<()> {
    for command in ["/cost", "/context"] {
        let finished = watch.begin();
        studio.send(format!("send-{command}"), thread.to_string(), prompt(command), Delivery::Queue);
        let sent = Instant::now();
        let left = pump_within(studio, signal, watch, &format!("{command} left Working"), CHECK_TIMEOUT, |watch| {
            watch.finished > finished && watch.row().is_some_and(|row| row.status != ThreadStatus::Working)
        });
        if let Err(error) = left {
            watch.miss(format!("{error} (busy {}, row {:?})", watch.busy, watch.row().map(|row| row.status)));
            if watch.busy {
                studio.abort(thread.to_string());
                pump(studio, signal, watch, &format!("{command}: Stop took the thread out of Working"), |watch| {
                    !watch.busy
                })?;
            }
        } else {
            println!("     {command} settled in {} ms", sent.elapsed().as_millis());
        }
    }
    Ok(())
}

fn queued_follow_up(studio: &Studio, signal: &Signal, watch: &mut Watch, thread: &str) -> anyhow::Result<()> {
    watch.begin();
    let first = "Count from one to forty in words, one number per line, and nothing else.";
    studio.send("send-count".to_string(), thread.to_string(), prompt(first), Delivery::Queue);
    pump(studio, signal, watch, "count turn is working", |watch| watch.busy)?;
    let follow = format!("Reply with only the word {QUEUED}.");
    studio.send("send-queued".to_string(), thread.to_string(), prompt(&follow), Delivery::Queue);
    let ran = pump(studio, signal, watch, "queued follow-up ran after the turn", |watch| {
        !watch.busy && watch.latest.as_deref().is_some_and(|text| text.to_ascii_lowercase().contains(QUEUED))
    });
    match ran {
        Ok(()) => {
            let users = watch.users.values().filter(|text| text.contains(QUEUED)).count();
            if users == 1 {
                println!("ok   the queued follow-up ran once after the running turn");
            } else {
                watch.miss(format!("the queued follow-up shows {users} times in the transcript"));
            }
        }
        Err(error) => {
            watch.miss(format!("{error} (busy {}, latest {:?})", watch.busy, watch.latest));
            if watch.busy {
                studio.abort(thread.to_string());
                pump(studio, signal, watch, "the thread left Working after the miss", |watch| !watch.busy)?;
            }
        }
    }
    Ok(())
}

fn efforts_running() -> Vec<String> {
    let parent = std::process::id().to_string();
    let Ok(listing) = Command::new("ps").args(["-A", "-o", "ppid=,command="]).output() else { return Vec::new() };
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let (ppid, command) = line.trim().split_once(' ')?;
            (ppid == parent && command.contains("stream-json")).then(|| {
                command
                    .split_once("--effort ")
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .unwrap_or("none")
                    .to_string()
            })
        })
        .collect()
}

fn effort_restarts(studio: &Studio, signal: &Signal, watch: &mut Watch, old: &str) {
    let deadline = Instant::now() + CHECK_TIMEOUT;
    loop {
        for update in studio.tick() {
            apply(watch, update);
        }
        let running = efforts_running();
        if !running.iter().any(|effort| effort == old) {
            println!("ok   the effort change restarted the process after the turn (running: {running:?})");
            return;
        }
        if Instant::now() >= deadline {
            watch.miss(format!(
                "the Claude Code process still runs at --effort {old} {} s after the turn that changed it (running: {running:?})",
                CHECK_TIMEOUT.as_secs()
            ));
            return;
        }
        signal.wait(Duration::from_millis(500));
    }
}

fn git(work: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git").current_dir(work).args(args).status()?;
    ensure!(status.success(), "git {args:?} failed");
    Ok(())
}

fn scratch_repo(work: &Path) -> anyhow::Result<()> {
    if work.join(".git").is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(work)?;
    git(work, &["init", "-q", "-b", "main"])?;
    std::fs::write(work.join("README.md"), "Scratch repo for the harness smoke.\n")?;
    git(work, &["add", "README.md"])?;
    git(work, &["-c", "user.name=smoke", "-c", "user.email=smoke@localhost", "commit", "-q", "-m", "init"])
}

fn open(config: &StudioConfig) -> anyhow::Result<(Arc<Studio>, Arc<Signal>, Watch)> {
    let signal = Arc::new(Signal::default());
    let studio = Studio::new(config.clone(), Arc::clone(&signal) as Arc<dyn Waker>)?;
    let mut watch = Watch::default();
    pump(&studio, &signal, &mut watch, "daemon connected", |watch| {
        matches!(watch.connection, Some(Connection::Ready))
    })?;
    Ok((studio, signal, watch))
}

fn first_run(config: &StudioConfig, target: &Target, work: &Path) -> anyhow::Result<(String, Vec<String>)> {
    let (studio, signal, mut watch) = open(config)?;
    let outcome = (|| {
        let watch = &mut watch;
        pump(&studio, &signal, watch, "models listed", |watch| {
            watch.models.iter().any(|model| model.provider == target.provider)
        })?;
        let models: Vec<ModelOption> =
            watch.models.iter().filter(|model| model.provider == target.provider).cloned().collect();
        let model = (target.model)(&models).context("no model to run")?;
        println!(
            "     {} models: {}",
            target.name,
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>().join(", ")
        );
        println!("     running {}/{model}", target.provider);
        studio.add_project(work.to_string_lossy().into_owned());
        pump(&studio, &signal, watch, "project added", |watch| watch.projects.len() == 1)?;
        let settings = ThreadSettings {
            provider: Some(target.provider.to_string()),
            model: Some(model),
            thinking_level: target.thinking.map(str::to_string),
            access: AccessMode::Supervised,
            worktree: WorktreeChoice::Local,
        };
        let first =
            format!("<div class=\"note\">Remember the word {WORD}. Reply with only that word, nothing else.</div>");
        let finished = watch.begin();
        let project = watch.projects.first().cloned();
        studio.start_thread("draft-harness".to_string(), project, prompt(&first), settings);
        pump(&studio, &signal, watch, "thread started", |watch| watch.thread.is_some())?;
        let thread = watch.thread.clone().context("thread id")?;
        studio.select_thread(Some(thread.clone()));
        pump(&studio, &signal, watch, "turn 1 finished", |watch| watch.finished > finished)?;
        let reply = watch.reply("turn 1")?;
        ensure!(reply.to_ascii_lowercase().contains(WORD), "turn 1 did not answer with {WORD}");
        println!("ok   reply contains the asked word");
        let row = watch.row().context("thread row")?;
        ensure!(row.harness == target.harness, "the row shows harness {:?}", row.harness);
        println!("ok   row lists harness {:?}", row.harness);
        studio.list_commands("harness-commands".to_string(), watch.projects.first().cloned());
        pump(&studio, &signal, watch, "commands listed with the CLI thread focused", |watch| watch.resources == 1)?;
        studio.list_skills("harness-skills".to_string(), watch.projects.first().cloned());
        pump(&studio, &signal, watch, "skills listed with the CLI thread focused", |watch| watch.resources == 2)?;

        let denied = approval_turn(
            &studio,
            &signal,
            watch,
            &thread,
            ("denied.txt", &write_prompt("denied.txt")),
            ApprovalAnswer::Deny,
        )?
        .len();
        ensure!(denied > 0, "Supervised did not ask before writing denied.txt");
        ensure!(!work.join("denied.txt").exists(), "denied.txt was written after a deny");
        println!("ok   deny left denied.txt unwritten ({denied} approvals denied)");
        let approved = approval_turn(
            &studio,
            &signal,
            watch,
            &thread,
            ("approved.txt", &write_prompt("approved.txt")),
            ApprovalAnswer::Approve,
        )?
        .len();
        ensure!(approved > 0, "Supervised did not ask before writing approved.txt");
        ensure!(work.join("approved.txt").is_file(), "approved.txt is missing after an approve");
        println!("ok   approve wrote approved.txt ({approved} approvals)");

        card_shows_command(&studio, &signal, watch, &thread, work)?;
        stop_settles_approval(&studio, &signal, watch, &thread)?;
        ensure!(!work.join("stopped.txt").exists(), "stopped.txt was written although its approval was never given");
        if target.harness == Harness::ClaudeCode {
            slash_command_settles(&studio, &signal, watch, &thread)?;
        }
        queued_follow_up(&studio, &signal, watch, &thread)?;

        let finished = watch.begin();
        studio.send("send-long".to_string(), thread.clone(), prompt(LONG), Delivery::Queue);
        pump(&studio, &signal, watch, "long turn is working", |watch| watch.busy)?;
        pump(&studio, &signal, watch, "long turn is streaming text", |watch| {
            watch.streamed || watch.finished > finished
        })?;
        ensure!(watch.finished == finished, "the long turn ended before the abort: {:?}", watch.failure);
        let old = target.thinking.unwrap_or("none");
        let new = if old == "medium" { "low" } else { "medium" };
        studio.set_thinking_level(thread.clone(), new.to_string());
        println!("     thinking level {old} -> {new} during the running turn");
        studio.abort(thread.clone());
        let aborted = Instant::now();
        pump(&studio, &signal, watch, "abort ended the turn", |watch| watch.finished > finished && !watch.busy)?;
        pump(&studio, &signal, watch, "abort took the row out of Working", |watch| {
            watch.row().is_some_and(|row| row.status != ThreadStatus::Working)
        })?;
        let status = watch.row().map(|row| row.status);
        println!("ok   aborted turn left Working in {} ms (row {status:?})", aborted.elapsed().as_millis());
        if target.harness == Harness::ClaudeCode {
            effort_restarts(&studio, &signal, watch, old);
        }
        Ok(thread)
    })();
    let misses = std::mem::take(&mut watch.misses);
    studio.shutdown();
    outcome.map(|thread| (thread, misses))
}

fn second_run(config: &StudioConfig, target: &Target, thread: &str) -> anyhow::Result<Vec<String>> {
    let (studio, signal, mut watch) = open(config)?;
    let outcome = (|| {
        let watch = &mut watch;
        watch.thread = Some(thread.to_string());
        pump(&studio, &signal, watch, "thread listed after the restart", |watch| watch.row().is_some())?;
        let row = watch.row().context("thread row")?;
        ensure!(row.harness == target.harness, "after the restart the row shows harness {:?}", row.harness);
        println!("ok   listed after restart: \"{}\" harness {:?}", row.title, row.harness);
        studio.select_thread(Some(thread.to_string()));
        pump(&studio, &signal, watch, "earlier turns shown", |watch| watch.users.len() >= 2)?;
        println!("ok   earlier turns shown ({} user messages, {} replies)", watch.users.len(), watch.answers.len());
        if watch.users.values().any(|text| text.starts_with("<div") && text.contains(WORD)) {
            println!("ok   the user message starting with <div survived the restart");
        } else {
            let first = watch.users.values().next().cloned().unwrap_or_default();
            watch.miss(format!(
                "the user message starting with <div is missing after the restart (first user message: {first:?})"
            ));
        }
        let reply = turn(
            &studio,
            &signal,
            watch,
            thread,
            "What word did I ask you to remember in my first message? Reply with only that word.",
            "resumed turn",
        )?;
        ensure!(reply.to_ascii_lowercase().contains(WORD), "the resumed session did not remember {WORD}");
        println!("ok   the session resumed: turn 1 word recalled");
        studio.load_usage("usage-1".to_string(), UsagePeriod::Day);
        pump(&studio, &signal, watch, "usage report loaded", |watch| watch.usage.is_some())?;
        let report = watch.usage.as_ref().context("usage")?;
        let rows: Vec<_> = report.by_model.iter().filter(|model| model.provider == target.provider).collect();
        for row in &rows {
            println!(
                "     usage {}/{}: {} tokens, {} requests, ${:.4}",
                row.provider, row.model, row.tokens, row.requests, row.cost_usd
            );
        }
        println!(
            "     usage total: {} tokens (in {} cache read {} out {}), ${:.4}, {} sessions",
            report.total_tokens,
            report.input_tokens,
            report.cache_read_tokens,
            report.output_tokens,
            report.cost_usd,
            report.sessions
        );
        ensure!(rows.iter().any(|row| row.tokens > 0), "the usage report has no {} tokens", target.provider);
        ensure!(report.output_tokens > 0, "the usage report counts no output tokens");
        println!("ok   usage report counts real tokens");
        Ok(())
    })();
    let misses = std::mem::take(&mut watch.misses);
    studio.shutdown();
    outcome.map(|()| misses)
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

pub fn run(target: &Target) -> anyhow::Result<()> {
    let usage = format!("usage: {} <eukhe> <scratch dir>", target.name);
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage.clone())?);
    let root = PathBuf::from(args.next().context(usage)?);
    let agent_dir = root.join("agent");
    let work = root.join("work");
    std::fs::create_dir_all(&agent_dir)?;
    scratch_repo(&work)?;
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent_dir);
    let socket = root.join("d.sock");
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent_dir.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        controller_main: None,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
    };
    let outcome = first_run(&config, target, &work).and_then(|(thread, mut misses)| {
        println!("ok   first Studio shut down; starting a second one on the same data dir");
        misses.extend(second_run(&config, target, &thread)?);
        Ok(misses)
    });
    let stopped = stop_daemon(&socket);
    let misses = outcome?;
    stopped?;
    if !misses.is_empty() {
        bail!("{} checks failed:\n- {}", misses.len(), misses.join("\n- "));
    }
    println!("PASS {}", target.name);
    Ok(())
}
