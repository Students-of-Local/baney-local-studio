use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use eukhe_client::daemon_client::DaemonClient;
use eukhe_types::daemon::DaemonCommand;
use serde_json::{json, Map, Value};

const PROBE: &str = "smoke-probe";
const BUSY: &str = "while the agent is running";
const HELD: &str = "owned by another client";

pub fn held(socket: &Path, thread: &str) -> anyhow::Result<bool> {
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
        let sessions = list.as_array().or_else(|| list.get("sessions").and_then(Value::as_array)).context("list")?;
        let active = sessions
            .iter()
            .map(|session| session.get("summary").unwrap_or(session))
            .find(|summary| summary.get("sessionId").and_then(Value::as_str) == Some(thread))
            .and_then(|summary| summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str))
            .with_context(|| format!("thread {thread} is not a live session: {list}"))?;
        let replace = |servers: Value| DaemonCommand::ReplaceAcpMcpServers {
            id: None,
            active_session_id: active.to_string(),
            owner_id: PROBE.to_string(),
            servers,
            rest: Map::default(),
        };
        let probe = json!([{ "type": "stdio", "name": PROBE, "command": "/usr/bin/true", "cwd": "/" }]);
        for _ in 0..50 {
            match client.request_ok(replace(probe.clone())).await.map_err(|error| format!("{error:#}")) {
                Ok(_) => return client.request_ok(replace(json!([]))).await.map(|_| false),
                Err(error) if error.contains(HELD) => return Ok(true),
                Err(error) if error.contains(BUSY) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(error) => bail!("the session's server list could not be probed: {error}"),
            }
        }
        bail!("the session stayed busy for 10 s")
    })
}
