use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SLOW: Duration = Duration::from_millis(1500);
const LAG: Duration = Duration::from_millis(1500);
const STOP_WORDS: &str = "You've hit your usage limit.";
const SKILL_READ: &str = "cat ~/.codex/skills/writing/SKILL.md";

#[derive(Default)]
struct Shared {
    out: Mutex<()>,
    busy: Mutex<bool>,
    interrupted: AtomicBool,
    turns: AtomicU64,
    answers: Mutex<Vec<(Value, Sender<String>)>>,
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

    fn completed(&self, turn: &str, status: &str, error: Option<&str>) {
        self.idle();
        let error = error.map_or(Value::Null, |message| json!({ "message": message }));
        self.notify("turn/completed", &json!({ "turn": { "id": turn, "status": status, "error": error } }));
    }
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
    let agent = format!("agent-{number}");
    shared.notify("item/started", &json!({ "item": { "id": agent, "type": "agentMessage" } }));
    shared.notify("item/completed", &json!({ "item": { "id": agent, "type": "agentMessage", "text": reply } }));
    shared.completed(&turn, "completed", None);
}

fn request(shared: &Arc<Shared>, id: &Value, method: &str, params: &Value) {
    match method {
        "model/list" => shared.respond(
            id,
            &json!({ "data": [{
                "id": "fake-mini",
                "displayName": "Fake mini",
                "supportedReasoningEfforts": [{ "reasoningEffort": "low" }, { "reasoningEffort": "medium" }],
            }] }),
        ),
        "thread/start" => {
            shared.respond(id, &json!({ "thread": { "id": format!("fake-thread-{}", std::process::id()) } }));
        }
        "thread/resume" => shared.respond(id, &json!({ "thread": { "id": params["threadId"], "turns": [] } })),
        "turn/start" => {
            let text = input_text(params);
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
