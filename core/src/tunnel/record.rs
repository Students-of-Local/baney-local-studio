use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::setup::serve_disallow;
use super::{process_info, RelayState, Status, Tunnel};

pub(super) const RECORD: &str = "relay.json";
pub(super) const STATE: &str = "relay-state.json";
pub(super) const FOLDER: &str = "relay";
pub(super) const AGAIN: &str = "local-studio tunnel setup --relay";
const OFF: &str = "local-studio tunnel off";
const CHECK: &str = "local-studio tunnel status --json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Record {
    pub(super) v: u32,
    pub(super) url: String,
    pub(super) name: String,
    pub(super) account_id: String,
    pub(super) key: String,
}

impl Record {
    pub(super) fn host(&self) -> &str {
        self.url.strip_prefix("https://").unwrap_or(&self.url)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Seen {
    pub(super) state: String,
    pub(super) at: i64,
    pub(super) pid: u32,
}

impl Tunnel {
    pub(super) fn relay_path(&self) -> PathBuf {
        self.dir.join(RECORD)
    }

    pub(super) fn relay_record(&self) -> Result<Option<Record>, String> {
        let path = self.relay_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let bad = || {
            let path = path.display();
            format!(
                "{path} isn't a relay record Local Studio can read. Run {OFF}, then set the relay up again: {AGAIN}"
            )
        };
        serde_json::from_slice(&bytes).ok().filter(|record: &Record| record.v == 1).map(Some).ok_or_else(bad)
    }

    pub(super) fn on_relay(&self) -> Result<Option<Record>, String> {
        let ours = self.route()?.filter(|route| route.owner == "local-studio" && route.route == "cloudflare");
        let Some(route) = ours else { return Ok(None) };
        Ok(self.relay_record()?.filter(|record| record.host() == route.hostname))
    }

    pub(super) fn relay_seen(&self) -> Option<Seen> {
        serde_json::from_slice(&fs::read(self.dir.join(STATE)).ok()?).ok()
    }

    pub(super) fn relay_status(&self, mut status: Status) -> Status {
        match self.on_relay() {
            Ok(None) => return status,
            Ok(Some(_)) => {}
            Err(message) => {
                status.message = message;
                status.next = "needs_setup".into();
                status.next_command = Some(OFF.into());
                status.relay = Some(RelayState { state: "bad_config".into(), since: None });
                return status;
            }
        }
        let seen = self.relay_seen();
        let alive = |pid| process_info(pid).is_some_and(|(stat, _)| !stat.starts_with('Z'));
        let since = seen.as_ref().map(|seen| seen.at);
        let state = seen.filter(|seen| alive(seen.pid)).map_or("stopped".into(), |seen| seen.state);
        let (next, message) = match state.as_str() {
            "connected" => ("ready", None),
            "connecting" => ("waiting", Some("This machine is connecting to its relay.")),
            "disconnected" => ("waiting", Some("This machine lost its relay and keeps trying to reach it again.")),
            "replaced" => {
                ("needs_setup", Some("Another machine took this relay over. To take it back, run setup again:"))
            }
            "unauthorized" => ("needs_setup", Some("The relay refuses this machine's key. Run setup again:")),
            "outdated" => {
                ("needs_setup", Some("The relay speaks a newer protocol. Update Local Studio, then run setup again:"))
            }
            "bad_config" => ("needs_setup", Some("The relay program can't use its settings. Run setup again:")),
            _ => ("needs_setup", Some("The relay program isn't running on this machine. Run setup again:")),
        };
        let command = match next {
            "ready" => None,
            "waiting" => Some(CHECK),
            _ => Some(AGAIN),
        };
        if let Some(message) = message {
            status.message = if next == "waiting" { message.into() } else { format!("{message} {AGAIN}") };
        }
        status.next = next.into();
        status.next_command = command.map(ToString::to_string);
        status.relay = Some(RelayState { state, since });
        status
    }

    pub(super) fn remove_relay(&self) -> Result<Vec<String>, String> {
        let mut removed = Vec::new();
        for name in [RECORD, STATE, FOLDER] {
            let path = self.dir.join(name);
            let gone = if name == FOLDER { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            match gone {
                Ok(()) => removed.push(name.to_string()),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        }
        Ok(removed)
    }

    pub(super) fn relay_off(&self, removed: &mut Vec<String>) -> Result<Option<String>, String> {
        let record = self.relay_record().ok().flatten();
        if let Some(record) = &record {
            serve_disallow(record.host());
        }
        removed.extend(self.remove_relay()?);
        Ok(record.map(|record| record.name))
    }
}
