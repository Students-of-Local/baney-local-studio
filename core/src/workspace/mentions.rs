use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::Semaphore;

use super::files;
use super::model::SearchHit;
use crate::model::{Mention, MentionKind};
use crate::plugins::{self, hub, Catalog, Plugin};
use crate::skills::{self, Hit, Listed, Query, Store};

const ROWS: u32 = 30;
const ABOVE: usize = 6;
const FOUND: usize = 5;
const WAIT: Duration = Duration::from_millis(300);

static LOOKUP: Semaphore = Semaphore::const_new(1);

pub(super) async fn list(
    data: PathBuf,
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
    let named = async {
        let turn = LOOKUP.acquire().await;
        let named = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            named(&data, plugins.as_deref(), project.as_deref().map(Path::new), &wanted)
        });
        named.await.unwrap_or_default()
    };
    let named = async {
        if path {
            return Vec::new();
        }
        tokio::time::timeout(WAIT, named).await.unwrap_or_default()
    };
    let (files, named) = tokio::join!(files, named);
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

fn stored(
    data: &Path,
    plugins: Option<&Path>,
    project: Option<&Path>,
    query: &str,
) -> Result<(Vec<Listed>, Vec<Hit>), String> {
    let mut store = Store::open(data, skills::roots(data, plugins, project)?)?;
    let listed: Vec<Listed> = store.list()?.into_iter().filter(|skill| skill.problem.is_none()).collect();
    let named = listed.iter().filter(|skill| skill.name.to_lowercase().contains(query)).count();
    let hits = store.search(&Query { text: query, limit: named + 2 * FOUND, ..Query::default() })?;
    Ok((listed, hits))
}

fn named(data: &Path, plugins: Option<&Path>, project: Option<&Path>, query: &str) -> Vec<Mention> {
    let installed = plugins.map(plugins::scan).unwrap_or_default().plugins;
    if query.is_empty() {
        return installed.iter().map(plugin).collect();
    }
    let (listed, hits) = stored(data, plugins, project, query).unwrap_or_default();
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
    for found in &installed {
        add(&[&found.id, &found.name], &|| plugin(found));
    }
    let offered = Catalog::bundled().map(|catalog| hub::catalog_rows(&catalog, &installed)).unwrap_or_default();
    for offer in offered.into_iter().filter(|offer| offer.installed.is_none()) {
        add(&[&offer.id, &offer.name], &|| Mention {
            badge: "Install".to_string(),
            install: offer.id.clone(),
            ..mention(MentionKind::Plugin, &offer.id, &offer.description)
        });
    }
    ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let mut rows: Vec<Mention> = ranked.into_iter().map(|(_, _, row)| row).collect();
    let hits = hits.iter().filter(|hit| listed.iter().any(|skill| skill.path == hit.path));
    let meant: Vec<Mention> = hits
        .map(|hit| skill(&hit.name, &hit.description, &hit.source))
        .filter(|row| !rows.contains(row))
        .take(FOUND)
        .collect();
    rows.extend(meant);
    rows
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
    Mention { badge: source.to_string(), ..mention(MentionKind::Skill, name, description) }
}

fn plugin(plugin: &Plugin) -> Mention {
    Mention { badge: "Plugin".to_string(), ..mention(MentionKind::Plugin, &plugin.id, &plugin.description) }
}

fn file(hit: &SearchHit) -> Mention {
    mention(MentionKind::File, &hit.relative, &hit.preview)
}
