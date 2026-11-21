use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Block, DaemonLaunch, Delivery, Item, ItemKind, Machine, MachineState, Project, PromptDraft, Studio,
    StudioConfig, ThreadRow, ThreadSettings, ThreadStatus, TranscriptOp, Update, Waker, WorktreeChoice,
};
use serde_json::{Map, Value};

const QUICK: Duration = Duration::from_secs(60);
const MODEL_STEP: Duration = Duration::from_secs(300);
const PROVIDER: &str = "claude-code";
const MODEL: &str = "haiku";
const FIRST: &str = "Reply with exactly: far pong";
const SECOND: &str = "Now reply with exactly: second";

fn draft(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
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

#[derive(Default)]
struct View {
    machines: Vec<Machine>,
    threads: Vec<ThreadRow>,
    projects: Vec<Project>,
    started: HashMap<String, String>,
    items: HashMap<String, Vec<Item>>,
    failed: HashMap<String, String>,
    partials: HashMap<String, usize>,
    daemon: Option<u32>,
}

impl View {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Machines { machines } => self.machines = machines,
            Update::Threads { threads } => self.threads = threads,
            Update::Projects { projects } => self.projects = projects,
            Update::ThreadStarted { draft, thread } => {
                self.started.insert(draft, thread);
            }
            Update::Transcript { thread, op } => {
                if matches!(&op, TranscriptOp::Insert { item, .. } | TranscriptOp::Update { item, .. }
                    if matches!(item.kind, ItemKind::Assistant { streaming: true, .. }))
                {
                    *self.partials.entry(thread.clone()).or_default() += 1;
                }
                let items = self.items.entry(thread).or_default();
                match op {
                    TranscriptOp::Reset { items: all } => *items = all,
                    TranscriptOp::Insert { index, item } => {
                        let index = usize::try_from(index).unwrap_or(usize::MAX).min(items.len());
                        items.insert(index, item);
                    }
                    TranscriptOp::Update { index, item } => {
                        if let Some(slot) = usize::try_from(index).ok().and_then(|index| items.get_mut(index)) {
                            *slot = item;
                        }
                    }
                }
            }
            Update::Notice { level, text } => println!("     notice {level:?}: {text}"),
            Update::RequestFailed { request, message } => {
                println!("     request {request} failed: {message}");
                self.failed.insert(request, message);
            }
            Update::Daemon { pid: Some(pid), .. } => self.daemon = Some(pid),
            _ => {}
        }
    }

    fn row(&self, thread: &str) -> Option<&ThreadRow> {
        self.threads.iter().find(|row| row.id == thread)
    }

    fn idle(&self, thread: &str) -> bool {
        self.row(thread).is_some_and(|row| !matches!(row.status, ThreadStatus::Working | ThreadStatus::Approval))
    }

    fn items(&self, thread: &str) -> &[Item] {
        self.items.get(thread).map_or(&[], Vec::as_slice)
    }

    fn replies(&self, thread: &str) -> Vec<String> {
        self.items(thread).iter().filter_map(reply).collect()
    }

    fn partials(&self, thread: &str) -> usize {
        self.partials.get(thread).copied().unwrap_or_default()
    }

    fn errors(&self, thread: &str) -> Vec<String> {
        self.items(thread)
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Assistant { error: Some(error), .. } => Some(format!("{error:?}")),
                _ => None,
            })
            .collect()
    }

    fn settled(&self, thread: &str, replies: usize, errors: usize) -> bool {
        self.idle(thread) && (self.replies(thread).len() > replies || self.errors(thread).len() > errors)
    }
}

fn reply(item: &Item) -> Option<String> {
    let ItemKind::Assistant { blocks, streaming: false, error: None, .. } = &item.kind else { return None };
    let text: String = blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(text.as_str()),
            Block::Thinking { .. } => None,
        })
        .collect();
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

struct Desk {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    view: View,
}

impl Desk {
    fn until(&mut self, step: &str, bound: Duration, mut check: impl FnMut(&View) -> bool) -> anyhow::Result<Duration> {
        let started = Instant::now();
        loop {
            for update in self.studio.tick() {
                self.view.apply(update);
            }
            if check(&self.view) {
                let elapsed = started.elapsed();
                println!("ok   {step} ({} ms)", elapsed.as_millis());
                return Ok(elapsed);
            }
            let Some(left) = bound.checked_sub(started.elapsed()) else {
                bail!("timed out after {} s: {step}", bound.as_secs());
            };
            let pending = self.signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut pending, _) = self
                .signal
                .ready
                .wait_timeout_while(pending, left.min(Duration::from_millis(200)), |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner);
            *pending = false;
        }
    }
}

fn settle(desk: &mut Desk, thread: &str, request: &str, before: (usize, usize, usize)) -> anyhow::Result<String> {
    let (replies, errors, partials) = before;
    desk.until("the reply comes back and the thread settles", MODEL_STEP, |view| {
        view.settled(thread, replies, errors) || view.failed.contains_key(request)
    })?;
    ensure!(!desk.view.failed.contains_key(request), "{request} failed");
    let failed = desk.view.errors(thread);
    ensure!(failed.len() == errors, "the far turn ended in an error: {}", failed.last().cloned().unwrap_or_default());
    let last = desk.view.replies(thread).last().cloned().unwrap_or_default();
    println!(
        "     {} partial transcript updates arrived before the reply settled",
        desk.view.partials(thread) - partials
    );
    Ok(last)
}

fn marks(view: &View, thread: &str) -> (usize, usize, usize) {
    (view.replies(thread).len(), view.errors(thread).len(), view.partials(thread))
}

fn turn(desk: &mut Desk, thread: &str, request: &str, text: &str, expect: &str) -> anyhow::Result<()> {
    let before = marks(&desk.view, thread);
    let sent = Instant::now();
    desk.studio.send(request.to_string(), thread.to_string(), draft(text), Delivery::Default);
    let last = settle(desk, thread, request, before)?;
    ensure!(last.to_ascii_lowercase().contains(expect), "the reply is {last:?}");
    println!("ok   reply {last:?} in {} ms", sent.elapsed().as_millis());
    Ok(())
}

fn probe(desk: &mut Desk, link: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    desk.studio.pair_machine(link.to_string());
    desk.until("pair_machine connects the far machine", QUICK, |view| {
        view.machines.iter().any(|machine| machine.state == MachineState::Connected)
    })?;
    let machine =
        desk.view.machines.iter().find(|machine| machine.state == MachineState::Connected).cloned().context("none")?;
    println!(
        "     {} ({}) {} {} at {}, paired in {} ms",
        machine.name,
        machine.id,
        machine.os,
        machine.version,
        machine.url,
        started.elapsed().as_millis()
    );
    let id = machine.id.clone();
    desk.until("the latency shows", QUICK, |view| {
        view.machines.iter().any(|known| known.id == id && known.latency_ms.is_some())
    })?;
    let latency = desk.view.machines.iter().find(|known| known.id == id).and_then(|known| known.latency_ms);
    println!("     latency {latency:?} ms");
    desk.until("the far machine's projects arrive", QUICK, |view| {
        view.projects.iter().any(|project| project.machine.as_deref() == Some(&id))
    })?;
    let project = desk
        .view
        .projects
        .iter()
        .find(|project| project.machine.as_deref() == Some(&id))
        .context("no far project")?
        .clone();
    println!("     project {} at {}", project.id, project.path);
    let settings = ThreadSettings {
        provider: Some(PROVIDER.to_string()),
        model: Some(MODEL.to_string()),
        thinking_level: None,
        access: AccessMode::Supervised,
        worktree: WorktreeChoice::Local,
    };
    let started = Instant::now();
    desk.studio.start_thread("probe-1".to_string(), Some(project.id.clone()), draft(FIRST), settings);
    desk.until("start_thread answers with a thread", MODEL_STEP, |view| {
        view.started.contains_key("probe-1") || view.failed.contains_key("probe-1")
    })?;
    let thread = desk.view.started.get("probe-1").cloned().context("start_thread failed")?;
    ensure!(thread.starts_with(&format!("@{id}/")), "the thread id {thread} is not on the far machine");
    desk.studio.select_thread(Some(thread.clone()));
    let first = settle(desk, &thread, "probe-1", (0, 0, 0))?;
    ensure!(first.to_ascii_lowercase().contains("far pong"), "the first reply is {first:?}");
    println!("ok   {thread}: reply {first:?} in {} ms from start", started.elapsed().as_millis());
    turn(desk, &thread, "probe-2", SECOND, "second")?;
    desk.studio.forget_machine(id.clone());
    desk.until("forget_machine removes the far machine", QUICK, |view| {
        view.machines.iter().all(|machine| machine.id != id)
    })?;
    Ok(())
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn stop_daemon(socket: &Path, known: Option<u32>) -> anyhow::Result<()> {
    let mut pid = known;
    if let Ok((client, _events)) = DaemonClient::connect(socket).await {
        pid =
            client.hello().get("supervisorPid").and_then(Value::as_u64).and_then(|pid| u32::try_from(pid).ok()).or(pid);
        let command = DaemonCommand::Shutdown { id: None, force: Some(true), if_idle: None, rest: Map::default() };
        client.request_ok(command).await.ok();
        client.close();
    }
    let Some(pid) = pid else { return Ok(()) };
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        ensure!(Instant::now() < deadline, "daemon {pid} is still running after Shutdown");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("ok   local daemon {pid} stopped");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let usage = "usage: link_probe <eukhe> <scratch dir>, with the pairing link in LOCAL_STUDIO_PAIR_LINK";
    let mut args = std::env::args_os().skip(1);
    let eukhe = std::fs::canonicalize(PathBuf::from(args.next().context(usage)?))?;
    let root = PathBuf::from(args.next().context(usage)?);
    let link = std::env::var("LOCAL_STUDIO_PAIR_LINK").context(usage)?;
    let (home, data, agent, socket) = (root.join("home"), root.join("data"), root.join("agent"), root.join("d.sock"));
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&agent)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_DAEMON_SOCKET", &socket);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent);
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    let signal = Arc::new(Signal::default());
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: Some(data.to_string_lossy().into_owned()),
        launch: Some(DaemonLaunch { program: eukhe.to_string_lossy().into_owned() }),
        controller_main: None,
        take_over_serve: false,
    };
    let studio =
        Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut desk = Desk { studio, signal, view: View::default() };
    let result = probe(&mut desk, &link);
    desk.studio.shutdown();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let stopped = runtime.block_on(stop_daemon(&socket, desk.view.daemon));
    let result = match (result, stopped) {
        (Err(error), Err(stop)) => {
            println!("FAIL {stop:#}");
            Err(error)
        }
        (result, stopped) => result.and(stopped),
    };
    match &result {
        Ok(()) => println!("PASS link probe"),
        Err(error) => println!("FAIL {error:#}"),
    }
    result
}
