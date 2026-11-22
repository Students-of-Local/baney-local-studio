mod plan;
mod source;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) use plan::{Ls, SyncState};
pub(crate) use source::{fingerprint, locate, read, T3Data};

use super::model::{ProjectSettings, Settings, T3Report};
use super::projects::{ProjectEntry, ProjectFile};
use super::settings;
use crate::store::Store;

const STATE_FILE: &str = "t3-sync.json";
const PROJECT_FILE: &str = "project-workspace.json";

pub(crate) type Fingerprint = Vec<(String, u64, u64)>;

pub(crate) fn load_state(data_dir: &Path) -> SyncState {
    std::fs::read(data_dir.join(STATE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save_state(data_dir: &Path, state: &SyncState) -> Result<(), String> {
    let path = data_dir.join(STATE_FILE);
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(data_dir).map_err(|error| format!("create {}: {error}", data_dir.display()))?;
    let temporary = data_dir.join(format!(".{STATE_FILE}.{}.tmp", std::process::id()));
    std::fs::remove_file(&temporary).ok();
    let written = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &path)
    })();
    written.map_err(|error| {
        std::fs::remove_file(&temporary).ok();
        format!("save {}: {error}", path.display())
    })
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

pub(crate) fn empty_report(state: &SyncState, problems: Vec<String>) -> T3Report {
    T3Report {
        found: false,
        source: String::new(),
        enabled: state.enabled,
        offered: state.offered,
        applied: false,
        last_synced_at: state.last_synced_at,
        choices: state.choices,
        groups: Vec::new(),
        problems,
    }
}

pub(crate) fn run(t3: &T3Data, ls: &mut Ls, state: &mut SyncState, apply: bool) -> T3Report {
    let outcome = plan::sync(t3, ls, state, apply);
    if apply {
        state.last_synced_at = Some(now_ms());
    }
    T3Report {
        found: true,
        source: t3.dir.to_string_lossy().into_owned(),
        enabled: state.enabled,
        offered: state.offered,
        applied: apply,
        last_synced_at: state.last_synced_at,
        choices: state.choices,
        groups: outcome.groups,
        problems: outcome.problems,
    }
}

fn load_files(data_dir: &Path) -> Result<(Ls, Store, ProjectFile), String> {
    let (loaded, error) = settings::load_json::<Settings>(&data_dir.join("settings.json"));
    if let Some(error) = error {
        return Err(error);
    }
    let (keybindings, error) =
        settings::load_json::<BTreeMap<String, Option<String>>>(&data_dir.join("keybindings.json"));
    if let Some(error) = error {
        return Err(error);
    }
    let (project_file, error) = settings::load_json::<ProjectFile>(&data_dir.join(PROJECT_FILE));
    if let Some(error) = error {
        return Err(error);
    }
    let store = Store::open(data_dir).map_err(|error| format!("{error:#}"))?;
    let project_settings: BTreeMap<String, ProjectSettings> =
        project_file.projects.iter().map(|(path, entry)| (path.clone(), entry.settings.clone())).collect();
    let ls = Ls {
        settings: settings::sanitize(loaded),
        keybindings,
        projects: store.projects.clone(),
        favorites: store.favorites.clone(),
        hidden: store.hidden.clone(),
        project_settings,
    };
    Ok((ls, store, project_file))
}

fn running(data_dir: &Path) -> Result<Option<std::fs::File>, String> {
    let path = data_dir.join("studio.lock");
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "Local Studio is running on {}. Use Settings, T3 Code sync, Import now there, or quit it first.",
            data_dir.display()
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(format!("lock {}: {error}", path.display())),
    }
}

pub fn import_from_t3(data_dir: Option<&str>, dry_run: bool) -> Result<T3Report, String> {
    let data_dir: PathBuf = crate::store::data_dir(data_dir)?;
    let mut state = load_state(&data_dir);
    let home = crate::store::home_dir().ok_or_else(|| "HOME is not set".to_string())?;
    let Some(dir) = locate(&home) else {
        return Ok(empty_report(
            &state,
            vec![format!("No T3 Code data was found under {}.", home.join(".t3").display())],
        ));
    };
    let t3 = read(&dir);
    if dry_run {
        let (mut ls, _, _) = load_files(&data_dir)?;
        let mut scratch = state.clone();
        return Ok(run(&t3, &mut ls, &mut scratch, false));
    }
    std::fs::create_dir_all(&data_dir).map_err(|error| format!("create {}: {error}", data_dir.display()))?;
    let _lock = running(&data_dir)?;
    let (mut ls, mut store, mut project_file) = load_files(&data_dir)?;
    let before = ls.clone();
    state.enabled = true;
    state.offered = true;
    let report = run(&t3, &mut ls, &mut state, true);
    if ls.settings != before.settings {
        settings::save_json(&data_dir.join("settings.json"), &ls.settings)?;
    }
    if ls.keybindings != before.keybindings {
        settings::save_json(&data_dir.join("keybindings.json"), &ls.keybindings)?;
    }
    if ls.projects != before.projects || ls.favorites != before.favorites || ls.hidden != before.hidden {
        store.projects.clone_from(&ls.projects);
        store.favorites.clone_from(&ls.favorites);
        store.hidden.clone_from(&ls.hidden);
        store.save().map_err(|error| format!("{error:#}"))?;
    }
    if ls.project_settings != before.project_settings {
        for (path, overrides) in &ls.project_settings {
            let entry: &mut ProjectEntry = project_file.projects.entry(path.clone()).or_default();
            entry.settings = overrides.clone();
        }
        settings::save_json(&data_dir.join(PROJECT_FILE), &project_file)?;
    }
    save_state(&data_dir, &state)?;
    Ok(report)
}
