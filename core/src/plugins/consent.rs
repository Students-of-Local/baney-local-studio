use std::collections::HashSet;
use std::path::Path;

use super::install::{live_hash, survey};
use super::{load, stage, valid_id, Capability, Catalog, Consent, Grants, Plugin, Source, Staged};
use crate::model::{ConsentNeed, ConsentRequest, ConsentRow, PluginInfo};

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
    let plugin = staged.plugin();
    let old = load(&root.join(&plugin.id)).ok();
    let asked = asked(plugin, staged.tree(), staged.origin(), &Grants::load(root)?, old.as_ref());
    if asked.is_empty() {
        let installed = staged.commit_with(root, Some((&[], &[]))).map_err(|errors| errors.join("; "))?;
        let message = summary(&installed.plugin, &installed.consent, true, 0);
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
    let (_, findings) = scanned(plugin)?;
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
    let (files, findings) = scanned(plugin).ok()?;
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
    match pending.kind {
        Kind::Install(staged) => {
            if scanned(staged.plugin())?.1 != pending.request.findings {
                return Err(format!(
                    "{} changed after the sheet opened, so nothing was installed or granted",
                    staged.plugin().id
                ));
            }
            let installed =
                staged.commit_with(root, Some((&pending.asked, &chosen))).map_err(|errors| errors.join("; "))?;
            let message = summary(&installed.plugin, &installed.consent, true, chosen.len());
            Ok(installed.note.map_or_else(|| message.clone(), |note| format!("{message}; {note}")))
        }
        Kind::Grant { plugin, tree } => {
            let _lock = Grants::lock(root)?;
            let mut grants = Grants::load(root)?;
            grants.grant(&plugin, &chosen, Some(&tree))?;
            grants.save(root)?;
            Ok(summary(&plugin, &grants.consent(&plugin), false, chosen.len()))
        }
    }
}

pub(super) fn asked(
    plugin: &Plugin,
    tree: &str,
    origin: &str,
    grants: &Grants,
    old: Option<&Plugin>,
) -> Vec<Capability> {
    let mut trial = grants.clone();
    trial.installed(plugin, tree, origin);
    let carried = trial.granted(&plugin.id).is_some_and(|(_, _, granted)| !granted.is_empty());
    plugin
        .capabilities
        .iter()
        .copied()
        .filter(|capability| !carried || old.is_none_or(|old| !old.asks(*capability)))
        .collect()
}

fn summary(plugin: &Plugin, consent: &Consent, install: bool, granted: usize) -> String {
    let count = if granted == 0 { "no new permissions".to_string() } else { format!("{granted} new permissions") };
    let done = if install {
        format!("Installed {} {} with {count}", plugin.name, plugin.version)
    } else {
        format!("{} {} has {count}", plugin.name, plugin.version)
    };
    match consent {
        Consent::Ready { .. } => match off(plugin, consent).as_slice() {
            [] => done,
            names => format!(
                "{done}; it runs without {} until you turn {} on in Settings, Plugins",
                names.join(", "),
                if names.len() == 1 { "it" } else { "them" }
            ),
        },
        Consent::NeedsConsent { missing } => {
            let names: Vec<&str> = missing
                .iter()
                .filter(|capability| !capability.sensitive())
                .map(|capability| capability.name())
                .collect();
            format!(
                "{done}, but it is off until it has {} (local-studio plugin grant {} {})",
                names.join(", "),
                plugin.id,
                names.join(" ")
            )
        }
        Consent::Refused { reason } => format!("{done}, but it is off: {reason}"),
    }
}

fn off(plugin: &Plugin, consent: &Consent) -> Vec<&'static str> {
    plugin
        .capabilities
        .iter()
        .filter(|capability| capability.sensitive() && !consent.sensitive().contains(capability))
        .map(|capability| capability.name())
        .collect()
}

#[must_use]
pub fn listing(plugin: &Plugin, consent: &Consent) -> PluginInfo {
    let granted: Vec<String> = match consent {
        Consent::Ready { sensitive } => plugin
            .capabilities
            .iter()
            .filter(|capability| !capability.sensitive() || sensitive.contains(capability))
            .map(|capability| capability.name().to_string())
            .collect(),
        Consent::NeedsConsent { missing } => plugin
            .capabilities
            .iter()
            .filter(|capability| !missing.contains(capability))
            .map(|capability| capability.name().to_string())
            .collect(),
        Consent::Refused { .. } => Vec::new(),
    };
    let status = match consent {
        Consent::Ready { .. } => match off(plugin, consent).as_slice() {
            [] => "On".to_string(),
            names => format!("On without {}", names.join(", ")),
        },
        Consent::NeedsConsent { missing } => {
            let names: Vec<&str> = missing
                .iter()
                .filter(|capability| !capability.sensitive())
                .map(|capability| capability.name())
                .collect();
            format!(
                "Off until it has {} (local-studio plugin grant {} {})",
                names.join(", "),
                plugin.id,
                names.join(" ")
            )
        }
        Consent::Refused { reason } => format!("Off: {reason}"),
    };
    PluginInfo {
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        icon: plugin.icon.as_ref().map(|icon| icon.to_string_lossy().into_owned()),
        version: plugin.version.clone(),
        description: plugin.description.clone(),
        ready: consent.ready(),
        status,
        rows: rows(plugin, &plugin.capabilities),
        granted,
    }
}

pub fn set_permission(root: &Path, id: &str, name: &str, on: bool) -> Result<String, String> {
    let capability = Capability::parse(name)
        .filter(|capability| capability.sensitive())
        .ok_or_else(|| format!("{name} is not a sensitive permission"))?;
    if !valid_id(id) {
        return Err(format!("no plugin {id:?}"));
    }
    let _lock = Grants::lock(root)?;
    let mut grants = Grants::load(root)?;
    if !on {
        grants.revoke(id, &[capability]);
        grants.save(root)?;
        return Ok(format!("{name} is off for {id}; a server already running keeps it until it starts again"));
    }
    let plugin = load(&root.join(id)).map_err(|errors| errors.join("; "))?;
    if !plugin.asks(capability) {
        return Err(format!("{} {} does not ask for {name}", plugin.id, plugin.version));
    }
    if !grants.consent(&plugin).ready() {
        return Err(format!("{} is off until it has its needed permissions, so {name} stays off", plugin.name));
    }
    let tree = grants.tree(&plugin.id).map(str::to_string);
    grants.grant(&plugin, &[capability], tree.as_deref())?;
    grants.save(root)?;
    Ok(format!("{name} is on for {}; its server gets it the next time it starts", plugin.name))
}

fn scanned(plugin: &Plugin) -> Result<(usize, Vec<String>), String> {
    let (files, findings) = survey(plugin)?;
    Ok((files, findings.into_iter().map(|path| format!("executable not named in the manifest: {path}")).collect()))
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
        findings,
        rows: rows(plugin, asked),
    }
}

fn rows(plugin: &Plugin, asked: &[Capability]) -> Vec<ConsentRow> {
    let command = plugin.mcp.as_ref().map(|mcp| {
        let name = mcp.command.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        std::iter::once(name).chain(mcp.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    });
    asked
        .iter()
        .map(|capability| ConsentRow {
            id: capability.name().to_string(),
            need: if capability.sensitive() { ConsentNeed::Sensitive } else { ConsentNeed::Needed },
            sentence: capability.sentence().to_string(),
            scope: capability.scope().replace("{command}", command.as_deref().unwrap_or("its server")),
            gates: plugin.gates.get(capability).cloned(),
        })
        .collect()
}
