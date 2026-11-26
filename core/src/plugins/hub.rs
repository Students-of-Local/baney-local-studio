use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use super::install::peek;
use super::{
    resolve_channel, Capability, Catalog, Consent, Grants, Origins, Plugin, PluginFailure, Source, Standing, MANIFEST,
};
use crate::model::{CatalogPlugin, ConsentNeed, PluginEntry, PluginGrant, PluginRow, PluginState, PluginUpdate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub rev: String,
    pub version: String,
    pub asks: Vec<String>,
    pub reference: String,
    pub replaced_by: Option<String>,
}

fn short(rev: &str) -> String {
    rev.chars().take(7).collect()
}

fn grant_row(plugin: Option<&Plugin>, capability: Capability, granted: bool) -> PluginGrant {
    PluginGrant {
        id: capability.name().to_string(),
        need: if capability.sensitive() { ConsentNeed::Sensitive } else { ConsentNeed::Needed },
        sentence: capability.sentence().to_string(),
        gates: plugin.and_then(|plugin| plugin.gates.get(&capability).cloned()),
        granted,
    }
}

fn sentences(capabilities: &[Capability]) -> String {
    capabilities.iter().map(|capability| capability.sentence().to_lowercase()).collect::<Vec<_>>().join(", ")
}

fn state(plugin: &Plugin, consent: &Consent, held: &[Capability], taken: &[String]) -> (PluginState, String) {
    match consent {
        Consent::Refused { reason } => (PluginState::Failed, format!("It cannot run: {reason}")),
        Consent::NeedsConsent { missing } => {
            (PluginState::Waiting, format!("Waiting for you to allow it to {}", sentences(missing)))
        }
        Consent::Ready if plugin.asks(Capability::McpServer) && taken.contains(&plugin.mcp_name()) => (
            PluginState::Off,
            format!(
                "Its tools are off: settings.json already has an MCP server named {} that Local Studio did not write",
                plugin.mcp_name()
            ),
        ),
        Consent::Ready => {
            let off: Vec<Capability> = plugin
                .capabilities
                .iter()
                .copied()
                .filter(|capability| capability.sensitive() && !held.contains(capability))
                .collect();
            let base = match (plugin.mcp.is_some(), plugin.local_only) {
                (true, true) => "Its tools run in threads on local models only".to_string(),
                (true, false) => "Its tools are on in every thread".to_string(),
                (false, true) => "Its commands run on local models only".to_string(),
                (false, false) => "Its commands and skills are on".to_string(),
            };
            if off.is_empty() {
                (PluginState::Running, base)
            } else {
                (PluginState::Running, format!("{base}; it cannot {} until you turn that on", sentences(&off)))
            }
        }
    }
}

fn source_label(source: &str, folder: &Path) -> String {
    if source.starts_with("catalog ") {
        "Local Studio catalog".to_string()
    } else if let Some(url) = source.strip_prefix("git ") {
        url.to_string()
    } else if let Some(path) = source.strip_prefix("folder ") {
        path.to_string()
    } else {
        format!("Copied into {}", folder.display())
    }
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn rows(
    root: &Path,
    ready: &[Standing],
    failures: &[PluginFailure],
    grants: &Grants,
    origins: &Origins,
    updates: &BTreeMap<String, Available>,
    taken: &[String],
    channel: &str,
) -> Vec<PluginRow> {
    let mut rows: Vec<PluginRow> = ready
        .iter()
        .map(|(plugin, consent, held)| {
            let (state, detail) = state(plugin, consent, held, taken);
            let detail = match updates.get(&plugin.id).and_then(|available| available.replaced_by.as_ref()) {
                Some(name) => format!("{detail}; it is replaced by {name}, and updating installs that"),
                None => detail,
            };
            let recorded = grants.granted(&plugin.id).map(|(_, source, _)| source.to_string()).unwrap_or_default();
            let origin = origins.get(&plugin.id);
            let granted = |capability: Capability| match consent {
                Consent::Ready => !capability.sensitive() || held.contains(&capability),
                _ => held.contains(&capability),
            };
            PluginRow {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                description: plugin.description.clone(),
                icon: plugin.icon.as_ref().map(|icon| icon.to_string_lossy().into_owned()),
                state,
                detail,
                grants: plugin
                    .capabilities
                    .iter()
                    .map(|capability| grant_row(Some(plugin), *capability, granted(*capability)))
                    .collect(),
                told: plugin.told(),
                commands: plugin
                    .commands
                    .iter()
                    .map(|command| PluginEntry { name: command.name.clone(), description: command.description.clone() })
                    .collect(),
                skills: plugin
                    .skills
                    .iter()
                    .map(|skill| PluginEntry { name: skill.name.clone(), description: skill.description.clone() })
                    .collect(),
                source: source_label(&recorded, root),
                revision: origin.and_then(|origin| origin.rev.as_deref()).map(short),
                pinned: origin.is_some_and(|origin| origin.pinned),
                channel: origin.and_then(|origin| origin.git.as_ref()).map(|_| channel.to_string()),
                update: updates.get(&plugin.id).map(|available| PluginUpdate {
                    version: available.version.clone(),
                    revision: short(&available.rev),
                    asks: available.asks.clone(),
                }),
                folder: super::absolute(&plugin.dir).to_string_lossy().into_owned(),
                local_only: plugin.local_only,
            }
        })
        .collect();
    rows.extend(failures.iter().map(|failure| {
        let id = failure.dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        PluginRow {
            name: id.clone(),
            id,
            version: String::new(),
            description: String::new(),
            icon: None,
            state: PluginState::Failed,
            detail: format!("It did not load: {}", failure.errors.join("; ")),
            grants: Vec::new(),
            told: false,
            commands: Vec::new(),
            skills: Vec::new(),
            source: String::new(),
            revision: None,
            pinned: false,
            channel: None,
            update: None,
            folder: super::absolute(&failure.dir).to_string_lossy().into_owned(),
            local_only: false,
        }
    }));
    rows
}

#[must_use]
pub fn catalog_rows(catalog: &Catalog, plugins: &[Plugin]) -> Vec<CatalogPlugin> {
    catalog
        .plugins
        .iter()
        .map(|entry| CatalogPlugin {
            id: entry.id.clone(),
            name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            description: entry.description.clone(),
            grants: entry
                .capabilities
                .iter()
                .filter_map(|name| Capability::parse(name))
                .map(|capability| grant_row(None, capability, false))
                .collect(),
            source: entry.git.clone(),
            revision: short(&entry.rev),
            local_only: entry.local_only,
            installed: plugins.iter().find(|plugin| plugin.id == entry.id).map(|plugin| plugin.version.clone()),
        })
        .collect()
}

fn version(text: &str) -> Option<(Vec<u64>, Vec<String>)> {
    let core = text.split_once('+').map_or(text, |(core, _)| core);
    let (numbers, pre) = core.split_once('-').map_or((core, None), |(numbers, pre)| (numbers, Some(pre)));
    let numbers: Vec<u64> = numbers.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    Some((numbers, pre.map(|pre| pre.split('.').map(str::to_string).collect()).unwrap_or_default()))
}

fn compare(new: &str, old: &str) -> Option<Ordering> {
    let (new_numbers, new_pre) = version(new)?;
    let (old_numbers, old_pre) = version(old)?;
    let identifier = |a: &String, b: &String| match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    };
    let pre = match (new_pre.is_empty(), old_pre.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => new_pre
            .iter()
            .zip(&old_pre)
            .map(|(a, b)| identifier(a, b))
            .find(|order| order.is_ne())
            .unwrap_or_else(|| new_pre.len().cmp(&old_pre.len())),
    };
    Some(new_numbers.cmp(&old_numbers).then(pre))
}

#[must_use]
pub fn check(root: &Path, plugins: &[Plugin], channel: &str) -> (BTreeMap<String, Available>, Vec<String>) {
    let catalog = Catalog::bundled();
    let origins = Origins::load(root).unwrap_or_default();
    let mut found: BTreeMap<String, Available> = BTreeMap::new();
    let mut errors: Vec<String> = Vec::new();
    for plugin in plugins {
        let Some(origin) = origins.get(&plugin.id) else { continue };
        if origin.pinned || catalog.as_ref().is_ok_and(|catalog| catalog.replacing(&plugin.id).is_some()) {
            if let Some(available) = catalog_update(plugin, origin.rev.as_deref(), &catalog, &mut errors) {
                found.insert(plugin.id.clone(), available);
            }
            continue;
        }
        let Some(url) = origin.git.as_deref() else { continue };
        let resolved = match resolve_channel(url, channel) {
            Ok(resolved) => resolved,
            Err(error) => {
                errors.push(format!("{}: {error}", plugin.name));
                continue;
            }
        };
        let (reference, rev) = resolved;
        if origin.rev.as_deref() == Some(rev.as_str()) {
            continue;
        }
        let peeked = match peek(url, &rev, origin.subdir.as_deref(), origin.rev.as_deref()) {
            Ok(peeked) => peeked,
            Err(error) => {
                errors.push(format!("{} on {reference}: {error}", plugin.name));
                continue;
            }
        };
        if !peeked.id.is_empty() && peeked.id != plugin.id {
            errors.push(format!("{} on {reference}: its {MANIFEST} names {} instead", plugin.name, peeked.id));
            continue;
        }
        match compare(&peeked.version, &plugin.version) {
            Some(Ordering::Greater) => {}
            Some(Ordering::Equal) if peeked.changed => {}
            _ => continue,
        }
        let asks: Vec<String> = peeked
            .capabilities
            .iter()
            .filter(|name| Capability::parse(name).is_none_or(|capability| !plugin.asks(capability)))
            .cloned()
            .collect();
        found.insert(plugin.id.clone(), Available { rev, version: peeked.version, asks, reference, replaced_by: None });
    }
    (found, errors)
}

fn catalog_update(
    plugin: &Plugin,
    installed: Option<&str>,
    catalog: &Result<Catalog, String>,
    errors: &mut Vec<String>,
) -> Option<Available> {
    let catalog = match catalog {
        Ok(catalog) => catalog,
        Err(error) => {
            errors.push(error.clone());
            return None;
        }
    };
    let replacing = catalog.replacing(&plugin.id);
    let entry = replacing.or_else(|| catalog.entry(&plugin.id))?;
    if replacing.is_none() && installed == Some(entry.rev.as_str()) {
        return None;
    }
    let peeked = match peek(&entry.git, &entry.rev, entry.subdir.as_deref(), None) {
        Ok(peeked) => peeked,
        Err(error) => {
            errors.push(format!("{} in the catalog: {error}", plugin.name));
            return None;
        }
    };
    let asks = peeked
        .capabilities
        .iter()
        .filter(|name| replacing.is_some() || Capability::parse(name).is_none_or(|capability| !plugin.asks(capability)))
        .cloned()
        .collect();
    Some(Available {
        rev: entry.rev.clone(),
        version: peeked.version,
        asks,
        reference: "catalog".to_string(),
        replaced_by: replacing.map(|entry| entry.name.clone().unwrap_or_else(|| entry.id.clone())),
    })
}

pub fn follow(root: &Path, id: &str, channel: &str) -> Result<Source, String> {
    let origins = Origins::load(root)?;
    let origin = origins.get(id).ok_or_else(|| format!("{id} was not installed from git, so it has no updates"))?;
    if origin.pinned {
        let catalog = Catalog::bundled()?;
        let entry = catalog.entry(id).ok_or_else(|| format!("{id} is no longer in the catalog"))?;
        if origin.rev.as_deref() == Some(entry.rev.as_str()) {
            return Err(format!("{id} is already at the catalog's pin ({})", short(&entry.rev)));
        }
        return Ok(Source::Catalog(id.to_string()));
    }
    let url = origin.git.clone().ok_or_else(|| format!("{id} was installed from a folder, so it has no updates"))?;
    let (reference, rev) = resolve_channel(&url, channel)?;
    if origin.rev.as_deref() == Some(rev.as_str()) {
        return Err(format!("{id} is already at {reference} ({})", short(&rev)));
    }
    let recorded = Grants::load(root)?.granted(id).map(|(_, source, _)| source.to_string()).unwrap_or_default();
    let label = if recorded.is_empty() { format!("git {url}") } else { recorded };
    Ok(Source::Follow { origin: label, url, rev, subdir: origin.subdir.clone(), exclude: origin.exclude.clone() })
}

pub fn remove(root: &Path, id: &str) -> Result<String, String> {
    if !super::valid_id(id) {
        return Err(format!("not a plugin id: {id}"));
    }
    let _lock = Grants::lock(root)?;
    let dir = root.join(id);
    let meta = std::fs::symlink_metadata(&dir).map_err(|error| format!("{id} is not installed: {error}"))?;
    let removed = if meta.file_type().is_symlink() {
        std::fs::remove_file(&dir)
    } else if meta.is_dir() {
        std::fs::remove_dir_all(&dir)
    } else {
        return Err(format!("{} is not a plugin folder", dir.display()));
    };
    removed.map_err(|error| format!("cannot remove {}: {error}", dir.display()))?;
    let mut grants = Grants::load(root)?;
    if grants.forget(id) {
        grants.save(root)?;
    }
    let mut origins = Origins::load(root)?;
    if origins.forget(id) {
        origins.save(root)?;
    }
    Ok(format!("Removed {id} and its permissions"))
}

pub(crate) fn retire(root: &Path, old: &str, new: &Plugin) -> Result<String, String> {
    if old == new.id {
        return Err(format!("{old} cannot replace itself"));
    }
    remove(root, old)?;
    let own = if new.capabilities.is_empty() { String::new() } else { format!(", and {} asks for its own", new.id) };
    Ok(format!("replaced {old}; its permissions went with it{own}"))
}

pub fn revoke(root: &Path, id: &str, capability: &str) -> Result<String, String> {
    let capability = Capability::parse(capability).ok_or_else(|| format!("unknown permission: {capability}"))?;
    let _lock = Grants::lock(root)?;
    let mut grants = Grants::load(root)?;
    if !grants.revoke(id, &[capability]) {
        return Err(format!("{id} does not hold {}", capability.name()));
    }
    grants.save(root)?;
    Ok(format!("{id} can no longer {}", capability.sentence().to_lowercase()))
}
