use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{oneshot, Semaphore};

use super::model::SearchHit;
use super::{files, Workspace};
use crate::model::{Mention, MentionKind};
use crate::plugins::{self, hub, Catalog, Plugin};
use crate::skills::{Hit, Listed, Query};

const ROWS: u32 = 30;
const ABOVE: usize = 6;
const FOUND: usize = 5;
const CREATE: &str = "create";
const WAIT: Duration = Duration::from_millis(300);

static LOOKUP: Semaphore = Semaphore::const_new(1);
static LATEST: AtomicU64 = AtomicU64::new(0);

type Found = (Vec<Listed>, Vec<Hit>);

pub(super) fn turn() -> u64 {
    LATEST.fetch_add(1, Ordering::Relaxed) + 1
}

pub(super) async fn list(
    asked: u64,
    workspace: Workspace,
    plugins: Option<PathBuf>,
    project: Option<String>,
    query: String,
    limit: u32,
) -> Result<Vec<Mention>, String> {
    let limit = if limit == 0 { ROWS } else { limit };
    let (root, wanted) = (project.clone(), query.clone());
    let files = tokio::task::spawn_blocking(move || {
        root.map_or_else(|| Ok(Vec::new()), |root| files::search_files(&root, &wanted, limit))
    });
    let path = query.contains(['/', '.']);
    let wanted = query.trim().to_lowercase();
    let installed = plugins.clone();
    let installed = tokio::task::spawn_blocking(move || installed.as_deref().map(plugins::scan).unwrap_or_default());
    let stored = async {
        if path || wanted.is_empty() {
            return Found::default();
        }
        tokio::time::timeout(WAIT, lookup(asked, workspace, plugins, project, wanted.clone())).await.unwrap_or_default()
    };
    let (files, installed, stored) = tokio::join!(files, installed, stored);
    let named = if path { Vec::new() } else { named(&installed.unwrap_or_default().plugins, &stored, &wanted) };
    let files = files.map_err(|error| error.to_string())??;
    let limit = limit as usize;
    Ok(if query.trim().is_empty() { files_first(&files, named, limit) } else { named_first(&files, named, limit) })
}

fn above(limit: usize) -> usize {
    ABOVE.min(limit / 2)
}

fn files_first(files: &[SearchHit], named: Vec<Mention>, limit: usize) -> Vec<Mention> {
    let room = limit.saturating_sub(named.len().min(above(limit)));
    files.iter().take(room).map(file).chain(named).take(limit).collect()
}

fn named_first(files: &[SearchHit], named: Vec<Mention>, limit: usize) -> Vec<Mention> {
    let most = if files.is_empty() { limit } else { above(limit) };
    named.into_iter().take(most).chain(files.iter().map(file)).take(limit).collect()
}

async fn lookup(
    asked: u64,
    workspace: Workspace,
    plugins: Option<PathBuf>,
    project: Option<String>,
    query: String,
) -> Found {
    let turn = LOOKUP.acquire().await;
    if LATEST.load(Ordering::Relaxed) != asked {
        return Found::default();
    }
    let (tell, told) = oneshot::channel();
    std::thread::spawn(move || {
        let found = stored(&workspace, plugins.as_deref(), project.as_deref(), &query);
        drop(turn);
        tell.send(found).ok();
    });
    told.await.ok().and_then(Result::ok).unwrap_or_default()
}

fn stored(workspace: &Workspace, plugins: Option<&Path>, project: Option<&str>, query: &str) -> Result<Found, String> {
    let (mut store, listed) = workspace.store_skills(plugins, project)?;
    let named = listed.iter().filter(|skill| skill.name.to_lowercase().contains(query)).count();
    let hits = store.search(&Query { text: query, limit: named + 2 * FOUND, ..Query::default() }).unwrap_or_default();
    Ok((listed, hits))
}

fn named(installed: &[Plugin], (listed, hits): &Found, query: &str) -> Vec<Mention> {
    if query.is_empty() {
        return installed.iter().map(plugin).collect();
    }
    let listed: Vec<&Listed> = listed.iter().filter(|skill| !skill.behind(installed)).collect();
    let first = CREATE.starts_with(query).then(|| Mention {
        badge: "Create".to_string(),
        ..mention(MentionKind::Skill, CREATE, "Make a skill from this thread")
    });
    let mut ranked: Vec<(usize, String, Mention)> = Vec::new();
    let mut add = |names: &[&str], row: &dyn Fn() -> Mention| {
        let holding = names.iter().map(|name| name.to_lowercase()).filter_map(|name| {
            let rank = if name == query { 0 } else { 1 + usize::from(!name.starts_with(query)) };
            name.contains(query).then_some((rank, name))
        });
        if let Some((rank, name)) = holding.min() {
            ranked.push((rank, name, row()));
        }
    };
    for found in &listed {
        add(&[&found.name], &|| skill(&found.name, &found.description, &found.source));
    }
    for found in installed {
        add(&[&found.id, &found.name], &|| plugin(found));
    }
    let offered = Catalog::bundled().map(|catalog| hub::catalog_rows(&catalog, installed)).unwrap_or_default();
    for offer in offered.into_iter().filter(|offer| offer.installed.is_none()) {
        add(&[&offer.id, &offer.name], &|| Mention {
            badge: "Install".to_string(),
            install: offer.id.clone(),
            ..mention(MentionKind::Plugin, &offer.id, &offer.description)
        });
    }
    ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let unnamed =
        |hit: &Hit| listed.iter().find(|skill| skill.path == hit.path && !skill.name.to_lowercase().contains(query));
    let meant = hits.iter().filter_map(unnamed).take(FOUND);
    let meant = meant.map(|found| skill(&found.name, &found.description, &found.source));
    first.into_iter().chain(ranked.into_iter().map(|(_, _, row)| row)).chain(meant).collect()
}

fn mention(kind: MentionKind, name: &str, detail: &str) -> Mention {
    Mention {
        kind,
        title: format!("@{name}"),
        detail: detail.to_string(),
        badge: String::new(),
        insert: format!("@{name} "),
        install: String::new(),
    }
}

fn skill(name: &str, description: &str, source: &str) -> Mention {
    let badge = match (source, source.split_at_checked(1)) {
        ("shipped", _) => "Local Studio".to_string(),
        (_, Some((first, rest))) => first.to_uppercase() + rest,
        _ => String::new(),
    };
    let row = Mention { badge, ..mention(MentionKind::Skill, name, description) };
    if name == CREATE {
        return Mention { title: format!("${name}"), insert: format!("${name} "), ..row };
    }
    row
}

fn plugin(plugin: &Plugin) -> Mention {
    Mention { badge: "Plugin".to_string(), ..mention(MentionKind::Plugin, &plugin.id, &plugin.description) }
}

fn file(hit: &SearchHit) -> Mention {
    mention(MentionKind::File, &hit.relative, &hit.preview)
}
