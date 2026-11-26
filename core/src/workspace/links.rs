use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::model::PullRequest;
use super::{git, process, Workspace};
use crate::threads;

const FRESH: Duration = Duration::from_secs(120);
const BACKOFF: Duration = Duration::from_secs(600);

#[derive(Default)]
struct Repo {
    fetched: Option<Instant>,
    failed: Option<Instant>,
    asked: bool,
    pulls: HashMap<String, PullRequest>,
    flight: Arc<tokio::sync::Mutex<()>>,
}

impl Repo {
    fn due(&self) -> bool {
        self.fetched.is_none_or(|at| at.elapsed() > scaled(FRESH))
            && self.failed.is_none_or(|at| at.elapsed() > scaled(BACKOFF))
    }
}

pub(crate) struct Links {
    repos: Mutex<HashMap<PathBuf, Repo>>,
    changed: watch::Sender<u64>,
    owner_watching: AtomicBool,
    remote_clients: AtomicUsize,
}

impl Default for Links {
    fn default() -> Self {
        Self {
            repos: Mutex::new(HashMap::new()),
            changed: watch::Sender::new(0),
            owner_watching: AtomicBool::new(true),
            remote_clients: AtomicUsize::new(0),
        }
    }
}

impl Links {
    fn repos(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Repo>> {
        self.repos.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn watched(&self) -> bool {
        self.owner_watching.load(Ordering::SeqCst) || self.remote_clients.load(Ordering::SeqCst) > 0
    }

    fn set_owner_watching(&self, watching: bool) {
        let was = self.watched();
        self.owner_watching.store(watching, Ordering::SeqCst);
        self.ask_again_if_woken(was);
    }

    fn set_remote_clients(&self, count: usize) {
        let was = self.watched();
        self.remote_clients.store(count, Ordering::SeqCst);
        self.ask_again_if_woken(was);
    }

    fn ask_again_if_woken(&self, was: bool) {
        if !was && self.watched() {
            self.changed.send_modify(|version| *version += 1);
        }
    }

    pub(crate) fn forget(&self, dir: &Path) {
        let Some(repo) = repository(dir) else { return };
        if let Some(entry) = self.repos().get_mut(&repo) {
            entry.fetched = None;
        }
    }
}

impl Workspace {
    pub(crate) fn pull_requests_changed(&self) -> watch::Receiver<u64> {
        self.inner.links.changed.subscribe()
    }

    pub(crate) fn set_owner_watching(&self, watching: bool) {
        self.inner.links.set_owner_watching(watching);
    }

    pub(crate) fn set_remote_clients(&self, count: usize) {
        self.inner.links.set_remote_clients(count);
    }

    pub(crate) fn thread_pull_request(&self, dir: &str, branch: &str) -> Option<PullRequest> {
        if branch.trim().is_empty() || process::which("gh").is_none() {
            return None;
        }
        let repo = repository(Path::new(dir))?;
        let (pull, ask) = {
            let mut repos = self.inner.links.repos();
            let entry = repos.entry(repo.clone()).or_default();
            let ask = !entry.asked && entry.due() && self.inner.links.watched();
            entry.asked |= ask;
            (entry.pulls.get(branch).cloned(), ask)
        };
        if ask {
            let workspace = self.clone();
            let dir = PathBuf::from(dir);
            self.inner.runtime.spawn(async move { workspace.refresh_repository(repo, &dir).await });
        }
        pull
    }

    pub(crate) async fn branch_pull_request(&self, dir: &Path, branch: &str, unwatched: bool) -> Option<PullRequest> {
        process::which("gh")?;
        let repo = repository(dir)?;
        if unwatched || self.inner.links.watched() {
            self.refresh_repository(repo.clone(), dir).await;
        }
        self.inner.links.repos().get(&repo)?.pulls.get(branch).cloned()
    }

    async fn refresh_repository(&self, repo: PathBuf, dir: &Path) {
        let flight = Arc::clone(&self.inner.links.repos().entry(repo.clone()).or_default().flight);
        let _flying = flight.lock().await;
        let due = self.inner.links.repos().get(&repo).is_some_and(Repo::due);
        let found = if due { Some(git::repository_pull_requests(dir).await) } else { None };
        let changed = {
            let mut repos = self.inner.links.repos();
            let entry = repos.entry(repo).or_default();
            entry.asked = false;
            match found {
                Some(Ok(pulls)) => {
                    entry.fetched = Some(Instant::now());
                    entry.failed = None;
                    let changed = entry.pulls != pulls;
                    entry.pulls = pulls;
                    changed
                }
                Some(Err(error)) => {
                    eprintln!("local-studio: pull requests for {}: {error}", dir.display());
                    entry.failed = Some(Instant::now());
                    false
                }
                None => false,
            }
        };
        if changed {
            self.inner.links.changed.send_modify(|version| *version += 1);
        }
    }
}

fn repository(dir: &Path) -> Option<PathBuf> {
    let (dot_git, root) = threads::git_dir(dir)?;
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        root.join(text.trim().strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map_or_else(|| git_dir.clone(), |common| git_dir.join(common.trim()));
    common.canonicalize().ok()
}

fn scaled(duration: Duration) -> Duration {
    if !cfg!(feature = "test-hooks") {
        return duration;
    }
    let second_ms = std::env::var("LOCAL_STUDIO_TEST_SECOND_MS").ok().and_then(|ms| ms.parse::<u32>().ok());
    second_ms.map_or(duration, |ms| duration * ms / 1000)
}
