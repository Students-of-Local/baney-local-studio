use std::collections::BTreeMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Harness;
use crate::platform::Mode;

const FILE: &str = "accounts.json";
#[cfg(target_os = "macos")]
const CLAUDE_SIGNED_IN: &str = ".claude.json";
#[cfg(not(target_os = "macos"))]
const CLAUDE_SIGNED_IN: &str = ".credentials.json";
pub(crate) const FALLBACK_PARK_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ParkFile {
    parked: BTreeMap<String, u64>,
}

pub(crate) struct Parks {
    path: PathBuf,
    parked: BTreeMap<String, u64>,
}

impl Parks {
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("harness").join(FILE);
        let file: ParkFile =
            std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let now = super::now_ms();
        let parked = file.parked.into_iter().filter(|(_, until)| *until > now).collect();
        Self { path, parked }
    }

    pub(crate) fn until(&self, dir: &Path) -> Option<u64> {
        let now = super::now_ms();
        self.parked.get(&key(dir)).copied().filter(|until| *until > now)
    }

    pub(crate) fn park(&mut self, dir: &Path, until: u64) -> Result<(), String> {
        self.parked.insert(key(dir), until);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let directory = self.path.parent().ok_or_else(|| "the accounts file has no folder".to_string())?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|error| format!("create {}: {error}", directory.display()))?;
        crate::platform::set_mode(directory, 0o700).map_err(|error| format!("{}: {error}", directory.display()))?;
        let bytes =
            serde_json::to_vec_pretty(&ParkFile { parked: self.parked.clone() }).map_err(|error| error.to_string())?;
        let temporary = directory.join(format!(".{FILE}.{}.tmp", std::process::id()));
        std::fs::remove_file(&temporary).ok();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.write_all(&bytes).map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.sync_all().ok();
        drop(file);
        std::fs::rename(&temporary, &self.path).map_err(|error| {
            std::fs::remove_file(&temporary).ok();
            format!("replace {}: {error}", self.path.display())
        })
    }
}

fn key(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

fn names(harness: Harness) -> Option<(&'static str, &'static str, &'static str)> {
    match harness {
        Harness::ClaudeCode => Some(("CLAUDE_CONFIG_DIR", ".claude", CLAUDE_SIGNED_IN)),
        Harness::Codex => Some(("CODEX_HOME", ".codex", "auth.json")),
        Harness::Eukhe => None,
    }
}

pub(crate) fn default_dir(harness: Harness) -> Option<PathBuf> {
    let (variable, base, _) = names(harness)?;
    std::env::var_os(variable)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::platform::home().map(|home| PathBuf::from(home).join(base)))
}

pub(crate) fn dir_of(harness: Harness, account: Option<&str>) -> Option<PathBuf> {
    account.map(PathBuf::from).or_else(|| default_dir(harness))
}

pub(crate) fn stored(harness: Harness, dir: &Path) -> Option<String> {
    (default_dir(harness).as_deref() != Some(dir)).then(|| key(dir))
}

pub(crate) fn label(dir: &Path) -> String {
    dir.file_name().map_or_else(|| key(dir), |name| name.to_string_lossy().into_owned())
}

fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), crate::platform::home()) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

pub(crate) fn list(harness: Harness, overrides: &[String]) -> Vec<PathBuf> {
    let mut accounts: Vec<PathBuf> = Vec::new();
    let mut add = |dir: PathBuf| {
        if !accounts.contains(&dir) {
            accounts.push(dir);
        }
    };
    let listed: Vec<PathBuf> =
        overrides.iter().map(|path| path.trim()).filter(|path| !path.is_empty()).map(expand).collect();
    if !listed.is_empty() {
        listed.into_iter().for_each(&mut add);
        return accounts;
    }
    let Some((_, base, marker)) = names(harness) else { return accounts };
    if let Some(dir) = default_dir(harness) {
        add(dir);
    }
    let Some(home) = crate::platform::home().map(PathBuf::from) else { return accounts };
    let prefix = format!("{base}-");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .filter(|dir| dir.is_dir() && dir.join(marker).exists())
        .collect();
    found.sort();
    found.into_iter().for_each(add);
    accounts
}

pub(crate) fn carry(harness: Harness, from: &Path, to: &Path, cwd: &str, native: &str) -> Result<bool, String> {
    let source = match harness {
        Harness::ClaudeCode => super::claude::session_file(Some(from), cwd, native),
        Harness::Codex => rollout(&from.join("sessions"), native),
        Harness::Eukhe => None,
    };
    let Some(source) = source else { return Ok(false) };
    let relative = source.strip_prefix(from).map_err(|error| error.to_string())?;
    let target = match harness {
        Harness::Codex => rollout(&to.join("sessions"), native),
        Harness::ClaudeCode | Harness::Eukhe => None,
    }
    .unwrap_or_else(|| to.join(relative));
    let parent = target.parent().ok_or_else(|| format!("{} has no folder", target.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let name = target.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::remove_file(&temporary).ok();
    let copied = (|| {
        let mut input = std::fs::File::open(&source)?;
        let mut output = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temporary, &target)
    })();
    copied.map(|()| true).map_err(|error| {
        std::fs::remove_file(&temporary).ok();
        format!("copy {} to {}: {error}", source.display(), target.display())
    })
}

pub(crate) fn rollout(root: &Path, thread: &str) -> Option<PathBuf> {
    if thread.is_empty() {
        return None;
    }
    let suffix = format!("{thread}.jsonl");
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok) {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_file() && name.starts_with("rollout-") && name.ends_with(&suffix) {
                return Some(path);
            }
        }
    }
    None
}
