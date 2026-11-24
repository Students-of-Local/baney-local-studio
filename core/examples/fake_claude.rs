use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

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

fn session_file(args: &[String]) -> Option<PathBuf> {
    let session = args.windows(2).find(|pair| pair[0] == "--session-id" || pair[0] == "--resume")?[1].clone();
    let root = account()?;
    let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
    let escaped: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    Some(root.join("projects").join(escaped).join(format!("{session}.jsonl")))
}

fn remember(file: Option<&PathBuf>, lines: &[Value]) {
    let Some(file) = file else { return };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(file) else { return };
    for line in lines {
        writeln!(out, "{line}").ok();
    }
}

fn run(
    turn: u32,
    frame: &Value,
    (effort, sources): (&str, &str),
    (inbox, file): (&Receiver<Value>, Option<&PathBuf>),
    waiting: &mut VecDeque<Value>,
) {
    let text = frame["message"]["content"].as_str().unwrap_or_default().to_string();
    emit(&json!({ "type": "system", "subtype": "init", "model": "fake-claude" }));
    emit(&json!({ "type": "user", "message": { "role": "user", "content": text }, "isReplay": true }));
    let mut aborted = false;
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
    let reply = format!("reply to {text} at effort {effort} with {sources} settings");
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
    remember(file, &[json!({ "type": "user", "message": { "role": "user", "content": text } }), assistant]);
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
    let file = session_file(&args);
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
                run(turn, &frame, (&effort, &sources), (&inbox, file.as_ref()), &mut waiting);
            }
            Some("control_request") => answer_control(&frame),
            _ => {}
        }
    }
}
