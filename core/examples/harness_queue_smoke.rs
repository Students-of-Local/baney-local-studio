use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use local_studio_core::{
    AccessMode, Connection, DaemonLaunch, Delivery, HarnessAccounts, ItemKind, NoticeLevel, NotificationKind,
    PromptDraft, Settings, Studio, StudioConfig, ThreadSettings, TranscriptOp, Update, Waker, WorktreeChoice,
};
use serde_json::{Map, Value};

#[allow(dead_code)]
#[path = "support/harness.rs"]
mod harness;

use harness::Signal;

const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(12);
const PROMPT: Duration = Duration::from_secs(2);
const RESUME_DELAY: &str = "LOCAL_STUDIO_FAKE_CODEX_RESUME_MS";
const START_DELAY: &str = "LOCAL_STUDIO_FAKE_CODEX_START_MS";
const OPEN_LATER: [&str; 2] = ["draft-stall-codex", "draft-codex-failed-stop"];

#[derive(Default)]
struct Watch {
    connection: Option<Connection>,
    projects: Vec<String>,
    rows: Vec<String>,
    started: BTreeMap<String, String>,
    thread: Option<String>,
    busy: bool,
    resets: u32,
    model: Option<String>,
    thinking: String,
    replies: BTreeMap<u64, String>,
    notices: BTreeMap<u64, String>,
    errors: Vec<String>,
    warnings: Vec<String>,
    failed: Vec<(String, String)>,
    refused: Vec<String>,
    misses: Vec<String>,
    settings: Option<Settings>,
}

impl Watch {
    fn replied(&self, text: &str) -> Option<&String> {
        self.replies.values().find(|reply| reply.contains(text))
    }

    fn answers(&self, text: &str) -> usize {
        self.replies.values().filter(|reply| reply.contains(text)).count()
    }

    fn noticed(&self, text: &str) -> bool {
        self.notices.values().any(|notice| notice.contains(text))
    }

    fn miss(&mut self, check: String) {
        println!("MISS {check}");
        self.misses.push(check);
    }
}

fn observe(watch: &mut Watch, id: u64, kind: &ItemKind) {
    match kind {
        ItemKind::Assistant { blocks, streaming: false, .. } => {
            let text: String = blocks
                .iter()
                .filter_map(|block| match block {
                    local_studio_core::Block::Text { text } => Some(text.as_str()),
                    local_studio_core::Block::Thinking { .. } => None,
                })
                .collect();
            if !text.trim().is_empty() {
                watch.replies.insert(id, text);
            }
        }
        ItemKind::Notice { text, .. } => {
            watch.notices.insert(id, text.clone());
        }
        _ => {}
    }
}

fn apply(watch: &mut Watch, update: Update) {
    match update {
        Update::Transcript { thread, op } if watch.thread.as_deref() == Some(thread.as_str()) => match op {
            TranscriptOp::Reset { items } => {
                watch.resets += 1;
                watch.replies.clear();
                watch.notices.clear();
                for item in &items {
                    observe(watch, item.id, &item.kind);
                }
            }
            TranscriptOp::Insert { item, .. } | TranscriptOp::Update { item, .. } => {
                observe(watch, item.id, &item.kind);
            }
        },
        Update::Thread { thread, info } if watch.thread.as_deref() == Some(thread.as_str()) => {
            watch.busy = info.busy;
            watch.model = info.model.map(|model| model.id);
            watch.thinking = info.thinking_level;
        }
        Update::Connection { state } => watch.connection = Some(state),
        Update::Settings { settings } => watch.settings = Some(settings),
        Update::Projects { projects } => watch.projects = projects.into_iter().map(|project| project.id).collect(),
        Update::Threads { threads } => watch.rows = threads.into_iter().map(|row| row.id).collect(),
        Update::ThreadStarted { draft, thread } => {
            watch.started.insert(draft, thread);
        }
        Update::Notice { level, text } => {
            println!("     notice {level:?}: {text}");
            match level {
                NoticeLevel::Error => watch.errors.push(text),
                NoticeLevel::Warning => watch.warnings.push(text),
                NoticeLevel::Info => {}
            }
        }
        Update::Notification { thread, kind: NotificationKind::Failed, body, .. } => watch.failed.push((thread, body)),
        Update::RequestFailed { request, message } => {
            println!("     request failed: {message}");
            watch.refused.push(format!("{request}: {message}"));
        }
        _ => {}
    }
}

struct Run {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    watch: Watch,
    sent: u32,
}

impl Run {
    fn until(&mut self, timeout: Duration, done: impl Fn(&Watch) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            for update in self.studio.tick() {
                apply(&mut self.watch, update);
            }
            if done(&self.watch) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
            self.signal.wait(left.min(Duration::from_millis(50)));
        }
    }

    fn setup(&mut self, step: &str, done: impl Fn(&Watch) -> bool) -> anyhow::Result<()> {
        if self.until(SETUP_TIMEOUT, done) {
            println!("ok   {step}");
            Ok(())
        } else {
            bail!("timed out waiting for: {step}")
        }
    }

    fn check(&mut self, step: &str, done: impl Fn(&Watch) -> bool) -> bool {
        if self.until(CHECK_TIMEOUT, done) {
            println!("ok   {step}");
            true
        } else {
            self.watch.miss(format!("{step} (not within {} s)", CHECK_TIMEOUT.as_secs()));
            false
        }
    }

    fn within(&mut self, step: &str, timeout: Duration, done: impl Fn(&Watch) -> bool) -> bool {
        let started = Instant::now();
        if self.until(timeout, done) {
            println!("ok   {step} ({} ms)", started.elapsed().as_millis());
            true
        } else {
            self.watch.miss(format!("{step} (not within {} ms)", timeout.as_millis()));
            false
        }
    }

    fn focus(&mut self, thread: &str) {
        self.watch.thread = Some(thread.to_string());
        self.watch.busy = false;
        self.watch.model = None;
        self.watch.replies.clear();
        self.watch.notices.clear();
        self.studio.select_thread(Some(thread.to_string()));
    }

    fn shown(&mut self, label: &str, thread: &str) -> anyhow::Result<()> {
        let resets = self.watch.resets;
        self.focus(thread);
        self.setup(&format!("{label}: transcript shown on select"), |watch| watch.resets > resets)
    }

    fn thread(&self) -> String {
        self.watch.thread.clone().unwrap_or_default()
    }

    fn send(&mut self, text: &str) {
        self.sent += 1;
        println!("     send {text:?}");
        self.studio.send(format!("send-{}", self.sent), self.thread(), prompt(text), Delivery::Queue);
    }

    fn start(&mut self, label: &str, (provider, model): (&str, &str), thinking: Option<&str>) -> anyhow::Result<()> {
        self.start_with(label, (provider, model), thinking, AccessMode::FullAccess)
    }

    fn begin(&mut self, label: &str, (provider, model): (&str, &str), thinking: Option<&str>, access: AccessMode) {
        self.watch.thread = None;
        self.watch.busy = false;
        self.watch.replies.clear();
        self.watch.notices.clear();
        let settings = ThreadSettings {
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            thinking_level: thinking.map(str::to_string),
            access,
            worktree: WorktreeChoice::Local,
        };
        let draft = format!("draft-{label}");
        self.studio.start_thread(draft, self.watch.projects.first().cloned(), prompt("hello"), settings);
    }

    fn start_with(
        &mut self,
        label: &str,
        (provider, model): (&str, &str),
        thinking: Option<&str>,
        access: AccessMode,
    ) -> anyhow::Result<()> {
        self.begin(label, (provider, model), thinking, access);
        let draft = format!("draft-{label}");
        self.setup(&format!("{label}: thread started"), |watch| watch.started.contains_key(&draft))?;
        let thread = self.watch.started.get(&draft).cloned().context("thread id")?;
        self.watch.thread = Some(thread.clone());
        self.studio.select_thread(Some(thread));
        self.setup(&format!("{label}: first reply"), |watch| watch.replied("reply to hello").is_some() && !watch.busy)
    }

    fn slow(&mut self, text: &str) -> anyhow::Result<()> {
        self.send(text);
        self.setup(&format!("{text:?} is working"), |watch| watch.busy)
    }
}

fn prompt(text: &str) -> PromptDraft {
    PromptDraft { text: text.to_string(), images: Vec::new(), files: Vec::new() }
}

fn ran_at(run: &mut Run, text: &str, effort: &str) {
    run.send(text);
    let reply = format!("reply to {text}");
    if run.check(&format!("{text:?} is answered"), |watch| watch.replied(&reply).is_some()) {
        let reply = run.watch.replied(&reply).cloned().unwrap_or_default();
        if reply.contains(&format!("at effort {effort}")) {
            println!("ok   {text:?} ran on the new effort: {reply}");
        } else {
            run.watch.miss(format!("{text:?} did not run on the new effort: {reply}"));
        }
    }
}

fn claude_restart_keeps_message(run: &mut Run) -> anyhow::Result<()> {
    run.start("claude-restart", ("claude-code", "haiku"), Some("low"))?;
    run.slow("slow alpha")?;
    run.studio.set_thinking_level(run.thread(), "medium".to_string());
    run.send("queued bravo");
    if !run.check("a message sent during the turn that changed the effort is answered", |watch| {
        watch.replied("reply to queued bravo").is_some()
    }) {
        return Ok(());
    }
    run.setup("the thread is idle after the held message", |watch| !watch.busy)?;
    ran_at(run, "charlie", "medium");
    Ok(())
}

fn claude_effort_during_open(run: &mut Run) -> anyhow::Result<()> {
    run.start("claude-reopen", ("claude-code", "haiku"), Some("low"))?;
    run.studio.restart_thread(run.thread());
    run.studio.set_thinking_level(run.thread(), "high".to_string());
    run.until(Duration::from_secs(1), |_| false);
    ran_at(run, "xray", "high");
    Ok(())
}

fn delete_during_start(run: &mut Run) -> anyhow::Result<()> {
    std::env::set_var(START_DELAY, "3000");
    let known = run.watch.rows.clone();
    run.begin("codex-delete", ("codex", "fake-mini"), None, AccessMode::FullAccess);
    let fresh = |watch: &Watch| watch.rows.iter().find(|row| !known.contains(row)).cloned();
    run.setup("codex-delete: the new thread is listed", |watch| fresh(watch).is_some())?;
    run.studio.delete_thread(fresh(&run.watch).context("new thread")?);
    run.check("deleting a thread before it started fails its draft", |watch| {
        watch.refused.iter().any(|refused| refused.starts_with("draft-codex-delete: "))
    });
    std::env::remove_var(START_DELAY);
    Ok(())
}

fn claude_stop_drops_held(run: &mut Run) -> anyhow::Result<()> {
    run.start("claude-discard", ("claude-code", "haiku"), Some("low"))?;
    run.slow("slow discard tango")?;
    run.studio.set_thinking_level(run.thread(), "medium".to_string());
    run.send("uniform");
    run.studio.abort(run.thread());
    run.setup("the stopped turn ended", |watch| !watch.busy)?;
    run.until(Duration::from_secs(5), |_| false);
    ran_at(run, "victor", "medium");
    Ok(())
}

fn claude_stop_after_effort_change(run: &mut Run) -> anyhow::Result<()> {
    run.start("claude-stop", ("claude-code", "haiku"), Some("low"))?;
    let thread = run.thread();
    let errors = run.watch.errors.len();
    let failed = run.watch.failed.len();
    run.slow("slow quota delta")?;
    run.studio.set_thinking_level(thread.clone(), "high".to_string());
    run.check("an account stop after an effort change shows the account error", |watch| {
        watch.errors[errors..].iter().any(|error| error.contains("account problem"))
    });
    run.check("an account stop after an effort change raises a Failed notification", |watch| {
        watch.failed[failed..].iter().any(|(id, body)| *id == thread && body.contains("account problem"))
    });
    Ok(())
}

fn codex_start_race(run: &mut Run) -> anyhow::Result<()> {
    run.start("codex-race", ("codex", "fake-mini"), None)?;
    let refused = run.watch.refused.len();
    run.slow("slow echo")?;
    run.send("lag foxtrot");
    run.setup("the running turn ended and the held message is starting", |watch| {
        watch.replied("reply to slow echo").is_some() && !watch.busy
    })?;
    run.send("golf");
    let both = run.check("a send while a held message is starting waits for it", |watch| {
        watch.replied("reply to lag foxtrot").is_some() && watch.replied("reply to golf").is_some()
    });
    if both {
        let order: Vec<&String> = run.watch.replies.values().collect();
        let foxtrot = order.iter().position(|reply| reply.contains("foxtrot"));
        let golf = order.iter().position(|reply| reply.contains("golf"));
        if foxtrot < golf {
            println!("ok   the held message ran before the later send");
        } else {
            run.watch.miss(format!("the later send ran before the held message: {order:?}"));
        }
    }
    if run.watch.refused.len() > refused {
        let refused = run.watch.refused[refused..].join("; ");
        run.watch.miss(format!("a send was refused while a held message was starting: {refused}"));
    }
    Ok(())
}

fn codex_failed_start(run: &mut Run) -> anyhow::Result<()> {
    run.start("codex-reject", ("codex", "fake-mini"), None)?;
    run.slow("slow hotel")?;
    run.send("reject india");
    run.send("juliet");
    run.check("a held message after one Codex refused to start still runs", |watch| {
        watch.replied("reply to juliet").is_some()
    });
    Ok(())
}

fn codex_drop(run: &mut Run, label: &str, first: &str, held: &str) -> anyhow::Result<()> {
    run.start(label, ("codex", "fake-mini"), None)?;
    run.slow(first)?;
    run.send(held);
    run.check(&format!("{label}: the dropped held message is shown in the transcript"), |watch| watch.noticed(held));
    Ok(())
}

fn codex_unexpected_approval(run: &mut Run) -> anyhow::Result<()> {
    run.start_with("codex-skill", ("codex", "fake-mini"), None, AccessMode::Supervised)?;
    let thread = run.thread();
    let mut watch = harness::Watch::default();
    watch.thread = Some(thread.clone());
    let step = "a Supervised turn that asks an approval no step expects";
    let outcome = harness::turn(&run.studio, &run.signal, &mut watch, &thread, "skill quebec", step);
    let denied: Vec<String> = watch.denied.iter().map(|(approval, _)| approval.command.clone()).collect();
    match outcome {
        Ok(reply) if reply.contains("after decline") && denied.iter().any(|command| command.contains("SKILL.md")) => {
            println!("ok   the unexpected approval {denied:?} was denied and the turn went on: {reply}");
        }
        Ok(reply) => run.watch.miss(format!("the turn ended without a deny of its approval: {reply}")),
        Err(error) => run.watch.miss(format!("{error:#}")),
    }
    Ok(())
}

fn codex_stall_keeps_others(run: &mut Run) -> anyhow::Result<()> {
    run.start("stall-claude", ("claude-code", "haiku"), Some("low"))?;
    let claude = run.thread();
    run.start("stall-codex", ("codex", "fake-mini"), None)?;
    let codex = run.thread();
    run.send("stall kilo");
    run.focus(&claude);
    run.send("lima");
    run.within("a Claude Code thread answers while a Codex turn/start stalls", PROMPT, |watch| {
        watch.replied("reply to lima").is_some()
    });
    run.focus(&codex);
    run.setup("the stalled Codex turn finishes", |watch| {
        watch.replied("reply to stall kilo").is_some() && !watch.busy
    })?;
    let warnings = run.watch.warnings.len();
    let codex_processes = fakes_running("fake_codex");
    run.send("lag slow mike");
    run.setup("the stalled send shows as working", |watch| watch.busy)?;
    run.studio.abort(codex.clone());
    run.check("Stop while turn/start is pending interrupts the turn once it starts", |watch| !watch.busy);
    if run.watch.replied("reply to lag slow mike").is_some() {
        run.watch.miss("the turn stopped while starting ran to the end".to_string());
    }
    let warned = run.watch.warnings[warnings..].join("; ");
    if warned.is_empty() && fakes_running("fake_codex") == codex_processes {
        println!("ok   Stop while starting kept the Codex process");
    } else {
        run.watch.miss(format!("Stop while starting killed or warned: {warned:?}"));
    }
    run.send("november");
    run.check("the thread answers after Stop while starting", |watch| watch.replied("reply to november").is_some());
    Ok(())
}

fn thread_flag(root: &Path, thread: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join("data").join("state.json")).ok()?;
    let state: Value = serde_json::from_str(&text).ok()?;
    state["threads"][thread][key].as_str().map(str::to_string)
}

fn holds(dir: &Path, suffix: &str) -> bool {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if entry.file_name().to_string_lossy().ends_with(suffix) {
                return true;
            }
        }
    }
    false
}

struct Account<'a> {
    root: &'a Path,
    label: &'a str,
    model: (&'a str, &'a str),
    thinking: Option<&'a str>,
    first: PathBuf,
    second: PathBuf,
}

impl Account<'_> {
    fn on(&self, run: &mut Run, step: &str, thread: &str, dir: &Path) {
        let (root, thread, want) = (self.root.to_path_buf(), thread.to_string(), dir.to_string_lossy().into_owned());
        run.check(step, |_| thread_flag(&root, &thread, "account").as_deref() == Some(want.as_str()));
    }

    fn session_in(&self, thread: &str, dir: &Path) -> bool {
        if self.model.0 == "codex" {
            thread_flag(self.root, thread, "nativeSession")
                .is_some_and(|native| holds(&dir.join("sessions"), &format!("-{native}.jsonl")))
        } else {
            holds(&dir.join("projects"), &format!("{thread}.jsonl"))
        }
    }
}

fn account_switch(run: &mut Run, account: &Account) -> anyhow::Result<()> {
    let label = account.label;
    let (first, second) = (&account.first, &account.second);
    let (from, to) = (name_of(first), name_of(second));
    std::fs::create_dir_all(first)?;
    std::fs::create_dir_all(second)?;
    std::fs::write(first.join("limited"), "")?;
    run.start(&format!("{label}-switch"), account.model, account.thinking)?;
    let thread = run.thread();
    account.on(run, &format!("{label}: a new thread starts on the first account {from}"), &thread, first);
    let failed = run.watch.failed.len();
    run.send("limit alpha");
    run.check(&format!("{label}: a turn that hits the usage limit on {from} is answered on {to}"), |watch| {
        watch.replied("reply to limit alpha").is_some() && !watch.busy
    });
    let notice = format!("{from} reached its usage limit until ");
    let next = format!("; continuing on {to}.");
    run.check(&format!("{label}: the transcript says the thread moved from {from} to {to}"), |watch| {
        watch.notices.values().any(|text| text.contains(&notice) && text.contains(&next))
    });
    account.on(run, &format!("{label}: the thread now runs on {to}"), &thread, second);
    if account.session_in(&thread, second) {
        println!("ok   {label}: the session file is in {to}");
    } else {
        run.watch.miss(format!("{label}: no session file for the thread in {to}"));
    }
    let raised: Vec<String> =
        run.watch.failed[failed..].iter().filter(|(id, _)| *id == thread).map(|(_, body)| body.clone()).collect();
    if raised.is_empty() {
        println!("ok   {label}: the switch raised no Failed notification");
    } else {
        run.watch.miss(format!("{label}: the switch raised a Failed notification: {raised:?}"));
    }
    run.start(&format!("{label}-after"), account.model, account.thinking)?;
    let after = run.thread();
    account.on(run, &format!("{label}: a new thread after the switch starts on {to}"), &after, second);
    std::fs::write(second.join("limited"), "")?;
    run.focus(&thread);
    let (errors, failed) = (run.watch.errors.len(), run.watch.failed.len());
    run.send("limit bravo");
    run.check(&format!("{label}: with every account limited the error names when the first one resets"), |watch| {
        watch.errors[errors..]
            .iter()
            .any(|error| error.contains("account problem") && error.contains("at its usage limit until"))
    });
    run.check(&format!("{label}: with every account limited a Failed notification names the reset"), |watch| {
        watch.failed[failed..].iter().any(|(id, body)| *id == thread && body.contains("at its usage limit until"))
    });
    Ok(())
}

fn name_of(dir: &Path) -> String {
    dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn account_scenarios(run: &mut Run, root: &Path) -> anyhow::Result<()> {
    let home = root.join("home");
    let dirs = |base: &str| (home.join(format!("{base}-a")), home.join(format!("{base}-b")));
    let (claude_a, claude_b) = dirs(".claude");
    let (codex_a, codex_b) = dirs(".codex");
    let path = |dir: &PathBuf| dir.to_string_lossy().into_owned();
    let mut settings = run.watch.settings.clone().unwrap_or_default();
    settings.harness_accounts =
        HarnessAccounts { claude: vec![path(&claude_a), path(&claude_b)], codex: vec![path(&codex_a), path(&codex_b)] };
    settings.switch_accounts_on_limit = true;
    run.studio.update_settings(settings);
    run.setup("two scratch accounts are listed for each CLI", |watch| {
        watch.settings.as_ref().is_some_and(|settings| settings.harness_accounts.codex.len() == 2)
    })?;
    let claude = Account {
        root,
        label: "claude-account",
        model: ("claude-code", "haiku"),
        thinking: Some("low"),
        first: claude_a,
        second: claude_b,
    };
    account_switch(run, &claude)?;
    let codex = Account {
        root,
        label: "codex-account",
        model: ("codex", "fake-mini"),
        thinking: None,
        first: codex_a,
        second: codex_b,
    };
    account_switch(run, &codex)
}

fn scenarios(run: &mut Run, root: &Path) -> anyhow::Result<()> {
    codex_stall_keeps_others(run)?;
    claude_restart_keeps_message(run)?;
    claude_effort_during_open(run)?;
    delete_during_start(run)?;
    claude_stop_after_effort_change(run)?;
    claude_stop_drops_held(run)?;
    codex_start_race(run)?;
    codex_failed_start(run)?;
    codex_drop(run, "codex-exit", "slow exit kilo", "lima")?;
    codex_drop(run, "codex-start-exit", "slow romeo", "crash sierra")?;
    codex_drop(run, "codex-error-stop", "slow quota-error mike", "november")?;
    codex_drop(run, "codex-failed-stop", "slow quota-fail oscar", "papa")?;
    codex_unexpected_approval(run)?;
    account_scenarios(run, root)
}

fn fakes_running(name: &str) -> usize {
    let parent = std::process::id().to_string();
    let Ok(listing) = Command::new("ps").args(["-A", "-o", "ppid=,command="]).output() else { return 0 };
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| line.trim().split_once(' '))
        .filter(|(ppid, command)| *ppid == parent && command.contains(name))
        .count()
}

fn clis_running() -> usize {
    fakes_running("fake_claude") + fakes_running("fake_codex")
}

fn select_spawns_nothing(run: &mut Run, started: &BTreeMap<String, String>) -> anyhow::Result<()> {
    run.setup("no CLI process runs before any thread is selected", |_| clis_running() == 0)?;
    let viewed: Vec<(&String, &String)> =
        started.iter().filter(|(label, _)| !OPEN_LATER.contains(&label.as_str())).collect();
    for (label, thread) in &viewed {
        run.shown(label, thread)?;
        match label.as_str() {
            "draft-claude-restart" => {
                run.check("select shows the earlier Claude Code turns", |watch| {
                    watch.replied("reply to charlie").is_some()
                });
                run.check("select shows the Claude Code model and effort", |watch| {
                    watch.model.as_deref() == Some("haiku") && watch.thinking == "medium"
                });
            }
            "draft-codex-race" => {
                run.check("select shows the earlier Codex turns", |watch| watch.replied("reply to golf").is_some());
                run.check("select shows the Codex model", |watch| watch.model.as_deref() == Some("fake-mini"));
            }
            _ => {}
        }
    }
    let count = viewed.len();
    run.check(&format!("selecting {count} CLI threads without sending leaves no CLI process running"), |_| {
        clis_running() == 0
    });
    println!("     CLI processes running: {}", clis_running());
    Ok(())
}

fn send_during_open(run: &mut Run, started: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let codex = started.get("draft-stall-codex").context("stall-codex thread")?.clone();
    let claude = started.get("draft-claude-discard").context("claude-discard thread")?.clone();
    std::env::set_var(RESUME_DELAY, "4000");
    run.focus(&codex);
    run.send("alpha");
    run.send("bravo");
    run.focus(&claude);
    run.send("yankee");
    run.within("a Claude Code thread answers while a Codex thread/resume stalls", PROMPT, |watch| {
        watch.replied("reply to yankee").is_some()
    });
    run.focus(&codex);
    if run.check("both sends during the in-flight open are answered", |watch| {
        watch.replied("reply to alpha").is_some() && watch.replied("reply to bravo").is_some()
    }) {
        let (alpha, bravo) = (run.watch.answers("reply to alpha"), run.watch.answers("reply to bravo"));
        if alpha == 1 && bravo == 1 {
            println!("ok   each send during the open ran once");
        } else {
            run.watch.miss(format!("sends during the open ran {alpha} and {bravo} times"));
        }
        let order: Vec<&String> = run.watch.replies.values().collect();
        let first = order.iter().position(|reply| reply.contains("alpha"));
        let second = order.iter().position(|reply| reply.contains("bravo"));
        if first < second {
            println!("ok   the sends ran in order");
        } else {
            run.watch.miss(format!("the sends during the open ran out of order: {order:?}"));
        }
    }
    let codex_processes = fakes_running("fake_codex");
    if codex_processes == 1 {
        println!("ok   one Codex process serves the thread");
    } else {
        run.watch.miss(format!("{codex_processes} Codex processes run for one thread"));
    }
    Ok(())
}

fn stop_during_open(run: &mut Run, started: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let thread = started.get("draft-codex-failed-stop").context("codex-failed-stop thread")?.clone();
    std::env::set_var(RESUME_DELAY, "4000");
    let before = fakes_running("fake_codex");
    run.focus(&thread);
    run.send("charlie");
    run.setup("the send during the open shows as working", |watch| watch.busy)?;
    run.studio.abort(thread.clone());
    run.within("Stop during an open takes the thread out of Working", PROMPT, |watch| !watch.busy);
    run.check("Stop during an open shows the dropped message", |watch| watch.noticed("charlie"));
    run.check("Stop during an open leaves no process for that thread", |_| fakes_running("fake_codex") == before);
    std::env::remove_var(RESUME_DELAY);
    run.send("delta");
    run.check("a send after the cancelled open is answered", |watch| watch.replied("reply to delta").is_some());
    if run.watch.replied("reply to charlie").is_some() {
        run.watch.miss("the message dropped by Stop ran anyway".to_string());
    }
    Ok(())
}

fn compact_during_open(run: &mut Run, started: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let thread = started.get("draft-codex-race").context("codex-race thread")?.clone();
    std::env::set_var(RESUME_DELAY, "1500");
    run.focus(&thread);
    run.studio.compact(thread);
    run.send("whiskey");
    run.check("Codex gets a compact sent during the open before the send after it", |watch| {
        watch.replied("reply to whiskey after compact").is_some()
    });
    std::env::remove_var(RESUME_DELAY);
    Ok(())
}

fn reopened(run: &mut Run, started: &BTreeMap<String, String>) -> anyhow::Result<()> {
    run.setup("daemon connected again", |watch| matches!(watch.connection, Some(Connection::Ready)))?;
    select_spawns_nothing(run, started)?;
    send_during_open(run, started)?;
    stop_during_open(run, started)?;
    compact_during_open(run, started)
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
    std::fs::write(work.join("README.md"), "Scratch repo for the harness queue smoke.\n")?;
    git(work, &["add", "README.md"])?;
    git(work, &["-c", "user.name=smoke", "-c", "user.email=smoke@localhost", "commit", "-q", "-m", "init"])
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
    let usage = "usage: harness_queue_smoke <eukhe> <scratch dir>";
    let mut args = std::env::args_os().skip(1);
    let daemon = PathBuf::from(args.next().context(usage)?);
    let root = PathBuf::from(args.next().context(usage)?);
    let examples = std::env::current_exe()?;
    let fakes = [("LOCAL_STUDIO_CLAUDE_BIN", "fake_claude"), ("LOCAL_STUDIO_CODEX_BIN", "fake_codex")];
    for (variable, name) in fakes {
        let binary = examples.with_file_name(name);
        ensure!(binary.is_file(), "build the {name} example first ({} is missing)", binary.display());
        std::env::set_var(variable, binary);
    }
    let home = root.join("home");
    let agent_dir = root.join("agent");
    let work = root.join("work");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&agent_dir)?;
    scratch_repo(&work)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
    std::env::set_var("CODEX_HOME", home.join(".codex"));
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", root.join("data"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", &agent_dir);
    let socket = root.join("d.sock");
    let config = StudioConfig {
        socket_path: Some(socket.to_string_lossy().into_owned()),
        session_dir: Some(agent_dir.join("sessions").to_string_lossy().into_owned()),
        data_dir: None,
        controller_main: None,
        launch: Some(DaemonLaunch { program: daemon.to_string_lossy().into_owned() }),
        take_over_serve: false,
    };
    let signal = Arc::new(Signal::default());
    let studio = Studio::new(config.clone(), Arc::clone(&signal) as Arc<dyn Waker>)?;
    let mut run = Run { studio: Arc::clone(&studio), signal, watch: Watch::default(), sent: 0 };
    let outcome = (|| {
        run.setup("daemon connected", |watch| matches!(watch.connection, Some(Connection::Ready)))?;
        studio.add_project(work.to_string_lossy().into_owned());
        run.setup("project added", |watch| watch.projects.len() == 1)?;
        scenarios(&mut run, &root)
    })();
    studio.shutdown();
    let mut misses = std::mem::take(&mut run.watch.misses);
    let outcome = outcome.and_then(|()| {
        println!("     restart Studio on the same data dir");
        let started = std::mem::take(&mut run.watch.started);
        let signal = Arc::new(Signal::default());
        let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>)?;
        let mut again = Run { studio: Arc::clone(&studio), signal, watch: Watch::default(), sent: run.sent };
        let outcome = reopened(&mut again, &started);
        studio.shutdown();
        misses.append(&mut again.watch.misses);
        outcome
    });
    let stopped = stop_daemon(&socket);
    outcome?;
    stopped?;
    if !misses.is_empty() {
        bail!("{} checks failed:\n- {}", misses.len(), misses.join("\n- "));
    }
    println!("PASS harness_queue_smoke");
    Ok(())
}
