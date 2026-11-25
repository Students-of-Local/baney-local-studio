use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use super::text;
use crate::{frontmatter, harness::trust, plugins};

const FILE: &str = "skills.sqlite";
pub(super) const SKILL: &str = "SKILL.md";
const BUILT_IN: &str = ".system";
const VERSION: i64 = 2;
const MAX_FILE: u64 = 256 * 1024;
const MAX_SKILLS: usize = 5000;
const MAX_DEPTH: usize = 4;
const FRESH: Duration = Duration::from_secs(2);
const FOLDERS: [&str; 3] = [".agents", ".claude", ".eukhe"];
pub(super) const BESIDE: [&str; 3] = ["asks.txt", "names.txt", "where.txt"];

struct Root {
    dir: PathBuf,
    source: String,
    owner: String,
    links: bool,
    files: Option<Vec<PathBuf>>,
}

pub struct Roots {
    list: Vec<Root>,
    trusted: BTreeSet<String>,
    here: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub name: String,
    pub description: String,
    pub source: String,
    pub path: PathBuf,
    pub problem: Option<String>,
}

struct Found {
    path: PathBuf,
    source: String,
    owner: String,
    links: bool,
    stamp: String,
}

struct Row {
    id: i64,
    stamp: String,
    owner: String,
    name: String,
    description: String,
    problem: Option<String>,
}

pub struct Store {
    file: PathBuf,
    pub(super) db: Connection,
    roots: Roots,
    pub(super) seen: HashMap<i64, Listed>,
    pub(super) hidden: Vec<Listed>,
    pub(super) checked: Option<Instant>,
}

impl Root {
    fn yours(dir: &Path, source: &str) -> Self {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        Self { dir, source: source.to_string(), owner: String::new(), links: true, files: None }
    }

    fn project(dir: &Path, project: &Path, owner: &str) -> Option<Self> {
        let dir = dir.canonicalize().ok().filter(|dir| dir.starts_with(project))?;
        Some(Self { dir, source: "project".to_string(), owner: owner.to_string(), links: false, files: None })
    }

    fn hides(&self, path: &Path, problem: String) -> Listed {
        Listed {
            name: folder(path),
            description: String::new(),
            source: self.source.clone(),
            path: path.to_path_buf(),
            problem: Some(problem),
        }
    }
}

pub fn roots(data: &Path, plugins: Option<&Path>, project: Option<&Path>) -> Result<Roots, String> {
    let mut list = vec![Root::yours(&data.join("skills"), "yours")];
    let trusted = crate::store::Store::open(data).map_err(|error| format!("{error:#}"))?.trusted;
    let home = crate::store::home_dir().map(|home| home.canonicalize().unwrap_or(home));
    let mut here = None;
    if let Some(dir) = project {
        let project = dir.canonicalize().map_err(|error| format!("{}: {error}", dir.display()))?;
        let owner = trust::root(&project.to_string_lossy());
        if trusted.contains(&owner) {
            let top =
                project.ancestors().find(|dir| dir.join(".git").exists()).filter(|top| Some(*top) != home.as_deref());
            let top = top.unwrap_or(&project);
            here = Some(top.to_path_buf());
            let folders = FOLDERS.map(|dir| top.join(dir).join("skills"));
            list.extend(folders.iter().filter_map(|dir| Root::project(dir, top, &owner)));
        }
    }
    let agent = crate::daemon::agent_dir().map(|dir| dir.join("skills"));
    let others =
        [".agents", ".claude", ".codex"].into_iter().filter_map(|dir| Some(home.as_ref()?.join(dir).join("skills")));
    list.extend(agent.into_iter().chain(others).map(|dir| Root::yours(&dir, "personal")));
    let plugins = plugins.map(plugins::scan).unwrap_or_default().plugins;
    list.extend(plugins.into_iter().filter(|plugin| !plugin.local_only).map(|plugin| Root {
        source: format!("plugin {}", plugin.id),
        owner: String::new(),
        links: false,
        files: Some(plugin.skills.into_iter().map(|skill| skill.path).collect()),
        dir: plugin.dir,
    }));
    Ok(Roots { list, trusted, here })
}

impl Store {
    pub fn open(data: &Path, roots: Roots) -> Result<Self, String> {
        let file = data.join(FILE);
        let db = connect(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        Ok(Self { file, db, roots, seen: HashMap::new(), hidden: Vec::new(), checked: None })
    }

    pub fn list(&mut self) -> Result<Vec<Listed>, String> {
        self.sound(|store| {
            store.refresh()?;
            let mut listed: Vec<Listed> = store.seen.values().chain(&store.hidden).cloned().collect();
            listed.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
            Ok(listed)
        })
    }

    pub(super) fn sound<T>(&mut self, step: impl Fn(&mut Self) -> rusqlite::Result<T>) -> Result<T, String> {
        let file = self.file.clone();
        let first = shared(&file, || step(self));
        let outcome = match first {
            Err(error) if damaged(&error) => {
                self.checked = None;
                rebuilt(&file, || {
                    self.db = Connection::open_in_memory()?;
                    self.db = prepare(&file)?;
                    let outcome = step(self);
                    if outcome.as_ref().is_err_and(damaged) {
                        self.db = Connection::open_in_memory()?;
                    }
                    outcome
                })
            }
            outcome => outcome,
        };
        outcome.map_err(|error| format!("{}: {error}", self.file.display()))
    }

    pub(super) fn refresh(&mut self) -> rusqlite::Result<()> {
        if self.checked.is_some_and(|checked| checked.elapsed() < FRESH) {
            return Ok(());
        }
        let mut hidden = Vec::new();
        let mut files = HashSet::new();
        let found: Vec<Found> = self.roots.list.iter().flat_map(|root| walk(root, &mut files, &mut hidden)).collect();
        let mut stored = rows(&self.db)?;
        let (changed, gone) = due(&stored, &self.roots, &found);
        if !changed.is_empty() || !gone.is_empty() {
            let write = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            stored = rows(&write)?;
            let (changed, gone) = due(&stored, &self.roots, &found);
            let replaced = changed.iter().any(|found| stored.get(&found.path).is_some_and(|row| !row.stamp.is_empty()));
            for id in &gone {
                drop_skill(&write, *id)?;
            }
            for found in changed {
                add(&write, found)?;
            }
            if replaced || !gone.is_empty() {
                write.execute_batch(
                    "INSERT INTO whole (whole) VALUES ('optimize'); INSERT INTO parts (parts) VALUES ('optimize');",
                )?;
            }
            stored = rows(&write)?;
            write.commit()?;
        }
        self.seen.clear();
        let mut names: HashMap<String, PathBuf> = HashMap::new();
        for Found { path, source, .. } in found {
            let Some(Row { id, name, description, problem, .. }) = stored.remove(&path) else { continue };
            let mut listed = Listed { name, description, source, path, problem };
            if listed.problem.is_none() {
                let name = listed.name.to_lowercase();
                if let Some(used) = names.get(&name) {
                    listed.problem = Some(format!("another skill has this name: {}", used.display()));
                    hidden.push(listed);
                    continue;
                }
                names.insert(name, listed.path.clone());
            }
            self.seen.insert(id, listed);
        }
        self.hidden = hidden;
        self.checked = Some(Instant::now());
        Ok(())
    }

    pub(super) fn current(&self) -> rusqlite::Result<bool> {
        let stored = rows(&self.db)?;
        Ok(self.seen.iter().all(|(id, skill)| stored.get(&skill.path).is_some_and(|row| row.id == *id)))
    }

    pub(super) fn named(&self, name: &str) -> rusqlite::Result<Option<i64>> {
        let (key, spelled) = (text::key(name), name.trim().to_lowercase());
        let exact = self.seen.iter().find(|(_, skill)| skill.problem.is_none() && skill.name.to_lowercase() == spelled);
        if exact.is_some() || key.is_empty() {
            return Ok(exact.map(|(id, _)| *id));
        }
        let mut rows = self.db.prepare("SELECT skill FROM names WHERE key = ?1 ORDER BY old, skill")?;
        let ids = rows.query_map([key], |row| row.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        let mut named = ids.into_iter().filter_map(|id| Some((id, self.seen.get(&id)?)));
        Ok(named.clone().find(|(_, skill)| skill.problem.is_none()).or_else(|| named.next()).map(|(id, _)| id))
    }
}

fn folder(path: &Path) -> String {
    let folder = path.parent().and_then(Path::file_name);
    folder.map(|name| text::line(&name.to_string_lossy())).unwrap_or_default()
}

fn stamp(path: &Path, links: bool) -> Option<(u64, String)> {
    let meta = if links { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
    let meta = meta.ok().filter(std::fs::Metadata::is_file)?;
    Some((meta.len(), changed(&meta)))
}

#[cfg(unix)]
fn changed(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    let times = [meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec()];
    format!("{} {} {times:?}", meta.len(), meta.ino())
}

#[cfg(not(unix))]
fn changed(meta: &std::fs::Metadata) -> String {
    format!("{} {:?}", meta.len(), meta.modified().ok())
}

fn walk(root: &Root, files: &mut HashSet<PathBuf>, hidden: &mut Vec<Listed>) -> Vec<Found> {
    let mut found = Vec::new();
    let paths: Box<dyn Iterator<Item = PathBuf>> = match &root.files {
        Some(files) => Box::new(files.iter().cloned()),
        None => Box::new(skill_files(root)),
    };
    for path in paths {
        let Some((size, mut stamp)) = path.to_str().and_then(|_| stamp(&path, root.links)) else { continue };
        let real = path.canonicalize().unwrap_or_else(|_| path.clone());
        if files.contains(&real) {
            continue;
        }
        if found.len() == MAX_SKILLS {
            hidden.push(root.hides(&path, format!("more than {MAX_SKILLS} skills in one folder")));
            break;
        }
        files.insert(real);
        if size > MAX_FILE {
            hidden.push(root.hides(&path, format!("larger than {} KB", MAX_FILE / 1024)));
            continue;
        }
        for name in BESIDE {
            stamp.push(' ');
            let beside = self::stamp(&path.with_file_name(name), root.links);
            stamp.push_str(&beside.map(|(_, stamp)| stamp).unwrap_or_default());
        }
        let (source, owner) = (root.source.clone(), root.owner.clone());
        found.push(Found { path, source, owner, links: root.links, stamp });
    }
    found
}

fn skill_files(root: &Root) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(&root.dir)
        .standard_filters(false)
        .follow_links(root.links)
        .max_depth(Some(MAX_DEPTH))
        .filter_entry(|entry| {
            let folder = entry.file_type().is_some_and(|kind| kind.is_dir());
            let dotted = entry.file_name().to_string_lossy().starts_with('.') && entry.file_name() != BUILT_IN;
            entry.depth() == 0
                || !folder
                || !dotted && (entry.depth() < 2 || !entry.path().parent().is_some_and(|dir| dir.join(SKILL).exists()))
        })
        .sort_by_file_name(Ord::cmp)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == SKILL)
        .map(ignore::DirEntry::into_path)
}

fn rows(db: &Connection) -> rusqlite::Result<HashMap<PathBuf, Row>> {
    db.prepare("SELECT path, id, stamp, owner, name, descr, problem FROM skills NOT INDEXED")?
        .query_map([], |row| {
            let (id, stamp, owner) = (row.get(1)?, row.get(2)?, row.get(3)?);
            let (name, description, problem) = (row.get(4)?, row.get(5)?, row.get(6)?);
            Ok((PathBuf::from(row.get::<_, String>(0)?), Row { id, stamp, owner, name, description, problem }))
        })?
        .collect()
}

fn due<'a>(known: &HashMap<PathBuf, Row>, roots: &Roots, found: &'a [Found]) -> (Vec<&'a Found>, Vec<i64>) {
    let here: HashSet<&Path> = found.iter().map(|found| found.path.as_path()).collect();
    let changed =
        |found: &&Found| known.get(&found.path).is_none_or(|row| row.stamp != found.stamp || row.owner != found.owner);
    let inside = |path: &Path, top: &Path| {
        let folder =
            path.ancestors().find(|dir| FOLDERS.iter().any(|name| dir.ends_with(Path::new(name).join("skills"))));
        let above = folder.and_then(Path::parent).unwrap_or(path);
        path.starts_with(top) && !above.ancestors().take_while(|dir| *dir != top).any(|dir| dir.join(".git").exists())
    };
    let elsewhere = |path: &Path, owner: &str| {
        roots.trusted.contains(owner) && roots.here.as_ref().is_none_or(|top| !inside(path, top)) && path.exists()
    };
    let lost = known.iter().filter(|(path, row)| !here.contains(path.as_path()) && !elsewhere(path, &row.owner));
    let lost = lost.map(|(_, row)| row.id).collect();
    (found.iter().filter(changed).collect(), lost)
}

fn add(db: &Connection, found: &Found) -> rusqlite::Result<()> {
    let path = found.path.to_string_lossy();
    let old: Option<i64> =
        db.query_row("SELECT id FROM skills WHERE path = ?1", [&path], |row| row.get(0)).optional()?;
    if let Some(old) = old {
        drop_skill(db, old)?;
    }
    let mut kept = found.stamp.as_str();
    let (content, unread) = match std::fs::read_to_string(&found.path) {
        Ok(content) => (content, None),
        Err(error) if error.kind() == ErrorKind::InvalidData => {
            (String::new(), Some("cannot be read as text".to_string()))
        }
        Err(error) => {
            kept = "";
            (String::new(), Some(error.to_string()))
        }
    };
    let extra = |name: &str| {
        let path = found.path.with_file_name(name);
        let small = stamp(&path, found.links).is_some_and(|(size, _)| size <= MAX_FILE);
        small.then(|| std::fs::read_to_string(path).ok()).flatten()
    };
    let field =
        |key: &str| frontmatter::field(&content, key).map(|value| text::line(&value)).filter(|value| !value.is_empty());
    let name = field("name").unwrap_or_else(|| folder(&found.path));
    let description = field("description");
    let problem = unread.or_else(|| description.is_none().then(|| "no description in its front matter".to_string()));
    let description = description.unwrap_or_default();
    let body = frontmatter::strip(&content);
    let sections = text::sections(&body);
    let chars = u32::try_from(body.chars().count()).unwrap_or(u32::MAX);
    let spoken = name.replace(['-', '_'], " ");
    let beside = BESIDE.map(|name| extra(name).unwrap_or_default());
    db.execute(
        "INSERT INTO skills (id, path, stamp, owner, name, descr, chars, problem)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![old, path, kept, found.owner, name, description, chars, problem],
    )?;
    let id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO whole (rowid, name, descr, asks, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, spoken, description, beside.join("\n"), body],
    )?;
    let mut part = db.prepare_cached("INSERT INTO parts (rowid, name, head, body) VALUES (?1, ?2, ?3, ?4)")?;
    let pieces =
        sections.iter().flat_map(|part| text::pieces(&part.text).into_iter().map(|piece| (part.head.as_str(), piece)));
    for (row, (head, piece)) in (id << 20..).zip([("", description.as_str())].into_iter().chain(pieces)) {
        part.execute(params![row, spoken, head, piece])?;
    }
    let mut named = db.prepare_cached("INSERT INTO names (skill, key, old) VALUES (?1, ?2, ?3)")?;
    named.execute(params![id, text::key(&name), false])?;
    for old in beside[1].lines().map(text::key).filter(|key| !key.is_empty()) {
        named.execute(params![id, old, true])?;
    }
    let listed = text::listed_places(&sections, extra("where.txt").as_deref());
    let linked = text::linked_places(&body);
    let mut place = db.prepare_cached("INSERT OR IGNORE INTO places (skill, place, listed) VALUES (?1, ?2, ?3)")?;
    for (places, listed) in [(listed, true), (linked, false)] {
        for at in places {
            place.execute(params![id, at, listed])?;
        }
    }
    Ok(())
}

fn drop_skill(db: &Connection, id: i64) -> rusqlite::Result<()> {
    for sql in [
        "DELETE FROM skills WHERE id = ?1",
        "DELETE FROM whole WHERE rowid = ?1",
        "DELETE FROM parts WHERE rowid >= ?1 << 20 AND rowid < (?1 + 1) << 20",
        "DELETE FROM names WHERE skill = ?1",
        "DELETE FROM places WHERE skill = ?1",
    ] {
        db.execute(sql, [id])?;
    }
    Ok(())
}

fn damaged(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => {
            matches!(failure.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::Unknown)
                || failure.extended_code == rusqlite::ffi::SQLITE_READONLY_DBMOVED
        }
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::Utf8Error(..) => true,
        _ => false,
    }
}

fn lock(file: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::options().create(true).append(true).open(file.with_extension("lock"))
}

fn shared<T>(file: &Path, attempt: impl FnOnce() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let _held = lock(file).and_then(|lock| lock.lock_shared().map(|()| lock));
    attempt()
}

fn rebuilt<T>(file: &Path, mut attempt: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let _held = lock(file).and_then(|lock| lock.lock().map(|()| lock));
    match attempt() {
        Err(error) if damaged(&error) => {
            remove(file);
            attempt()
        }
        outcome => outcome,
    }
}

fn remove(file: &Path) {
    for end in ["", "-journal"] {
        let mut name = file.as_os_str().to_os_string();
        name.push(end);
        let _ = std::fs::remove_file(name);
    }
}

fn connect(file: &Path) -> rusqlite::Result<Connection> {
    match shared(file, || prepare(file)) {
        Err(error) if damaged(&error) => rebuilt(file, || prepare(file)),
        outcome => outcome,
    }
}

fn prepare(file: &Path) -> rusqlite::Result<Connection> {
    create_private(file);
    let mut db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(30))?;
    db.pragma_update(None, "secure_delete", true)?;
    let write = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if write.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? != VERSION {
        write.execute_batch(
            "DROP TABLE IF EXISTS skills; DROP TABLE IF EXISTS whole; DROP TABLE IF EXISTS parts;
             DROP TABLE IF EXISTS names; DROP TABLE IF EXISTS places;
             CREATE TABLE skills (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT UNIQUE, stamp TEXT, owner TEXT, name TEXT,
                                  descr TEXT, chars INTEGER, problem TEXT);
             CREATE VIRTUAL TABLE whole USING fts5 (name, descr, asks, body, content='', contentless_delete=1,
                                  tokenize='porter unicode61 remove_diacritics 2');
             CREATE VIRTUAL TABLE parts USING fts5 (name, head, body,
                                  tokenize='porter unicode61 remove_diacritics 2');
             CREATE TABLE names (skill INTEGER, key TEXT, old INTEGER);
             CREATE INDEX names_key ON names (key);
             CREATE TABLE places (skill INTEGER, place TEXT, listed INTEGER, PRIMARY KEY (skill, place));",
        )?;
        write.pragma_update(None, "user_version", VERSION)?;
    }
    write.commit()?;
    Ok(db)
}

fn create_private(file: &Path) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let _ = options.open(file);
    #[cfg(unix)]
    let _ = std::fs::set_permissions(file, std::os::unix::fs::PermissionsExt::from_mode(0o600));
}
