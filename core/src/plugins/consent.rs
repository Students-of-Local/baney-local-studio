use std::collections::HashSet;
use std::path::Path;

use super::install::{live_hash, survey};
use super::{load, stage, Capability, Catalog, Grants, Plugin, Source, Staged};
use crate::model::{ConsentNeed, ConsentRequest, ConsentRow};

pub enum Offer {
    Installed(String),
    Ask(Box<Pending>),
}

pub struct Pending {
    request: ConsentRequest,
    asked: Vec<Capability>,
    kind: Kind,
}

enum Kind {
    Install(Staged),
    Grant { plugin: Plugin, tree: String },
}

#[derive(Default)]
pub struct Pendings {
    items: Vec<Pending>,
    dismissed: HashSet<String>,
}

impl Pendings {
    #[must_use]
    pub fn requests(&self) -> Vec<ConsentRequest> {
        self.items.iter().map(|pending| pending.request.clone()).collect()
    }

    #[must_use]
    pub fn asks(&self, request: &str) -> bool {
        self.dismissed.contains(request) || self.items.iter().any(|pending| pending.request.request == request)
    }

    #[must_use]
    pub fn offered(&self, plugin: &Plugin) -> bool {
        self.asks(&grant_request(plugin)) || self.items.iter().any(|pending| pending.request.plugin == plugin.id)
    }

    pub fn push(&mut self, pending: Pending) -> bool {
        if self.asks(&pending.request.request) {
            return false;
        }
        self.items.push(pending);
        true
    }

    pub fn take(&mut self, request: &str) -> Option<Pending> {
        let index = self.items.iter().position(|pending| pending.request.request == request)?;
        let pending = self.items.remove(index);
        if matches!(pending.kind, Kind::Grant { .. }) {
            self.dismissed.insert(pending.request.request.clone());
        }
        Some(pending)
    }
}

impl Pending {
    #[must_use]
    pub fn request(&self) -> &ConsentRequest {
        &self.request
    }
}

pub fn offer_install(root: &Path, arg: &str, request: String) -> Result<Offer, String> {
    let source = Source::parse(arg)?;
    let catalog = Catalog::bundled()?;
    let staged = stage(root, &source, &catalog).map_err(|errors| errors.join("; "))?;
    let grants = Grants::load(root)?;
    let plugin = staged.plugin();
    let mut trial = grants.clone();
    trial.installed(plugin, staged.tree(), staged.origin());
    let carried = trial.granted(&plugin.id).is_some_and(|(_, _, granted)| !granted.is_empty());
    let old = load(&root.join(&plugin.id)).ok();
    let asked: Vec<Capability> = plugin
        .capabilities
        .iter()
        .copied()
        .filter(|capability| !carried || old.as_ref().is_none_or(|old| !old.asks(*capability)))
        .collect();
    if asked.is_empty() {
        let installed = staged.commit(root).map_err(|errors| errors.join("; "))?;
        let plugin = &installed.plugin;
        let message = format!("Installed {} {} with no new permissions", plugin.name, plugin.version);
        return Ok(Offer::Installed(
            installed.note.map_or_else(|| message.clone(), |note| format!("{message}; {note}")),
        ));
    }
    let (from, pin) = match &source {
        Source::Dir(_) => ("a local folder".to_string(), None),
        Source::Git { url, rev } => (url.clone(), Some(rev.clone())),
        Source::Catalog(id) => {
            ("the Local Studio catalog".to_string(), catalog.entry(id).map(|entry| entry.rev.clone()))
        }
    };
    let (_, findings) = survey(plugin)?;
    let mut request = sheet(request, plugin, (from, pin), staged.files(), findings, &asked);
    request.install = true;
    request.update_from = old.map(|old| old.version);
    Ok(Offer::Ask(Box::new(Pending { request, asked, kind: Kind::Install(staged) })))
}

#[must_use]
pub fn offer_grant(plugin: &Plugin, missing: &[Capability], grants: &Grants) -> Option<Pending> {
    if !missing.iter().any(|capability| !capability.sensitive()) {
        return None;
    }
    let tree = live_hash(&plugin.dir).ok()?;
    let (files, findings) = survey(plugin).ok()?;
    let from = match grants.granted(&plugin.id).map(|(_, source, _)| source).unwrap_or_default() {
        source if source.starts_with("catalog ") => "the Local Studio catalog".to_string(),
        source if source.starts_with("git ") => source.trim_start_matches("git ").to_string(),
        "" => "a folder Local Studio did not install".to_string(),
        _ => "a local folder".to_string(),
    };
    let request = sheet(grant_request(plugin), plugin, (from, None), files, findings, missing);
    Some(Pending { request, asked: missing.to_vec(), kind: Kind::Grant { plugin: plugin.clone(), tree } })
}

pub fn answer(root: &Path, pending: Pending, sensitive: &[String]) -> Result<String, String> {
    let mut chosen: Vec<Capability> = pending.asked.iter().copied().filter(|asked| !asked.sensitive()).collect();
    for name in sensitive {
        match Capability::parse(name) {
            Some(capability) if capability.sensitive() && pending.asked.contains(&capability) => {
                chosen.push(capability);
            }
            _ => return Err(format!("{name} is not a sensitive permission this sheet asked for")),
        }
    }
    let (plugin, note) = match pending.kind {
        Kind::Install(staged) => {
            let installed = staged.commit(root).map_err(|errors| errors.join("; "))?;
            (installed.plugin, installed.note)
        }
        Kind::Grant { plugin, tree } => {
            if live_hash(&plugin.dir)? != tree {
                return Err(format!("{} changed after the sheet opened, so nothing was granted", plugin.id));
            }
            (plugin, None)
        }
    };
    if !chosen.is_empty() {
        let mut grants = Grants::load(root)?;
        grants.grant(&plugin, &chosen)?;
        grants.save(root)?;
    }
    let message = format!("{} {} has {} new permissions", plugin.name, plugin.version, chosen.len());
    Ok(note.map_or_else(|| message.clone(), |note| format!("{message}; {note}")))
}

fn grant_request(plugin: &Plugin) -> String {
    format!("consent:{}@{}", plugin.id, plugin.version)
}

fn sheet(
    request: String,
    plugin: &Plugin,
    (source, pin): (String, Option<String>),
    files: usize,
    findings: Vec<String>,
    asked: &[Capability],
) -> ConsentRequest {
    let command = plugin.mcp.as_ref().map(|mcp| {
        let name = mcp.command.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        std::iter::once(name).chain(mcp.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    });
    let rows = asked
        .iter()
        .map(|capability| ConsentRow {
            id: capability.name().to_string(),
            need: if capability.sensitive() { ConsentNeed::Sensitive } else { ConsentNeed::Needed },
            sentence: capability.sentence().to_string(),
            scope: capability.scope().replace("{command}", command.as_deref().unwrap_or("its server")),
            gates: plugin.gates.get(capability).cloned(),
        })
        .collect();
    ConsentRequest {
        request,
        plugin: plugin.id.clone(),
        name: plugin.name.clone(),
        icon: plugin.icon.as_ref().map(|icon| icon.to_string_lossy().into_owned()),
        description: plugin.description.clone(),
        version: plugin.version.clone(),
        install: false,
        update_from: None,
        source,
        pin: pin.map(|rev| rev.chars().take(7).collect()),
        files: files as u64,
        findings: findings.into_iter().map(|path| format!("executable not named in the manifest: {path}")).collect(),
        rows,
    }
}
