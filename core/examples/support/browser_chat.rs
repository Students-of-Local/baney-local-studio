use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use serde_json::{json, Map, Value};

pub const ID: &str = "browser-globe";

pub fn write(sessions: &Path, work: &Path, turns: usize) -> anyhow::Result<()> {
    let stamp = |at: usize| format!("2026-11-24T12:00:{at:02}.000Z");
    let mut lines = vec![json!({ "type": "session", "version": 3, "id": ID, "timestamp": stamp(0), "cwd": work })];
    for index in 0..turns * 2 {
        let parent = index.checked_sub(1).map(|before| format!("m{before}"));
        let message = if index % 2 == 0 {
            json!({ "role": "user", "content": [{ "type": "text", "text": format!("globe question {index}") }], "timestamp": 0 })
        } else {
            json!({ "role": "assistant", "content": [{ "type": "text", "text": format!("globe answer {index}") }],
                "api": "openai-completions", "provider": "panel", "model": "panel", "stopReason": "stop", "timestamp": 0,
                "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } } })
        };
        lines.push(json!({ "type": "message", "id": format!("m{index}"), "parentId": parent, "timestamp": stamp(index + 1), "message": message }));
    }
    let lines: Vec<String> = lines.iter().map(Value::to_string).collect();
    let whole = sessions.with_file_name("browser-chat.part");
    std::fs::write(&whole, lines.join("\n") + "\n")?;
    std::fs::rename(&whole, sessions.join(format!("{ID}.jsonl")))?;
    Ok(())
}

pub fn rewrite(socket: &Path, sessions: &Path, work: &Path, turns: usize) -> anyhow::Result<()> {
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
        let rows = list.as_array().or_else(|| list["sessions"].as_array()).context("list")?;
        let row = rows
            .iter()
            .map(|row| row.get("summary").unwrap_or(row))
            .find(|row| row["sessionId"] == ID)
            .with_context(|| format!("{ID} is not live: {list}"))?;
        let active = row.get("activeSessionId").or_else(|| row.get("id")).and_then(Value::as_str).context("active")?;
        let path = row["sessionFile"].as_str().context("session file")?.to_string();
        let kill = DaemonCommand::Kill { id: None, active_session_id: active.to_string(), rest: Map::default() };
        client.request_ok(kill).await?;
        let delete = DaemonCommand::DeleteSavedSession {
            id: None,
            active_session_id: None,
            session_path: path,
            rest: Map::default(),
        };
        client.request_ok(delete).await?;
        anyhow::Ok(())
    })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while sessions.join(ID).exists() {
        ensure!(Instant::now() < deadline, "{ID} is still in {}", sessions.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    write(sessions, work, turns)
}
