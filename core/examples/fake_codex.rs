use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const SLOW: Duration = Duration::from_millis(1500);
const LAG: Duration = Duration::from_millis(1500);
const STALL: Duration = Duration::from_secs(5);
const RESUME_DELAY: &str = "LOCAL_STUDIO_FAKE_CODEX_RESUME_MS";
const START_DELAY: &str = "LOCAL_STUDIO_FAKE_CODEX_START_MS";
const STOP_WORDS: &str = "You've hit your usage limit.";
const SKILL_READ: &str = "cat ~/.codex/skills/writing/SKILL.md";
const LIMITED: &str = "limited";
const LIMIT_WORDS: &str = "You've hit your usage limit. Try again later.";
const RESET_SECONDS: u64 = 2 * 60 * 60;
const ROLLOUT_DAY: &str = "2026/11/21";

#[derive(Default)]
struct Shared {
    out: Mutex<()>,
    busy: Mutex<bool>,
    interrupted: AtomicBool,
    compacted: AtomicBool,
    turns: AtomicU64,
    answers: Mutex<Vec<(Value, Sender<String>)>>,
    thread: Mutex<String>,
    trust: Mutex<String>,
}

impl Shared {
    fn write(&self, frame: &Value) {
        let _guard = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = std::io::stdout().lock();
        writeln!(out, "{frame}").ok();
        out.flush().ok();
    }

    fn respond(&self, id: &Value, result: &Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn refuse(&self, id: &Value, message: &str) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32600, "message": message } }));
    }

    fn notify(&self, method: &str, params: &Value) {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn ask(&self, turn: &str, number: u64) -> Option<String> {
        let id = json!(format!("approval-{number}"));
        let (sender, answer) = channel();
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).push((id.clone(), sender));
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "item/commandExecution/requestApproval",
            "params": { "turnId": turn, "itemId": format!("command-{number}"), "command": SKILL_READ },
        }));
        loop {
            if let Ok(decision) = answer.recv_timeout(Duration::from_millis(20)) {
                return Some(decision);
            }
            if self.interrupted.load(Ordering::Relaxed) {
                return None;
            }
        }
    }

    fn answered(&self, id: &Value, result: &Value) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = answers.iter().position(|(known, _)| known == id) {
            let (_, sender) = answers.remove(index);
            sender.send(result["decision"].as_str().unwrap_or_default().to_string()).ok();
        }
    }

    fn idle(&self) {
        *self.busy.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }

    fn store(thread: &str) -> Option<PathBuf> {
        let sessions = account()?.join("sessions");
        let suffix = format!("-{thread}.jsonl");
        let mut pending = vec![sessions.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if entry.file_name().to_string_lossy().ends_with(&suffix) {
                    return Some(path);
                }
            }
        }
        Some(sessions.join(ROLLOUT_DAY).join(format!("rollout-2026-11-21T00-00-00{suffix}")))
    }

    fn remember(&self, turn: &Value) {
        let thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some(file) = Self::store(&thread) else { return };
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
            writeln!(out, "{turn}").ok();
        }
    }

    fn thread(&self, id: &Value) -> Value {
        let id = id.as_str().unwrap_or_default();
        *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = id.to_string();
        let turns: Vec<Value> = Self::store(id)
            .and_then(|file| std::fs::read_to_string(file).ok())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        json!({ "thread": { "id": id, "turns": turns } })
    }

    fn completed(&self, turn: &str, status: &str, error: Option<&str>) {
        self.idle();
        let error = error.map_or(Value::Null, |message| json!({ "message": message }));
        self.notify("turn/completed", &json!({ "turn": { "id": turn, "status": status, "error": error } }));
    }
}

fn account() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

fn limited() -> bool {
    account().is_some_and(|dir| dir.join(LIMITED).is_file())
}

fn input_text(params: &Value) -> String {
    params["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn(shared: &Shared, id: &Value, text: &str) {
    let number = shared.turns.fetch_add(1, Ordering::Relaxed) + 1;
    let turn = format!("turn-{number}");
    if text.contains("lag") {
        std::thread::sleep(LAG);
    }
    if text.contains("stall") {
        std::thread::sleep(STALL);
    }
    shared.respond(id, &json!({ "turn": { "id": turn } }));
    shared.notify("turn/started", &json!({ "turn": { "id": turn } }));
    let user =
        json!({ "id": format!("user-{number}"), "type": "userMessage", "content": [{ "type": "text", "text": text }] });
    shared.notify("item/started", &json!({ "item": user }));
    shared.notify("item/completed", &json!({ "item": user }));
    if text.contains("slow") {
        let deadline = Instant::now() + SLOW;
        while Instant::now() < deadline && !shared.interrupted.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    if shared.interrupted.swap(false, Ordering::Relaxed) {
        shared.completed(&turn, "interrupted", None);
        return;
    }
    if text.contains("exit") {
        std::process::exit(1);
    }
    if text.contains("limit") && limited() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        shared.notify(
            "account/rateLimits/updated",
            &json!({ "rateLimits": { "primary": { "usedPercent": 100.0, "windowDurationMins": 300, "resetsAt": now + RESET_SECONDS } } }),
        );
        let error = json!({ "message": LIMIT_WORDS, "codexErrorInfo": "usageLimitExceeded" });
        shared.notify("error", &json!({ "error": error, "willRetry": false }));
        shared.idle();
        shared.notify("turn/completed", &json!({ "turn": { "id": turn, "status": "failed", "error": error } }));
        return;
    }
    if text.contains("quota-error") {
        shared.notify("error", &json!({ "error": { "message": STOP_WORDS }, "willRetry": false }));
        shared.completed(&turn, "failed", Some(STOP_WORDS));
        return;
    }
    if text.contains("quota-fail") {
        shared.completed(&turn, "failed", Some(STOP_WORDS));
        return;
    }
    let mut reply = format!("reply to {text}");
    if text.contains("skill") {
        let Some(decision) = shared.ask(&turn, number) else {
            shared.interrupted.store(false, Ordering::Relaxed);
            shared.completed(&turn, "interrupted", None);
            return;
        };
        reply = format!("{reply} after {decision}");
    }
    reply.push_str(&shared.trust.lock().unwrap_or_else(PoisonError::into_inner));
    let agent = format!("agent-{number}");
    shared.notify("item/started", &json!({ "item": { "id": agent, "type": "agentMessage" } }));
    let reply = json!({ "id": agent, "type": "agentMessage", "text": reply });
    shared.notify("item/completed", &json!({ "item": reply }));
    shared.remember(&json!({ "id": turn, "items": [user, reply] }));
    shared.completed(&turn, "completed", None);
}

fn pause(variable: &str) {
    let delay = std::env::var(variable).ok().and_then(|ms| ms.parse().ok()).unwrap_or(0);
    std::thread::sleep(Duration::from_millis(delay));
}

fn at_home() -> bool {
    let home = std::env::var_os("HOME").and_then(|home| PathBuf::from(home).canonicalize().ok());
    home.is_some() && home == std::env::current_dir().ok()
}

fn request(shared: &Arc<Shared>, id: &Value, method: &str, params: &Value) {
    if matches!(method, "thread/start" | "thread/resume") {
        let projects = params["config"]["projects"].as_object().cloned().unwrap_or_default();
        let levels = projects
            .iter()
            .map(|(folder, project)| format!(" [{} {folder}]", project["trust_level"].as_str().unwrap_or_default()));
        *shared.trust.lock().unwrap_or_else(PoisonError::into_inner) = levels.collect();
    }
    match method {
        "initialize" if !at_home() => shared.refuse(id, "fake codex runs outside the home folder"),
        "model/list" => shared.respond(
            id,
            &json!({ "data": [{
                "id": "fake-mini",
                "displayName": "Fake mini",
                "supportedReasoningEfforts": [{ "reasoningEffort": "low" }, { "reasoningEffort": "medium" }],
            }] }),
        ),
        "thread/start" => {
            pause(START_DELAY);
            let thread = format!("fake-thread-{}", std::process::id());
            shared.respond(id, &shared.thread(&json!(thread)));
        }
        "thread/resume" => {
            pause(RESUME_DELAY);
            shared.respond(id, &shared.thread(&params["threadId"]));
        }
        "thread/read" => shared.respond(id, &shared.thread(&params["threadId"])),
        "thread/compact/start" => {
            shared.compacted.store(true, Ordering::Relaxed);
            shared.respond(id, &json!({}));
        }
        "turn/start" => {
            let mut text = input_text(params);
            if shared.compacted.swap(false, Ordering::Relaxed) {
                text.push_str(" after compact");
            }
            if text.contains("crash") {
                std::process::exit(1);
            }
            if text.contains("reject") {
                shared.refuse(id, "fake codex rejected this turn");
                return;
            }
            {
                let mut busy = shared.busy.lock().unwrap_or_else(PoisonError::into_inner);
                if *busy {
                    drop(busy);
                    shared.refuse(id, "fake codex already has a turn running");
                    return;
                }
                *busy = true;
            }
            let shared = Arc::clone(shared);
            let id = id.clone();
            std::thread::spawn(move || turn(&shared, &id, &text));
        }
        "turn/interrupt" => {
            shared.interrupted.store(true, Ordering::Relaxed);
            shared.respond(id, &json!({}));
        }
        _ => shared.respond(id, &json!({})),
    }
}

fn main() {
    let shared = Arc::new(Shared::default());
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
        match (frame.get("id"), frame.get("method").and_then(Value::as_str)) {
            (Some(id), Some(method)) => request(&shared, id, method, frame.get("params").unwrap_or(&Value::Null)),
            (Some(id), None) => shared.answered(id, frame.get("result").unwrap_or(&Value::Null)),
            _ => {}
        }
    }
}
