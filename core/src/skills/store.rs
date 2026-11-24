use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ignore::WalkBuilder;
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use super::text;
use crate::{frontmatter, harness::trust, plugins};

const FILE: &str = "skills.sqlite";
const VERSION: i64 = 1;
const MAX_FILE: u64 = 256 * 1024;
const MAX_SKILLS: usize = 5000;
const MAX_DEPTH: usize = 4;
const FRESH: Duration = Duration::from_secs(2);

pub struct Root {
    dir: PathBuf,
    source: String,
    links: bool,
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
    stamp: String,
}

#[derive(Clone)]
struct Seen {
    name: String,
    source: String,
    path: PathBuf,
}

pub struct Store {
    file: PathBuf,
    db: Connection,
    roots: Vec<Root>,
    seen: HashMap<i64, Seen>,
    hidden: Vec<(PathBuf, String)>,
    checked: Option<Instant>,
}

impl Root {
    fn new(dir: &Path, source: &str, links: bool) -> Self {
        Self { dir: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()), source: source.to_string(), links }
    }
}

#[must_use]
pub fn roots(data: &Path, plugins: Option<&Path>, project: Option<&Path>) -> Vec<Root> {
    let mut roots = vec![Root::new(&data.join("skills"), "yours", true)];
    if let Some(project) = project {
        let cwd = project.to_string_lossy();
        let trusted = crate::store::Store::open(data).is_ok_and(|store| store.trusted.contains(&trust::root(&cwd)));
        if trusted {
            let folders = [".agents", ".claude", ".eukhe"].map(|dir| project.join(dir).join("skills"));
            roots.extend(folders.map(|dir| Root::new(&dir, "project", false)));
        }
    }
    let agent = crate::daemon::agent_dir().map(|dir| dir.join("skills"));
    let home = crate::store::home_dir();
    let others =
        [".agents", ".claude", ".codex"].into_iter().filter_map(|dir| Some(home.as_ref()?.join(dir).join("skills")));
    let personal = agent.into_iter().chain(others);
    roots.extend(personal.map(|dir| Root::new(&dir, "personal", true)));
    let plugins = plugins.map(plugins::scan).unwrap_or_default().plugins;
    for plugin in plugins.iter().filter(|plugin| !plugin.local_only) {
        let dirs: BTreeSet<PathBuf> =
            plugin.skills.iter().filter_map(|skill| Some(skill.path.parent()?.parent()?.to_path_buf())).collect();
        roots.extend(dirs.iter().map(|dir| Root::new(dir, &format!("plugin {}", plugin.id), false)));
    }
    roots
}

impl Store {
    pub fn open(data: &Path, roots: Vec<Root>) -> Result<Self, String> {
        let file = data.join(FILE);
        let db = connect(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        Ok(Self { file, db, roots, seen: HashMap::new(), hidden: Vec::new(), checked: None })
    }

    pub fn list(&mut self) -> Result<Vec<Listed>, String> {
        self.sound(|store| {
            store.refresh()?;
            let mut listed = Vec::new();
            let mut rows = store.db.prepare("SELECT name, descr, problem FROM skills WHERE id = ?1")?;
            for (id, seen) in &store.seen {
                listed.push(rows.query_row([id], |row| {
                    Ok(Listed {
                        name: row.get(0)?,
                        description: row.get(1)?,
                        source: seen.source.clone(),
                        path: seen.path.clone(),
                        problem: row.get(2)?,
                    })
                })?);
            }
            listed.extend(store.hidden.iter().map(|(path, problem)| Listed {
                name: folder(path),
                description: String::new(),
                source: String::new(),
                path: path.clone(),
                problem: Some(problem.clone()),
            }));
            listed.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
            Ok(listed)
        })
    }

    fn sound<T>(&mut self, step: impl Fn(&mut Self) -> rusqlite::Result<T>) -> Result<T, String> {
        let outcome = match step(self) {
            Err(error) if damaged(&error) => {
                self.checked = None;
                self.db = Connection::open_in_memory().map_err(|error| error.to_string())?;
                remove(&self.file);
                connect(&self.file).and_then(|db| {
                    self.db = db;
                    step(self)
                })
            }
            outcome => outcome,
        };
        outcome.map_err(|error| format!("{}: {error}", self.file.display()))
    }

    fn refresh(&mut self) -> rusqlite::Result<()> {
        if self.checked.is_some_and(|checked| checked.elapsed() < FRESH) {
            return Ok(());
        }
        let mut hidden = Vec::new();
        let found: Vec<Found> = self.roots.iter().flat_map(|root| walk(root, &mut hidden)).collect();
        let (changed, gone) = due(&self.db, &self.roots, &found)?;
        if !changed.is_empty() || !gone.is_empty() {
            let write = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (changed, gone) = due(&write, &self.roots, &found)?;
            for id in gone {
                drop_skill(&write, id)?;
            }
            for found in changed {
                add(&write, found)?;
            }
            write.commit()?;
        }
        self.seen.clear();
        let mut names: HashMap<String, PathBuf> = HashMap::new();
        let mut row = self.db.prepare("SELECT id, name FROM skills WHERE path = ?1")?;
        for found in found {
            let stored =
                row.query_row([found.path.to_string_lossy()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            let Some((id, name)): Option<(i64, String)> = stored else {
                hidden.push((found.path, "cannot be read as text".to_string()));
                continue;
            };
            if self.seen.contains_key(&id) {
                continue;
            }
            match names.get(&text::key(&name)) {
                Some(used) => hidden.push((found.path, format!("another skill has this name: {}", used.display()))),
                None => {
                    names.insert(text::key(&name), found.path.clone());
                    self.seen.insert(id, Seen { name, source: found.source, path: found.path });
                }
            }
        }
        drop(row);
        self.hidden = hidden;
        self.checked = Some(Instant::now());
        Ok(())
    }
}

fn folder(path: &Path) -> String {
    let folder = path.parent().and_then(Path::file_name);
    folder.map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stamp(path: &Path) -> Option<(u64, String)> {
    let meta = std::fs::symlink_metadata(path).ok().filter(std::fs::Metadata::is_file)?;
    let changed = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), format!("{}:{changed}", meta.len())))
}

fn walk(root: &Root, hidden: &mut Vec<(PathBuf, String)>) -> Vec<Found> {
    let files = WalkBuilder::new(&root.dir)
        .standard_filters(false)
        .hidden(true)
        .follow_links(root.links)
        .max_depth(Some(MAX_DEPTH))
        .filter_entry(|entry| {
            let folder = entry.file_type().is_some_and(|kind| kind.is_dir());
            !folder || entry.depth() < 2 || !entry.path().parent().is_some_and(|dir| dir.join("SKILL.md").exists())
        })
        .sort_by_file_name(Ord::cmp)
        .build();
    let mut found = Vec::new();
    for entry in files.filter_map(Result::ok).filter(|entry| entry.file_name() == "SKILL.md") {
        let path = entry.into_path();
        let Some((size, stamp)) = path.to_str().and_then(|_| stamp(&path)) else { continue };
        if size > MAX_FILE {
            hidden.push((path, format!("larger than {} KB", MAX_FILE / 1024)));
            continue;
        }
        found.push(Found { path, source: root.source.clone(), stamp });
        if found.len() == MAX_SKILLS {
            break;
        }
    }
    found
}

fn due<'a>(db: &Connection, roots: &[Root], found: &'a [Found]) -> rusqlite::Result<(Vec<&'a Found>, Vec<i64>)> {
    let known: HashMap<PathBuf, (i64, String)> = db
        .prepare("SELECT path, id, stamp FROM skills")?
        .query_map([], |row| Ok((PathBuf::from(row.get::<_, String>(0)?), (row.get(1)?, row.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    let here: HashSet<&Path> = found.iter().map(|found| found.path.as_path()).collect();
    let changed = |found: &&Found| known.get(&found.path).is_none_or(|(_, stamp)| *stamp != found.stamp);
    let gone =
        |path: &Path| !here.contains(path) && (!path.exists() || roots.iter().any(|root| path.starts_with(&root.dir)));
    let lost = known.iter().filter(|(path, _)| gone(path)).map(|(_, (id, _))| *id).collect();
    Ok((found.iter().filter(changed).collect(), lost))
}

fn add(db: &Connection, found: &Found) -> rusqlite::Result<()> {
    let path = found.path.to_string_lossy();
    let old: Option<i64> =
        db.query_row("SELECT id FROM skills WHERE path = ?1", [&path], |row| row.get(0)).optional()?;
    if let Some(old) = old {
        drop_skill(db, old)?;
    }
    let Ok(content) = std::fs::read_to_string(&found.path) else { return Ok(()) };
    let field =
        |key: &str| frontmatter::field(&content, key).map(|value| text::line(&value)).filter(|value| !value.is_empty());
    let name = field("name").unwrap_or_else(|| text::line(&folder(&found.path)));
    let description = field("description");
    let problem = description.is_none().then_some("no description in its front matter");
    let description = description.unwrap_or_default();
    db.execute(
        "INSERT INTO skills (id, path, stamp, name, descr, problem) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![old, path, found.stamp, name, description, problem],
    )?;
    Ok(())
}

fn drop_skill(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute("DELETE FROM skills WHERE id = ?1", [id]).map(|_| ())
}

fn damaged(error: &rusqlite::Error) -> bool {
    matches!(error.sqlite_error_code(), Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase))
}

fn remove(file: &Path) {
    for end in ["", "-journal"] {
        let mut name = file.as_os_str().to_os_string();
        name.push(end);
        let _ = std::fs::remove_file(name);
    }
}

fn connect(file: &Path) -> rusqlite::Result<Connection> {
    match prepare(file) {
        Err(error) if damaged(&error) => {
            remove(file);
            prepare(file)
        }
        outcome => outcome,
    }
}

fn prepare(file: &Path) -> rusqlite::Result<Connection> {
    create_private(file);
    let mut db = Connection::open(file)?;
    db.busy_timeout(Duration::from_secs(30))?;
    let write = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if write.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? != VERSION {
        write.execute_batch(
            "DROP TABLE IF EXISTS skills;
             CREATE TABLE skills (id INTEGER PRIMARY KEY, path TEXT UNIQUE, stamp TEXT, name TEXT, descr TEXT,
                                  problem TEXT);",
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
}
