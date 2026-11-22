use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::model::{AppUpdateState, ChannelInstall};
use super::Workspace;
use crate::model::Update;
use crate::update::{self, Build, Checked, Offer, Refusal, Staged};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

pub(crate) struct Updates {
    state: Mutex<AppUpdateState>,
    offer: Mutex<Option<Offer>>,
    staged: Mutex<Option<Staged>>,
    started: AtomicBool,
    busy: tokio::sync::Mutex<()>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            state: Mutex::new(AppUpdateState::Idle),
            offer: Mutex::new(None),
            staged: Mutex::new(None),
            started: AtomicBool::new(false),
            busy: tokio::sync::Mutex::new(()),
        }
    }
}

fn unavailable(build: &Build) -> Option<String> {
    if let Some(refusal) = build.refusal() {
        return Some(refusal.to_string());
    }
    update::current_install().err()
}

impl Workspace {
    pub(crate) fn start_updates(&self) {
        if self.inner.updates.started.swap(true, Ordering::SeqCst) {
            self.push_update_state();
            return;
        }
        let build = Build::current();
        if let Some(reason) = unavailable(&build) {
            self.set_update(AppUpdateState::Unavailable { reason });
            return;
        }
        let workspace = self.clone();
        self.inner.runtime.spawn(async move {
            let data_dir = workspace.inner.data_dir.clone();
            let found = {
                let build = build.clone();
                tokio::task::spawn_blocking(move || update::staged(&build, &data_dir)).await.ok().flatten()
            };
            if let Some(staged) = found {
                workspace
                    .set_update(AppUpdateState::Ready { version: staged.version.clone(), notes: staged.notes.clone() });
                *workspace.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner) = Some(staged);
            } else {
                workspace.set_update(AppUpdateState::Idle);
            }
            let watcher = workspace.clone();
            workspace.inner.runtime.spawn(async move { watcher.watch_automatic().await });
            tokio::time::sleep(FIRST_CHECK).await;
            loop {
                if workspace.inner.stopped.load(Ordering::SeqCst) {
                    return;
                }
                workspace.run_update_check(false).await;
                tokio::time::sleep(update::CHECK_EVERY).await;
            }
        });
    }

    async fn watch_automatic(&self) {
        let mut settings = self.settings();
        let mut automatic = settings.borrow().automatic_updates;
        while settings.changed().await.is_ok() {
            let now = settings.borrow().automatic_updates;
            if now && !automatic && matches!(self.update_state(), AppUpdateState::Available { .. }) {
                self.download_update();
            }
            automatic = now;
        }
    }

    pub(crate) fn check_for_update(&self) {
        if !self.inner.updates.started.load(Ordering::SeqCst) {
            self.start_updates();
        }
        let workspace = self.clone();
        self.inner.runtime.spawn(async move { workspace.run_update_check(true).await });
    }

    pub(crate) fn download_update(&self) {
        let offer = self.inner.updates.offer.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some(offer) = offer else {
            self.check_for_update();
            return;
        };
        let workspace = self.clone();
        self.inner.runtime.spawn(async move {
            let Ok(_busy) = workspace.inner.updates.busy.try_lock() else { return };
            workspace.run_download(offer).await;
        });
    }

    pub(crate) fn apply_update(&self) {
        let staged = self.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some(staged) = staged else {
            self.push_update_state();
            return;
        };
        let workspace = self.clone();
        self.inner.runtime.spawn(async move {
            let Ok(_busy) = workspace.inner.updates.busy.try_lock() else { return };
            workspace.set_update(AppUpdateState::Applying { version: staged.version.clone() });
            let build = Build::current();
            let version = staged.version.clone();
            let notes = staged.notes.clone();
            let outcome = tokio::task::spawn_blocking(move || -> Result<PathBuf, Refusal> {
                let target = update::current_install().map_err(Refusal::Apply)?;
                let applied = update::apply(&build, &staged, &target)?;
                let _ = std::fs::remove_dir_all(&staged.dir);
                update::relaunch(&applied.target).map_err(Refusal::Apply)?;
                Ok(applied.target)
            })
            .await;
            match outcome {
                Ok(Ok(_)) => {
                    workspace.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner).take();
                    workspace.set_update(AppUpdateState::Restarting { version });
                }
                Ok(Err(refusal)) => {
                    let staged_gone = matches!(
                        refusal,
                        Refusal::BadSha256 { .. }
                            | Refusal::BadSignature(_)
                            | Refusal::BadBundle(_)
                            | Refusal::RolledBack(_)
                    );
                    if staged_gone {
                        let dropped =
                            workspace.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner).take();
                        if let Some(dropped) = dropped {
                            let _ = std::fs::remove_dir_all(&dropped.dir);
                        }
                    }
                    workspace.set_update(AppUpdateState::Failed {
                        reason: refusal.to_string(),
                        version: Some(version),
                        notes,
                    });
                }
                Err(error) => workspace.set_update(AppUpdateState::Failed {
                    reason: format!("The install stopped: {error}"),
                    version: Some(version),
                    notes,
                }),
            }
        });
    }

    pub(crate) fn open_channel(&self, channel: &str) {
        if channel == crate::CHANNEL {
            return;
        }
        if let Err(error) = update::open_channel(channel) {
            self.error(error);
        }
    }

    async fn run_update_check(&self, manual: bool) {
        let Ok(_busy) = self.inner.updates.busy.try_lock() else { return };
        let build = Build::current();
        if let Some(reason) = unavailable(&build) {
            self.set_update(AppUpdateState::Unavailable { reason });
            return;
        }
        let ready = self.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if ready.is_none() && (manual || !matches!(self.update_state(), AppUpdateState::Available { .. })) {
            self.set_update(AppUpdateState::Checking);
        }
        match update::check(&build).await {
            Ok(Checked::UpToDate { feed }) => {
                self.inner.updates.offer.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(staged) = ready {
                    self.set_update(AppUpdateState::Ready { version: staged.version, notes: staged.notes });
                } else {
                    self.set_update(AppUpdateState::UpToDate { latest: feed });
                }
            }
            Ok(Checked::Available(offer)) => {
                *self.inner.updates.offer.lock().unwrap_or_else(PoisonError::into_inner) = Some(offer.clone());
                if let Some(staged) = ready.filter(|staged| staged.version == offer.version) {
                    self.set_update(AppUpdateState::Ready { version: staged.version, notes: staged.notes });
                } else if self.current_settings().automatic_updates {
                    self.run_download(offer).await;
                } else {
                    self.set_update(AppUpdateState::Available { version: offer.version, notes: offer.notes });
                }
            }
            Err(refusal) => {
                if let Some(staged) = ready {
                    self.set_update(AppUpdateState::Ready { version: staged.version, notes: staged.notes });
                } else {
                    self.set_update(AppUpdateState::Failed {
                        reason: refusal.to_string(),
                        version: None,
                        notes: String::new(),
                    });
                }
            }
        }
    }

    async fn run_download(&self, offer: Offer) {
        let build = Build::current();
        let version = offer.version.clone();
        let notes = offer.notes.clone();
        self.set_update(AppUpdateState::Downloading {
            version: version.clone(),
            notes: notes.clone(),
            received: 0,
            total: None,
        });
        let mut last = Instant::now().checked_sub(PROGRESS_EVERY).unwrap_or_else(Instant::now);
        let progress = |received: u64, total: Option<u64>| {
            if last.elapsed() >= PROGRESS_EVERY || total == Some(received) {
                last = Instant::now();
                self.set_update(AppUpdateState::Downloading {
                    version: version.clone(),
                    notes: notes.clone(),
                    received,
                    total,
                });
            }
        };
        match update::download(&build, &offer, &self.inner.data_dir, progress).await {
            Ok(staged) => {
                self.set_update(AppUpdateState::Ready { version: staged.version.clone(), notes: staged.notes.clone() });
                *self.inner.updates.staged.lock().unwrap_or_else(PoisonError::into_inner) = Some(staged);
            }
            Err(refusal) => {
                self.set_update(AppUpdateState::Failed {
                    reason: refusal.to_string(),
                    version: Some(offer.version),
                    notes: offer.notes,
                });
            }
        }
    }

    fn update_state(&self) -> AppUpdateState {
        self.inner.updates.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn push_update_state(&self) {
        self.push(Update::AppUpdate { state: self.update_state() });
    }

    fn set_update(&self, state: AppUpdateState) {
        state.clone_into(&mut self.inner.updates.state.lock().unwrap_or_else(PoisonError::into_inner));
        self.push(Update::AppUpdate { state });
    }
}

pub(crate) fn channel_installs() -> Vec<ChannelInstall> {
    update::installed_channels()
        .into_iter()
        .map(|(channel, installed)| ChannelInstall {
            name: update::channel_name(&channel).to_string(),
            current: channel == crate::CHANNEL,
            installed,
            channel,
        })
        .collect()
}
