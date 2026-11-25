use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

pub fn skills(server: &Value, words: &str) -> Option<String> {
    let args = server["args"].as_array()?;
    args.iter().find(|arg| *arg == "--plugins")?;
    let mut command = Command::new(server["command"].as_str()?);
    command.env_remove("LOCAL_STUDIO_DATA_DIR");
    command.args(args.iter().filter_map(Value::as_str));
    command.envs(server["env"].as_object()?.iter().filter_map(|(name, value)| Some((name, value.as_str()?))));
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let client = json!({ "name": "fake", "version": "1" });
    let opened = json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": client });
    let asked = json!({ "name": "skill_search", "arguments": { "words": words } });
    for frame in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": opened }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": asked }),
    ] {
        writeln!(stdin, "{frame}").ok()?;
    }
    let answer = BufReader::new(child.stdout.take()?)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|frame| frame["id"] == 2)?;
    drop(stdin);
    child.wait().ok()?;
    Some(answer["result"]["content"][0]["text"].as_str()?.lines().next()?.to_string())
}
