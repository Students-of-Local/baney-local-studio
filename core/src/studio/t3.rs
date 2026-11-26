use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Actor, Command};
use crate::model::{NoticeLevel, Update};
use crate::workspace::model::{T3Choices, T3ItemState, T3Report};
use crate::workspace::t3::{self, Fingerprint, Ls, SyncState, T3Data};

pub(super) const POLL: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct Watch {
    state: Option<SyncState>,
    print: Option<Fingerprint>,
    dir: Option<PathBuf>,
    reading: bool,
    pending: bool,
    pending_import: Option<T3Choices>,
}

fn imported(report: &T3Report) -> usize {
    report
        .groups
        .iter()
        .filter(|group| group.chosen)
        .flat_map(|group| &group.items)
        .filter(|item| item.state == T3ItemState::Import)
        .count()
}

impl Actor {
    fn t3_state(&mut self) -> SyncState {
        if self.t3.state.is_none() {
            self.t3.state = Some(t3::load_state(&self.data_dir));
        }
        self.t3.state.clone().unwrap_or_default()
    }

    fn t3_keep(&mut self, state: SyncState) {
        if self.t3.state.as_ref() != Some(&state) {
            if let Err(error) = t3::save_state(&self.data_dir, &state) {
                self.mailbox.notice(NoticeLevel::Warning, error);
            }
        }
        self.t3.state = Some(state);
    }

    pub(super) fn t3_poll(&mut self, force: bool, import: Option<T3Choices>) {
        if self.t3.reading {
            if force {
                self.t3.pending = true;
                self.t3.pending_import = import.or(self.t3.pending_import.take());
            }
            return;
        }
        let dir = self.t3.dir.clone().filter(|dir| dir.is_dir()).or_else(|| t3::locate(Path::new(&self.home)));
        let Some(dir) = dir else {
            if force || self.t3.print.is_none() || self.t3.dir.is_some() {
                self.t3.dir = None;
                self.t3.print = Some(Vec::new());
                let state = self.t3_state();
                if import.is_some() {
                    self.mailbox.notice(NoticeLevel::Warning, "No T3 Code data was found on this machine.");
                }
                self.mailbox.push(Update::T3Sync { report: t3::empty_report(&state, Vec::new()) });
            }
            return;
        };
        let print = t3::fingerprint(&dir);
        if !force && self.t3.dir.as_ref() == Some(&dir) && self.t3.print.as_ref() == Some(&print) {
            return;
        }
        self.t3.dir = Some(dir.clone());
        self.t3.reading = true;
        let inbox = self.itself.clone();
        tokio::task::spawn_blocking(move || {
            let data = t3::read(&dir);
            inbox.send(Command::T3Loaded { data: Some(Box::new(data)), print, import });
        });
    }

    fn t3_ls(&self) -> Ls {
        let project_settings: BTreeMap<_, _> = self
            .store
            .projects
            .iter()
            .map(|project| (project.path.clone(), self.workspace.project_entry(&project.path).settings))
            .filter(|(_, settings)| *settings != crate::workspace::model::ProjectSettings::default())
            .collect();
        Ls {
            settings: self.workspace.current(),
            keybindings: self.workspace.keybinding_overrides(),
            projects: self.store.projects.clone(),
            favorites: self.store.favorites.clone(),
            hidden: self.store.hidden.clone(),
            project_settings,
            sessions: t3::sessions(&self.store),
            threads: Vec::new(),
        }
    }

    fn t3_write(&mut self, before: &Ls, after: Ls) {
        if after.settings != before.settings {
            self.workspace.apply_settings(after.settings);
        }
        if after.keybindings != before.keybindings {
            self.workspace.replace_keybindings(after.keybindings);
        }
        for (path, settings) in &after.project_settings {
            if before.project_settings.get(path) != Some(settings) {
                self.workspace.update_project_settings(path.clone(), settings.clone());
            }
        }
        let threads = !after.threads.is_empty();
        t3::add_threads(&mut self.store, after.threads);
        let models = after.favorites != before.favorites || after.hidden != before.hidden;
        if models {
            self.store.favorites = after.favorites;
            self.store.hidden = after.hidden;
        }
        if after.projects != before.projects {
            self.store.projects = after.projects;
            self.projects_changed();
        } else if models || threads {
            self.save();
        }
        self.dirty |= threads;
        if models {
            self.push_models();
        }
    }

    pub(super) fn t3_loaded(&mut self, data: Option<Box<T3Data>>, print: Fingerprint, import: Option<T3Choices>) {
        self.t3.reading = false;
        self.t3.print = Some(print);
        let mut state = self.t3_state();
        if let Some(choices) = import {
            state.choices = choices;
            state.enabled = true;
            state.offered = true;
        }
        if let Some(data) = data {
            let mut ls = self.t3_ls();
            let report = if state.enabled {
                let before = ls.clone();
                let report = t3::run(&data, &mut ls, &mut state, true);
                self.t3_write(&before, ls);
                report
            } else {
                let mut scratch = state.clone();
                t3::run(&data, &mut ls, &mut scratch, false)
            };
            if import.is_some() {
                let count = imported(&report);
                let text = if count == 0 {
                    "Local Studio already has your T3 Code setup.".to_string()
                } else {
                    format!("Brought over {count} item{} from T3 Code.", if count == 1 { "" } else { "s" })
                };
                self.mailbox.notice(NoticeLevel::Info, text);
            }
            self.mailbox.push(Update::T3Sync { report });
        }
        self.t3_keep(state);
        if std::mem::take(&mut self.t3.pending) {
            let import = self.t3.pending_import.take();
            self.t3_poll(true, import);
        }
    }

    pub(super) fn t3_dismiss(&mut self) {
        let mut state = self.t3_state();
        state.offered = true;
        self.t3_keep(state);
        self.t3_poll(true, None);
    }

    pub(super) fn t3_set_sync(&mut self, enabled: bool) {
        let mut state = self.t3_state();
        state.enabled = enabled;
        state.offered = true;
        self.t3_keep(state);
        self.t3_poll(true, None);
    }
}
