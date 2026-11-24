use std::path::{Path, PathBuf};
use std::time::Duration;

use super::files;
use super::model::SearchHit;
use crate::model::{Mention, MentionKind};
use crate::plugins::{self, hub, Catalog, Plugin};
use crate::skills::{self, Query, Store};

const ROWS: u32 = 30;
const ABOVE: usize = 6;
const FOUND: usize = 5;
const WAIT: Duration = Duration::from_millis(300);

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
    let named = tokio::task::spawn_blocking(move || {
        if path {
            return Ok(Vec::new());
        }
        named(&data, plugins.as_deref(), project.as_deref().map(Path::new), &wanted)
    });
    let (files, named) = tokio::join!(files, tokio::time::timeout(WAIT, named));
    let files = files.map_err(|error| error.to_string())??;
    let named = match named {
        Ok(named) => named.map_err(|error| error.to_string())??,
        Err(_) => Vec::new(),
    };
    let limit = limit as usize;
    Ok(if query.trim().is_empty() { files_first(&files, named, limit) } else { named_first(&files, named, limit) })
}

fn files_first(files: &[SearchHit], named: Vec<Mention>, limit: usize) -> Vec<Mention> {
    let room = limit.saturating_sub(named.len().min(ABOVE));
    files.iter().take(room).map(file).chain(named).take(limit).collect()
}

fn named_first(files: &[SearchHit], named: Vec<Mention>, limit: usize) -> Vec<Mention> {
    let most = if files.is_empty() { limit } else { ABOVE };
    named.into_iter().take(most).chain(files.iter().map(file)).take(limit).collect()
}

fn named(data: &Path, plugins: Option<&Path>, project: Option<&Path>, query: &str) -> Result<Vec<Mention>, String> {
    let installed = plugins.map(plugins::scan).unwrap_or_default().plugins;
    if query.is_empty() {
        return Ok(installed.iter().map(plugin).collect());
    }
    let mut store = Store::open(data, skills::roots(data, plugins, project)?)?;
    let listed: Vec<_> = store.list()?.into_iter().filter(|skill| skill.problem.is_none()).collect();
    let mut ranked: Vec<(bool, String, Mention)> = Vec::new();
    let mut add = |names: &[&str], row: &dyn Fn() -> Mention| {
        let names: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
        if names.iter().any(|name| name.contains(query)) {
            ranked.push((!names.iter().any(|name| name.starts_with(query)), names.concat(), row()));
        }
    };
    for found in &listed {
        add(&[&found.name], &|| skill(&found.name, &found.description, &found.source));
    }
    for found in &installed {
        add(&[&found.id, &found.name], &|| plugin(found));
    }
    let offered = hub::catalog_rows(&Catalog::bundled()?, &installed);
    for offer in offered.into_iter().filter(|offer| offer.installed.is_none()) {
        add(&[&offer.id, &offer.name], &|| Mention {
            badge: "Install".to_string(),
            install: offer.id.clone(),
            ..mention(MentionKind::Plugin, &offer.id, &offer.description)
        });
    }
    ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let mut rows: Vec<Mention> = ranked.into_iter().map(|(_, _, row)| row).collect();
    let hits = store.search(&Query { text: query, limit: FOUND, ..Query::default() })?;
    for hit in hits.iter().filter(|hit| listed.iter().any(|skill| skill.path == hit.path)) {
        let row = skill(&hit.name, &hit.description, &hit.source);
        if !rows.contains(&row) {
            rows.push(row);
        }
    }
    Ok(rows)
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
