use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::plan::{account_dir, display, driver_name, harness_of, map_model, unsupported_reason, Engine, Ls};
use super::source::{Database, Instance, Selection, T3Data};
use crate::harness::{self, accounts};
use crate::model::{AccessMode, Harness};
use crate::store::ThreadFlags;
use crate::workspace::model::{T3ItemState, T3Kind};
use crate::workspace::time::parse_rfc3339_ms;

#[derive(Debug, Clone)]
pub(super) struct Place {
    harness: Harness,
    account: PathBuf,
    cwd: String,
}

#[derive(Debug, Clone)]
pub(super) struct Conversation {
    id: String,
    project: String,
    title: String,
    driver: String,
    instance: String,
    session: String,
    selection: Selection,
    access: String,
    worktree: Option<String>,
    cwd: Option<String>,
    active_at: Option<i64>,
    settled_at: Option<i64>,
    snoozed_until: Option<i64>,
    archived: bool,
    running: bool,
    place: Result<Place, String>,
}

const CONVERSATION_TABLES: [&str; 4] = [
    "orchestration_v2_projection_threads",
    "orchestration_v2_projection_provider_threads",
    "orchestration_v2_projection_provider_sessions",
    "orchestration_v2_projection_runs",
];

const CONVERSATIONS: &str = "SELECT t.thread_id, t.project_id, t.title, p.driver, p.provider_instance_id, \
     json_extract(p.payload_json, '$.nativeThreadRef.nativeId'), json_extract(t.payload_json, '$.modelSelection'), \
     t.runtime_mode, json_extract(t.payload_json, '$.worktreePath'), \
     (SELECT json_extract(s.payload_json, '$.cwd') FROM orchestration_v2_projection_provider_sessions s \
      WHERE s.provider_session_id = p.provider_session_id AND json_valid(s.payload_json)), \
     COALESCE((SELECT MAX(r.requested_at) FROM orchestration_v2_projection_runs r WHERE r.thread_id = t.thread_id), \
      t.updated_at), \
     json_extract(t.payload_json, '$.settledAt'), json_extract(t.payload_json, '$.snoozedUntil'), \
     t.archived_at IS NOT NULL, \
     EXISTS(SELECT 1 FROM orchestration_v2_projection_runs r WHERE r.thread_id = t.thread_id \
      AND r.completed_at IS NULL AND r.status NOT IN ('completed', 'cancelled', 'failed', 'interrupted')) \
     FROM orchestration_v2_projection_threads t \
     JOIN orchestration_v2_projection_provider_threads p ON p.provider_thread_id = t.active_provider_thread_id \
     WHERE t.deleted_at IS NULL AND CASE WHEN json_valid(t.payload_json) AND json_valid(p.payload_json) \
      THEN json_extract(t.payload_json, '$.lineage.parentThreadId') IS NULL \
      AND json_extract(p.payload_json, '$.nativeThreadRef.nativeId') IS NOT NULL END ORDER BY 11 DESC";

pub(super) fn read(database: &Database) -> Result<Vec<Conversation>, String> {
    for table in CONVERSATION_TABLES {
        if !database.has(table)? {
            return Ok(Vec::new());
        }
    }
    let conversations = database.rows(CONVERSATIONS, |row| {
        let text = |column: usize| row.get::<_, Option<String>>(column).unwrap_or_default();
        let time = |column: usize| text(column).as_deref().and_then(parse_rfc3339_ms);
        let selection = text(6).and_then(|text| serde_json::from_str(&text).ok());
        Ok(Conversation {
            id: row.get(0)?,
            project: text(1).unwrap_or_default(),
            title: text(2).unwrap_or_default(),
            driver: text(3).unwrap_or_default(),
            instance: text(4).unwrap_or_default(),
            session: text(5).unwrap_or_default(),
            selection: selection.unwrap_or_default(),
            access: text(7).unwrap_or_default(),
            worktree: text(8),
            cwd: text(9),
            active_at: time(10),
            settled_at: time(11),
            snoozed_until: time(12),
            archived: row.get(13)?,
            running: row.get(14)?,
            place: Err(String::new()),
        })
    })?;
    Ok(conversations.into_iter().filter(|known| plain(&known.session)).collect())
}

fn plain(session: &str) -> bool {
    (1..=64).contains(&session.len()) && session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn rollouts(sessions: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut pending = vec![sessions.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names
}

pub(super) fn place(t3: &mut T3Data) {
    let mut known = BTreeMap::new();
    let mut conversations = std::mem::take(&mut t3.conversations);
    for conversation in &mut conversations {
        conversation.place = find(t3, conversation, &mut known);
    }
    t3.conversations = conversations;
}

fn find(
    t3: &T3Data,
    conversation: &Conversation,
    known: &mut BTreeMap<PathBuf, BTreeSet<String>>,
) -> Result<Place, String> {
    let driver = &conversation.driver;
    let harness = harness_of(driver).ok_or_else(|| unsupported_reason(driver))?;
    let mut instances: Vec<&Instance> = t3.instances.iter().filter(|known| known.driver == *driver).collect();
    instances.sort_by_key(|instance| instance.id != conversation.instance);
    if instances.first().is_some_and(|instance| instance.id == conversation.instance && !instance.enabled) {
        return Err("Its account is turned off in T3 Code.".to_string());
    }
    let dirs = instances.into_iter().filter_map(|instance| account_dir(instance, harness));
    let dirs: Vec<PathBuf> = dirs.chain(accounts::home_dir_of(harness)).collect();
    let root = t3.projects.iter().find(|project| project.id == conversation.project).map(|project| &project.path);
    let folders = [conversation.worktree.as_ref(), root, conversation.cwd.as_ref()];
    let folders: Vec<&String> = folders.into_iter().flatten().collect();
    let found = match harness {
        Harness::ClaudeCode => dirs.iter().find_map(|dir| {
            let held = |cwd: &&&String| harness::claude_session_exists(Some(dir), cwd, &conversation.session);
            folders.iter().find(held).map(|cwd| (dir, *cwd))
        }),
        Harness::Codex | Harness::Eukhe => {
            let suffix = format!("-{}.jsonl", conversation.session);
            let held = |dir: &&PathBuf| {
                let names = known.entry((*dir).clone()).or_insert_with(|| rollouts(&dir.join("sessions")));
                names.iter().any(|name| name.ends_with(&suffix))
            };
            let cwd = folders.iter().find(|cwd| Path::new(cwd).is_dir()).or(folders.first());
            dirs.iter().find(held).zip(cwd.copied())
        }
    };
    let (account, cwd) = found.ok_or_else(|| format!("Its {} session is not on this machine.", driver_name(driver)))?;
    Ok(Place { harness, account: account.clone(), cwd: cwd.clone() })
}

fn access_of(mode: &str) -> AccessMode {
    match mode {
        "full-access" => AccessMode::FullAccess,
        "auto" => AccessMode::Auto,
        "auto-accept-edits" => AccessMode::AutoAcceptEdits,
        _ => AccessMode::Supervised,
    }
}

fn thread_flags(t3: &T3Data, ls: &Ls, conversation: &Conversation, found: &Place) -> ThreadFlags {
    let model = map_model(t3, &conversation.selection.instance_id, &conversation.selection.model).ok();
    let model = model.filter(|model| model.provider == harness::provider_of(found.harness));
    let project = t3.projects.iter().find(|project| project.id == conversation.project);
    let project = project.and_then(|project| std::fs::canonicalize(&project.path).ok());
    let project = project.map(|path| path.to_string_lossy().into_owned());
    ThreadFlags {
        harness: found.harness,
        account: accounts::stored(found.harness, &found.account),
        fork_of: Some(conversation.session.clone()),
        cwd: Some(found.cwd.clone()),
        title: Some(conversation.title.clone()).filter(|title| !title.trim().is_empty()),
        thinking_level: model.is_some().then(|| conversation.selection.effort()).flatten(),
        model: model.map(|model| model.model),
        access: Some(access_of(&conversation.access)),
        project: project.filter(|path| ls.projects.iter().any(|known| &known.path == path)),
        activity_at: conversation.active_at,
        settled_at: conversation.settled_at,
        snoozed_until: conversation.snoozed_until,
        archived: conversation.archived,
        ..ThreadFlags::default()
    }
}

pub(super) fn threads_group(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Threads;
    for conversation in &t3.conversations {
        let key = format!("thread:{}", conversation.id);
        let label = crate::threads::clean_title(&conversation.title);
        let label = if label.is_empty() { crate::threads::NEW_THREAD_TITLE.to_string() } else { label };
        let present = ls.sessions.contains(&conversation.session);
        let known = present || engine.imported.contains_key(&key);
        let blocked = match &conversation.place {
            Ok(_) if conversation.running => Some("Running in T3 Code. It comes over when it stops.".to_string()),
            place => place.as_ref().err().cloned(),
        };
        if let Some(reason) = blocked.filter(|_| !known) {
            engine.item(kind, key, label, reason, T3ItemState::Unsupported);
            continue;
        }
        let state = engine.member(kind, &key, present);
        let folder = conversation.place.as_ref().ok().map(|place| {
            let gone = !Path::new(&place.cwd).is_dir();
            let note = if gone { ". Its folder is gone: you can read it, not continue it." } else { "" };
            format!(", {}{note}", display(Path::new(&place.cwd)))
        });
        let detail = format!("{}{}", driver_name(&conversation.driver), folder.unwrap_or_default());
        if let Some(place) = conversation.place.as_ref().ok().filter(|_| !known && engine.live(kind)) {
            ls.threads.push(thread_flags(t3, ls, conversation, place));
            ls.sessions.insert(conversation.session.clone());
        }
        engine.item(kind, key, label, detail, state);
    }
}
