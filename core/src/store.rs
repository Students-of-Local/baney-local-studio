use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::model::{AccessMode, Delivery, Harness, ImageAttachment, ModelKey, PromptDraft};

const VERSION: u32 = 1;
const FILE: &str = "state.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredProject {
    pub(crate) path: String,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) icon: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ThreadFlags {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "pinnedAt")]
    pub(crate) pin_order: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) settled_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unsettled_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snoozed_until: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) woke_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) done_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) activity_at: Option<i64>,
    pub(crate) archived: bool,
    pub(crate) unread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) access: Option<AccessMode>,
    pub(crate) additions: u32,
    pub(crate) deletions: u32,
    pub(crate) auto_settle_disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fan_out: Option<String>,
    #[serde(skip_serializing_if = "is_eukhe")]
    pub(crate) harness: Harness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fork_of: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) forked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) local_only: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) skills_done: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) account: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) project_refused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) caller: Option<crate::plugins::Caller>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) held: Vec<HeldPrompt>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct HeldPrompt {
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) images: Vec<HeldImage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) files: Vec<String>,
    pub(crate) steer: Option<bool>,
    #[serde(skip)]
    pub(crate) request: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct HeldImage {
    pub(crate) mime_type: String,
    pub(crate) data: String,
}

impl HeldPrompt {
    pub(crate) fn new(prompt: &PromptDraft, delivery: Delivery, request: Option<String>) -> Self {
        Self {
            text: prompt.text.clone(),
            images: prompt
                .images
                .iter()
                .map(|image| HeldImage {
                    mime_type: image.mime_type.clone(),
                    data: crate::images::encode_base64(&image.data),
                })
                .collect(),
            files: prompt.files.clone(),
            steer: match delivery {
                Delivery::Steer => Some(true),
                Delivery::Queue => Some(false),
                Delivery::Default => None,
            },
            request,
        }
    }

    pub(crate) fn prompt(&self) -> PromptDraft {
        PromptDraft {
            text: self.text.clone(),
            images: self
                .images
                .iter()
                .filter_map(|image| {
                    Some(ImageAttachment {
                        mime_type: image.mime_type.clone(),
                        data: crate::images::decode_base64(&image.data)?,
                    })
                })
                .collect(),
            files: self.files.clone(),
        }
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.steer {
            Some(true) => Delivery::Steer,
            Some(false) => Delivery::Queue,
            None => Delivery::Default,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_eukhe(harness: &Harness) -> bool {
    *harness == Harness::Eukhe
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    version: u32,
    #[serde(default)]
    projects: Vec<StoredProject>,
    #[serde(default)]
    threads: BTreeMap<String, ThreadFlags>,
    #[serde(default)]
    favorites: Vec<ModelKey>,
    #[serde(default)]
    hidden: Vec<ModelKey>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    trusted: BTreeSet<String>,
}

#[derive(Debug)]
pub(crate) struct Store {
    path: PathBuf,
    pub(crate) projects: Vec<StoredProject>,
    pub(crate) threads: BTreeMap<String, ThreadFlags>,
    pub(crate) favorites: Vec<ModelKey>,
    pub(crate) hidden: Vec<ModelKey>,
    pub(crate) trusted: BTreeSet<String>,
}

impl Store {
    pub(crate) fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join(FILE);
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StateFile>(&bytes)
                .with_context(|| format!("read the Local Studio state {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StateFile {
                version: VERSION,
                projects: Vec::new(),
                threads: BTreeMap::new(),
                favorites: Vec::new(),
                hidden: Vec::new(),
                trusted: BTreeSet::new(),
            },
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        if file.version > VERSION {
            anyhow::bail!(
                "{} was written by a newer Local Studio (state version {}, this build reads {VERSION})",
                path.display(),
                file.version
            );
        }
        Ok(Self {
            path,
            projects: file.projects,
            threads: file.threads,
            favorites: file.favorites,
            hidden: file.hidden,
            trusted: file.trusted,
        })
    }

    pub(crate) fn project_id(&self, project: &str) -> Option<String> {
        if let Some(known) = self.projects.iter().find(|known| known.path == project) {
            return Some(known.path.clone());
        }
        let canonical = std::fs::canonicalize(project).ok()?;
        let canonical = canonical.to_string_lossy();
        self.projects.iter().find(|known| known.path == canonical).map(|known| known.path.clone())
    }

    pub(crate) fn flags(&self, thread: &str) -> ThreadFlags {
        self.threads.get(thread).cloned().unwrap_or_default()
    }

    pub(crate) fn activity_at(&self, thread: &str) -> Option<i64> {
        self.threads.get(thread).and_then(|flags| flags.activity_at)
    }

    pub(crate) fn update(&mut self, thread: &str, change: impl FnOnce(&mut ThreadFlags)) {
        let flags = self.threads.entry(thread.to_string()).or_default();
        change(flags);
        if *flags == ThreadFlags::default() {
            self.threads.remove(thread);
        }
    }

    pub(crate) fn save(&self) -> anyhow::Result<()> {
        write(&self.path, &self.bytes()?)
    }

    fn bytes(&self) -> anyhow::Result<Vec<u8>> {
        let file = StateFile {
            version: VERSION,
            projects: self.projects.clone(),
            threads: self.threads.clone(),
            favorites: self.favorites.clone(),
            hidden: self.hidden.clone(),
            trusted: self.trusted.clone(),
        };
        Ok(serde_json::to_vec_pretty(&file)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedThread {
    pub id: String,
    pub title: String,
    pub project: String,
    pub harness: &'static str,
    pub modified: i64,
}

pub fn harness_threads(data_dir: &Path) -> Result<Vec<ListedThread>, String> {
    let store = Store::open(data_dir).map_err(|error| format!("{error:#}"))?;
    let mut listed: Vec<ListedThread> = store
        .threads
        .into_iter()
        .filter(|(_, flags)| flags.harness != Harness::Eukhe && !flags.archived && flags.local_only.is_none())
        .map(|(id, flags)| ListedThread {
            title: flags
                .title
                .map(|title| crate::threads::clean_title(&title))
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| crate::threads::NEW_THREAD_TITLE.to_string()),
            project: flags.project.or(flags.cwd).unwrap_or_default(),
            harness: crate::harness::provider_of(flags.harness),
            modified: flags.activity_at.unwrap_or_default(),
            id,
        })
        .collect();
    listed.sort_by(|left, right| right.modified.cmp(&left.modified).then_with(|| left.id.cmp(&right.id)));
    Ok(listed)
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let directory = path.parent().context("the state file has no directory")?;
    std::fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    let temporary = directory.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let mut handle = std::fs::File::create(&temporary).with_context(|| format!("write {}", temporary.display()))?;
    handle.write_all(bytes)?;
    handle.sync_all()?;
    drop(handle);
    std::fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))
}

pub(crate) struct Saver {
    next: Option<Sender<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl Saver {
    pub(crate) fn new(store: &Store, failed: impl Fn(anyhow::Error) + Send + 'static) -> Self {
        let path = store.path.clone();
        let (next, saves) = std::sync::mpsc::channel::<Vec<u8>>();
        let thread = std::thread::spawn(move || {
            let mut written = Vec::new();
            while let Ok(first) = saves.recv() {
                let bytes = saves.try_iter().last().unwrap_or(first);
                if bytes == written {
                    continue;
                }
                match write(&path, &bytes) {
                    Ok(()) => written = bytes,
                    Err(error) => failed(error),
                }
            }
        });
        Self { next: Some(next), thread: Some(thread) }
    }

    pub(crate) fn save(&self, store: &Store) -> anyhow::Result<()> {
        let bytes = store.bytes()?;
        match &self.next {
            Some(next) => Ok(next.send(bytes)?),
            None => write(&store.path, &bytes),
        }
    }

    pub(crate) fn close(&mut self) {
        self.next = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn data_dir(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(value) = std::env::var_os("LOCAL_STUDIO_DATA_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    default_data_dir()
}

#[cfg(unix)]
fn default_data_dir() -> Result<PathBuf, String> {
    let home = home_dir().ok_or_else(|| "HOME is not set".to_string())?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library").join("Application Support").join("Local Studio"));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".local").join("share"), PathBuf::from);
    Ok(base.join("local-studio"))
}

#[cfg(windows)]
fn default_data_dir() -> Result<PathBuf, String> {
    dot_dir()
}

#[cfg(unix)]
pub(crate) fn dot_dir() -> Result<PathBuf, String> {
    Ok(home_dir().ok_or("HOME is not set")?.join(".local-studio"))
}

#[cfg(windows)]
pub(crate) fn dot_dir() -> Result<PathBuf, String> {
    Ok(crate::platform::local_app_data().ok_or("LOCALAPPDATA is not set")?.join("Local Studio"))
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    crate::platform::home().filter(|value| !value.is_empty()).map(PathBuf::from)
}
