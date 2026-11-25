use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[path = "support/mcp.rs"]
mod mcp;
#[path = "support/skill.rs"]
mod skill;

const ALLOWED: &str = "mcp__local-studio-skills__skill_search,mcp__local-studio-skills__skill_read";
const SLOW: Duration = Duration::from_millis(1500);
const LIMITED: &str = "limited";
const RESET_SECONDS: u64 = 2 * 60 * 60;

fn emit(frame: &Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{frame}").ok();
    out.flush().ok();
}

fn answer_control(frame: &Value) {
    emit(&json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": frame["request_id"], "response": {} },
    }));
}

fn interrupt(frame: &Value) -> bool {
    frame["type"] == "control_request" && frame["request"]["subtype"] == "interrupt"
}

fn account() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

fn limited() -> bool {
    account().is_some_and(|dir| dir.join(LIMITED).is_file())
}

fn session_file(args: &[String]) -> Option<(PathBuf, Option<PathBuf>)> {
    let named = |flag: &str| args.windows(2).find(|pair| pair[0] == flag).map(|pair| pair[1].clone());
    let root = account()?;
    let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
    let escaped: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let file = |session: String| root.join("projects").join(&escaped).join(format!("{session}.jsonl"));
    let own = named("--session-id").or_else(|| named("--resume"))?;
    Some((file(own), named("--resume").map(file).filter(|_| args.iter().any(|arg| arg == "--fork-session"))))
}

fn remember(session: Option<&(PathBuf, Option<PathBuf>)>, lines: &[Value]) {
    let Some((file, origin)) = session else { return };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Some(origin) = origin.as_ref().filter(|_| !file.exists()) {
        std::fs::copy(origin, file).ok();
    }
    let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(file) else { return };
    for line in lines {
        writeln!(out, "{line}").ok();
    }
}

fn ask(inbox: &Receiver<Value>, waiting: &mut VecDeque<Value>, turn: u32) -> Option<String> {
    let id = format!("fake-ask-{turn}");
    let color = json!({ "question": "Which color?", "header": "Color", "multiSelect": false,
        "options": [{ "label": "Red", "description": "warm" }, { "label": "Blue", "description": "cool" }] });
    let size = json!({ "question": "Which size?", "header": "Size", "multiSelect": true,
        "options": [{ "label": "Small", "description": "" }, { "label": "Large", "description": "" }] });
    let input = json!({ "questions": [color, size] });
    emit(&json!({ "type": "control_request", "request_id": id,
        "request": { "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": input } }));
    loop {
        let next = inbox.recv().unwrap_or_else(|_| std::process::exit(0));
        if interrupt(&next) {
            answer_control(&next);
            return None;
        }
        if next["type"] == "control_response" && next["response"]["request_id"] == id {
            let response = &next["response"]["response"];
            if response["behavior"] != "allow" {
                return Some("question denied".to_string());
            }
            let answers = &response["updatedInput"]["answers"];
            let picked = |key: &str| answers[key].as_str().unwrap_or("nothing").to_string();
            return Some(format!("picked {} and {}", picked("Which color?"), picked("Which size?")));
        }
        if next["type"] == "control_request" {
            answer_control(&next);
        } else {
            waiting.push_back(next);
        }
    }
}

fn run(
    turn: u32,
    frame: &Value,
    (effort, sources, servers): (&str, &str, Option<&Value>),
    (inbox, session): (&Receiver<Value>, Option<&(PathBuf, Option<PathBuf>)>),
    waiting: &mut VecDeque<Value>,
) {
    let text = frame["message"]["content"].as_str().unwrap_or_default().to_string();
    emit(&json!({ "type": "system", "subtype": "init", "model": "fake-claude" }));
    emit(&json!({ "type": "user", "message": { "role": "user", "content": text }, "isReplay": true }));
    let mut aborted = false;
    let mut asked = None;
    if text.contains("questions") {
        asked = ask(inbox, waiting, turn);
        aborted = asked.is_none();
    }
    if text.contains("slow") {
        let deadline = Instant::now() + SLOW;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match inbox.recv_timeout(left) {
                Ok(next) if interrupt(&next) => {
                    answer_control(&next);
                    if text.contains("discard") {
                        waiting.clear();
                    }
                    aborted = true;
                    break;
                }
                Ok(next) if next["type"] == "control_request" => answer_control(&next),
                Ok(next) => waiting.push_back(next),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => std::process::exit(0),
            }
        }
    }
    if aborted {
        emit(
            &json!({ "type": "result", "subtype": "error_during_execution", "is_error": false, "terminal_reason": "aborted_streaming" }),
        );
        return;
    }
    if text.contains("limit-text") && limited() {
        emit(
            &json!({ "type": "result", "subtype": "success", "is_error": true, "result": "You've hit your limit \u{b7} resets 3pm" }),
        );
        return;
    }
    if text.contains("limit") && limited() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        emit(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "status": "rejected", "resetsAt": now + RESET_SECONDS, "rateLimitType": "five_hour" },
        }));
        emit(
            &json!({ "type": "result", "subtype": "success", "is_error": true, "result": "You've hit your limit, resets later" }),
        );
        return;
    }
    if text.contains("quota") {
        emit(
            &json!({ "type": "result", "subtype": "success", "is_error": true, "result": "You've hit your usage limit, resets 5pm" }),
        );
        return;
    }
    let mut reply =
        skill::reply(&text).unwrap_or_else(|| format!("reply to {text} at effort {effort} with {sources} settings"));
    if let Some(asked) = asked {
        reply = format!("{reply}: {asked}");
    }
    if text.contains("look up") {
        let found = servers.and_then(|servers| mcp::skills(&servers["mcpServers"]["local-studio-skills"], &text));
        reply = format!("{reply}, found {}", found.as_deref().unwrap_or("no server"));
    }
    let assistant = json!({
        "type": "assistant",
        "message": {
            "id": format!("msg-{}-{turn}", std::process::id()),
            "role": "assistant",
            "model": "fake-claude",
            "content": [{ "type": "text", "text": reply }],
        },
    });
    emit(&assistant);
    remember(session, &[json!({ "type": "user", "message": { "role": "user", "content": text } }), assistant]);
    emit(&json!({ "type": "result", "subtype": "success", "is_error": false, "result": reply }));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let effort =
        args.windows(2).find(|pair| pair[0] == "--effort").map_or_else(|| "none".to_string(), |pair| pair[1].clone());
    let sources = args
        .windows(2)
        .find(|pair| pair[0] == "--setting-sources")
        .map_or_else(|| "all".to_string(), |pair| pair[1].clone());
    let servers = args.windows(2).find(|pair| pair[0] == "--mcp-config").and_then(|pair| {
        let allowed = args.iter().any(|arg| arg == ALLOWED);
        allowed.then(|| serde_json::from_str::<Value>(&pair[1]).ok()).flatten()
    });
    let session = session_file(&args);
    let (frames, inbox) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(frame) = serde_json::from_str::<Value>(&line) {
                if frames.send(frame).is_err() {
                    break;
                }
            }
        }
    });
    let mut waiting = VecDeque::new();
    let mut turn = 0;
    loop {
        let frame = match waiting.pop_front() {
            Some(frame) => frame,
            None => match inbox.recv() {
                Ok(frame) => frame,
                Err(_) => return,
            },
        };
        match frame["type"].as_str() {
            Some("user") => {
                turn += 1;
                run(turn, &frame, (&effort, &sources, servers.as_ref()), (&inbox, session.as_ref()), &mut waiting);
            }
            Some("control_request") => answer_control(&frame),
            _ => {}
        }
    }
}
