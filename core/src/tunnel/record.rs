use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::setup::{serve_disallow, WRANGLER, YML};
use super::{process_info, RelayState, Status, Tunnel};
use crate::platform::mode;

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

    fn relay_chmod(&self) -> Option<String> {
        let path = self.relay_path();
        let open = fs::metadata(&path).is_ok_and(|meta| mode(&meta) & 0o077 != 0);
        open.then(|| format!("chmod 600 '{}'", path.display().to_string().replace('\'', "'\\''")))
    }

    pub(super) fn relay_record(&self) -> Result<Option<Record>, String> {
        if let Some(chmod) = self.relay_chmod() {
            let path = self.relay_path();
            let path = path.display();
            return Err(format!(
                "Others on this machine can read the relay's key in {path}, so it isn't used. Run {chmod}, then set the relay up again for a new key: {AGAIN}"
            ));
        }
        self.relay_read()
    }

    fn relay_read(&self) -> Result<Option<Record>, String> {
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
        let record = self.relay_record();
        if self.dir.join(YML).exists() && !matches!(&record, Ok(Some(record)) if record.host() == route.hostname) {
            return Ok(None);
        }
        match record? {
            Some(record) if record.host() != route.hostname => {
                let (path, host) = (self.relay_path(), route.hostname);
                Err(format!("{} isn't for {host}. Run {OFF}, then set the relay up again: {AGAIN}", path.display()))
            }
            record => Ok(record),
        }
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
                status.next_command = Some(self.relay_chmod().unwrap_or(OFF.into()));
                status.relay = Some(RelayState { state: "bad_config".into(), since: None });
                return status;
            }
        }
        let seen = self.relay_seen();
        let alive = |pid| process_info(pid).is_some_and(|(stat, _)| !stat.starts_with('Z'));
        let seen = seen.filter(|seen| seen.state == "stopped" || alive(seen.pid));
        let since = seen.as_ref().map(|seen| seen.at);
        let state = seen.map_or("stopped".into(), |seen| seen.state);
        let (next, message) = match state.as_str() {
            "connected" => ("ready", "This machine is connected to its relay."),
            "connecting" => ("waiting", "This machine is connecting to its relay."),
            "disconnected" => ("waiting", "This machine lost its relay and keeps trying to reach it again."),
            "replaced" => ("needs_setup", "Another machine took this relay over. To take it back, run setup again:"),
            "unauthorized" => (
                "needs_setup",
                "The relay refuses this machine's key, and this machine asks again every minute. If that doesn't end, run setup again:",
            ),
            "outdated" => {
                ("needs_setup", "The relay speaks a newer protocol. Update Local Studio, then run setup again:")
            }
            "bad_config" => ("needs_setup", "The relay program can't use its settings. Run setup again:"),
            _ => ("needs_setup", "The relay program isn't running on this machine. Run setup again:"),
        };
        let command = match next {
            "ready" => None,
            "waiting" => Some(CHECK),
            _ => Some(AGAIN),
        };
        status.message = match next {
            "ready" => format!("{} {message}", status.message),
            "waiting" => message.into(),
            _ => format!("{message} {AGAIN}"),
        };
        status.next = next.into();
        status.next_command = command.map(ToString::to_string);
        status.relay = Some(RelayState { state, since });
        status
    }

    pub(super) fn remove_relay(&self) -> Result<Vec<String>, String> {
        let mut removed = Vec::new();
        for name in [FOLDER, STATE, RECORD] {
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

    pub(super) fn relay_off(
        &self,
        taken: bool,
        removed: &mut Vec<String>,
        left: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let record = self.relay_read();
        if let Ok(Some(record)) = &record {
            serve_disallow(record.host(), left);
        }
        removed.extend(self.remove_relay()?);
        Ok(match record {
            Ok(None) => None,
            Ok(Some(Record { name, account_id, .. })) if taken => {
                Some(format!("Another machine uses the Worker {name} on your Cloudflare account {account_id}, so it stays."))
            }
            Ok(Some(Record { name, account_id, .. })) => Some(format!(
                "The Worker {name} stays on your Cloudflare account. To delete it: CLOUDFLARE_ACCOUNT_ID={account_id} npx --yes {WRANGLER} delete --name {name}"
            )),
            Err(_) => Some("relay.json couldn't be read, so a relay Worker may stay on your Cloudflare account.".into()),
        })
    }
}
