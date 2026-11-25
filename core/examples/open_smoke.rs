use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, Delivery, ItemKind, PromptDraft, Studio, StudioConfig, ThreadRow,
    ThreadSettings, ThreadStatus, TranscriptOp, Update, UsagePeriod, UsageReport, Waker, WorktreeChoice,
};
use serde_json::{json, Map, Value};

#[path = "support/browser_chat.rs"]
mod browser_chat;
#[allow(dead_code)]
#[path = "support/harness.rs"]
mod harness;
#[path = "support/real_model.rs"]
mod real_model;

use harness::Signal;

const STEP_TIMEOUT: Duration = Duration::from_mins(5);
const MESSAGES: usize = 10_000;
const RUNS: usize = 5;
const REST: Duration = Duration::from_secs(3);
const CHAT_LISTED: Duration = Duration::from_secs(15);
const AGAIN_MS: u128 = 150;
const DEBUG_AGAIN_MS: u128 = 5_000;
const CLAUDE: &str = "Claude Code";
const WORDS: &str =
    "the parser keeps each span, so the error names the line that the reader wrote and not the line the macro made";

#[derive(Default)]
struct Watch {
    connection: Option<Connection>,
    projects: Vec<String>,
    threads: Vec<ThreadRow>,
    started: HashMap<String, String>,
    shown: Option<(String, usize)>,
    resets: usize,
    replied: bool,
    usage: Option<UsageReport>,
    notices: Vec<String>,
}

impl Watch {
    fn items(&self, thread: &str) -> usize {
        self.shown.as_ref().filter(|(shown, _)| shown == thread).map_or(0, |(_, items)| *items)
    }

    fn settled(&self, thread: &str) -> bool {
        self.threads.iter().any(|row| row.id == thread && matches!(row.status, ThreadStatus::Done | ThreadStatus::Idle))
    }
}

struct Session {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    watch: Watch,
}

impl Session {
    fn open(config: StudioConfig) -> anyhow::Result<Self> {
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut session = Self { studio, signal, watch: Watch::default() };
        session.pump("daemon connected", |watch| matches!(watch.connection, Some(Connection::Ready)))?;
        Ok(session)
    }

    fn pump(&mut self, step: &str, done: impl Fn(&Watch) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            for update in self.studio.tick() {
                match update {
                    Update::Connection { state } => self.watch.connection = Some(state),
                    Update::Projects { projects } => {
                        self.watch.projects = projects.into_iter().map(|project| project.id).collect();
                    }
                    Update::Threads { threads } => self.watch.threads = threads,
                    Update::ThreadStarted { draft, thread } => {
                        self.watch.started.insert(draft, thread);
                    }
                    Update::Transcript { thread, op: TranscriptOp::Reset { items } } => {
                        self.watch.shown = Some((thread, items.len()));
                        self.watch.resets += 1;
                    }
                    Update::Transcript {
                        op: TranscriptOp::Insert { item, .. } | TranscriptOp::Update { item, .. },
                        ..
                    } => self.watch.replied |= matches!(item.kind, ItemKind::Assistant { .. }),
                    Update::Usage { report, .. } => self.watch.usage = Some(report),
                    Update::Notice { level, text } => {
                        println!("     notice {level:?}: {text}");
                        self.watch.notices.push(text);
                    }
                    _ => {}
                }
            }
            if done(&self.watch) {
                return Ok(());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                bail!("timed out waiting for: {step}");
            };
            self.signal.wait(left.min(Duration::from_millis(100)));
        }
    }

    fn start(&mut self, name: &str, settings: ThreadSettings) -> anyhow::Result<String> {
        self.studio.start_thread(name.to_string(), self.watch.projects.first().cloned(), prompt(), settings);
        self.pump(&format!("{name} thread started"), |watch| watch.started.contains_key(name))?;
        let thread = self.watch.started[name].clone();
        self.pump(&format!("{name} thread answered"), |watch| watch.settled(&thread))?;
        println!("ok   {name} thread answered");
        Ok(thread)
    }

    fn answer(&mut self, thread: &str) -> anyhow::Result<()> {
        self.watch.replied = false;
        self.studio.send("send".to_string(), thread.to_string(), prompt(), Delivery::Queue);
        self.pump("reply shown", |watch| watch.replied)?;
        self.pump("reply finished", |watch| watch.settled(thread))
    }

    fn rest(&mut self) -> anyhow::Result<()> {
        let rested = Instant::now() + REST;
        self.pump("rest", |_| Instant::now() >= rested)
    }

    fn show(&mut self, thread: &str, items: usize) -> anyhow::Result<Duration> {
        self.rest()?;
        (self.watch.shown, self.watch.resets) = (None, 0);
        let started = Instant::now();
        self.studio.select_thread(Some(thread.to_string()));
        self.pump("transcript shown", |watch| watch.items(thread) >= items)?;
        Ok(started.elapsed())
    }
}

fn prompt() -> PromptDraft {
    PromptDraft { text: "Reply with only the word ok.".to_string(), images: Vec::new(), files: Vec::new() }
}

fn jsonl(dir: &Path, name: Option<&str>) -> anyhow::Result<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|extension| extension == "jsonl")
                && name.is_none_or(|name| path.file_name().is_some_and(|file| file == name))
            {
                found.push(path);
            }
        }
    }
    ensure!(found.len() == 1, "one session file under {}, found {found:?}", dir.display());
    found.pop().context("session file")
}

fn text_of(index: usize) -> String {
    format!("{index}: {WORDS}\n\n```rust\nfn step_{index}() -> usize {{ {index} }}\n```\n")
}

fn grow(file: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::OpenOptions::new().append(true).open(file)?.write_all(text.as_bytes())?;
    Ok(())
}

fn grow_eukhe(sessions: &Path) -> anyhow::Result<()> {
    let file = jsonl(sessions, Some("main.jsonl"))?;
    let text = std::fs::read_to_string(&file)?;
    let commits: Vec<Value> = text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
    let entries = || commits.iter().flat_map(|commit| commit["writes"].as_array().into_iter().flatten());
    let mut seq = commits.last().and_then(|commit| commit["seq"].as_u64()).context("last seq")?;
    let ids = entries().flat_map(|write| [&write["value"]["id"], &write["record"]["id"], &write["id"]]);
    let mut id = ids.filter_map(Value::as_u64).max().context("last id")?;
    let sample = |kind: &str| {
        entries().find(|write| write["value"]["kind"] == kind).cloned().with_context(|| format!("no {kind} entry"))
    };
    let (user, mut assistant) = (sample("pi.user")?, sample("pi.assistant")?);
    if let Some(value) = assistant["value"].as_object_mut() {
        value.remove("byTaskId");
    }
    let mut out = String::new();
    for index in 0..MESSAGES {
        seq += 1;
        id += 1;
        let mut write = if index % 2 == 0 { user.clone() } else { assistant.clone() };
        write["value"]["id"] = json!(id);
        write["value"]["model"][0]["content"] = json!([{ "type": "text", "text": text_of(index) }]);
        writeln!(out, "{}", json!({ "format": 1, "type": "commit", "seq": seq, "writes": [write] }))?;
    }
    grow(&file, &out)
}

fn grow_claude(home: &Path) -> anyhow::Result<()> {
    let file = jsonl(&home.join("projects"), None)?;
    let mut out = String::new();
    for index in 0..MESSAGES {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        let mut message = json!({ "role": role, "content": [{ "type": "text", "text": text_of(index) }] });
        if role == "assistant" {
            message["id"] = json!(format!("msg_{index}"));
            message["model"] = json!("fake-claude");
        }
        let line = json!({ "type": role, "message": message, "timestamp": "2026-11-24T12:00:00.000Z", "uuid": index.to_string() });
        writeln!(out, "{line}")?;
    }
    grow(&file, &out)
}

fn stop_daemon(socket: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (client, _events) = DaemonClient::connect(socket).await?;
        client
            .request_ok(DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() })
            .await?;
        anyhow::Ok(())
    })?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    while std::os::unix::net::UnixStream::connect(socket).is_ok() {
        ensure!(Instant::now() < deadline, "the daemon still answers on {}", socket.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn sessions_let_go(agent_dir: &Path) -> anyhow::Result<()> {
    let leases = agent_dir.join("session-leases");
    let deadline = Instant::now() + STEP_TIMEOUT;
    while std::fs::read_dir(&leases).is_ok_and(|mut held| held.next().is_some()) {
        ensure!(Instant::now() < deadline, "a session worker still holds a lease in {}", leases.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn seed(config: StudioConfig, work: &Path) -> anyhow::Result<[(&'static str, String); 2]> {
    let mut session = Session::open(config)?;
    let outcome = (|| {
        session.studio.add_project(work.to_string_lossy().into_owned());
        session.pump("project added", |watch| watch.projects.len() == 1)?;
        let eukhe = real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local);
        let claude = ThreadSettings {
            provider: Some("claude-code".to_string()),
            model: Some("haiku".to_string()),
            ..real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local)
        };
        let threads = [("Eukhe", session.start("Eukhe", eukhe)?), (CLAUDE, session.start(CLAUDE, claude)?)];
        session.studio.load_usage("usage-1".to_string(), UsagePeriod::Day);
        session.pump("usage report loaded", |watch| watch.usage.is_some())?;
        real_model::check_usage(session.watch.usage.as_ref().context("usage")?)?;
        Ok(threads)
    })();
    session.studio.shutdown();
    outcome
}

fn changed_elsewhere(session: &mut Session, config: StudioConfig, away: &str) -> anyhow::Result<()> {
    let thread = session.start("Short", real_model::thread_settings(AccessMode::FullAccess, WorktreeChoice::Local))?;
    session.show(&thread, 2)?;
    let before = session.watch.items(&thread);
    session.show(away, MESSAGES)?;
    let mut other = Session::open(config)?;
    let sent = (|| {
        other.pump("thread listed", |watch| watch.settled(&thread))?;
        other.studio.select_thread(Some(thread.clone()));
        other.pump("thread shown", |watch| watch.items(&thread) >= before)?;
        other.answer(&thread)
    })();
    other.studio.shutdown();
    sent?;
    session.studio.select_thread(Some(thread.clone()));
    session.pump("the other app's message shown", |watch| watch.items(&thread) > before)?;
    println!("ok   a thread changed in another app shows the change when opened again");
    Ok(())
}

fn panel_chat(session: &mut Session, agent_dir: &Path, work: &Path, socket: &Path) -> anyhow::Result<()> {
    let sessions = agent_dir.join("sessions");
    ensure!(session.watch.threads.iter().all(|row| !row.browser), "a thread of the app is marked as a browser chat");
    let settled = Instant::now() + Duration::from_secs(5);
    session.pump("refreshes settled", |_| Instant::now() >= settled)?;
    browser_chat::write(&sessions, work, 1)?;
    let written = Instant::now();
    session.pump("the side panel's chat listed", |watch| {
        watch.threads.iter().any(|row| row.id == browser_chat::ID && row.browser)
    })?;
    ensure!(written.elapsed() < CHAT_LISTED, "the side panel's chat was listed after {:?}", written.elapsed());
    session.show(browser_chat::ID, 2)?;
    session.watch.notices.clear();
    browser_chat::rewrite(socket, &sessions, work, 2)?;
    let written = Instant::now();
    session.pump("the side panel's new turn shown", |watch| watch.items(browser_chat::ID) >= 4)?;
    ensure!(written.elapsed() < CHAT_LISTED, "the side panel's new turn was shown after {:?}", written.elapsed());
    ensure!(session.watch.notices.is_empty(), "the app said {:?}", session.watch.notices);
    println!("ok   a side panel chat is listed, opened, and shows its next turn without a notice");
    Ok(())
}

fn measure(
    config: impl Fn() -> StudioConfig,
    agent_dir: &Path,
    work: &Path,
    threads: &[(&'static str, String)],
) -> anyhow::Result<()> {
    let mut session = Session::open(config())?;
    let outcome = (|| {
        session.pump("threads listed", |watch| {
            threads.iter().all(|(_, thread)| watch.threads.iter().any(|row| row.id == *thread))
        })?;
        let mut again: HashMap<&str, Vec<Duration>> = HashMap::new();
        for (name, thread) in threads {
            println!("     {name}: opened first in {} ms", session.show(thread, MESSAGES)?.as_millis());
        }
        for _ in 0..RUNS {
            for (name, thread) in threads {
                again.entry(name).or_default().push(session.show(thread, MESSAGES)?);
                session.rest()?;
                let resets = session.watch.resets;
                ensure!(*name == CLAUDE || resets == 1, "{name}: an unchanged thread is sent {resets} times");
            }
        }
        for (name, _) in threads {
            let mut times = again.remove(name).unwrap_or_default();
            times.sort_unstable();
            let times: Vec<u128> = times.iter().map(Duration::as_millis).collect();
            println!("     {name}: opened again in {times:?} ms");
            let most = if cfg!(debug_assertions) && *name == CLAUDE { DEBUG_AGAIN_MS } else { AGAIN_MS };
            ensure!(times[RUNS / 2] < most, "{name}: a {MESSAGES} message thread opens again within {most} ms");
        }
        let other = agent_dir.with_file_name("other").to_string_lossy().into_owned();
        changed_elsewhere(&mut session, StudioConfig { data_dir: Some(other), ..config() }, &threads[1].1)?;
        panel_chat(&mut session, agent_dir, work, &agent_dir.with_file_name("d.sock"))
    })();
    session.studio.shutdown();
    outcome
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: open_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let root = PathBuf::from(args.next().context(usage)?);
    let claude = std::env::current_exe()?.with_file_name("fake_claude");
    ensure!(claude.is_file(), "build the fake_claude example first ({} is missing)", claude.display());
    let home = root.join("home");
    let agent_dir = root.join("agent");
    let work = root.join("work");
    for dir in [&home, &work] {
        std::fs::create_dir_all(dir)?;
    }
    real_model::install(&agent_dir)?;
    std::env::set_var("LOCAL_STUDIO_CLAUDE_BIN", claude);
    std::env::set_var("HOME", &home);
    std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent_dir);
    let socket = root.join("d.sock");
    let config = || StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent_dir.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        controller_main: None,
        take_over_serve: false,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
    };
    let seeded = seed(config(), &work);
    let stopped = stop_daemon(&socket);
    let threads = seeded?;
    stopped?;
    sessions_let_go(&agent_dir)?;
    grow_eukhe(&agent_dir.join("sessions"))?;
    grow_claude(&home.join(".claude"))?;
    let measured = measure(config, &agent_dir, &work, &threads);
    let stopped = stop_daemon(&socket);
    measured?;
    stopped?;
    println!("PASS open smoke");
    Ok(())
}
