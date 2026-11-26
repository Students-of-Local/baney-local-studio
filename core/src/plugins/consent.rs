use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::compat::{report, text_findings, Translation};
use super::install::{live_hash, survey};
use super::{load, stage, Capability, Catalog, Consent, Grants, Plugin, Source, Staged};
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
    running: HashMap<String, String>,
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

    pub fn begin(&mut self, request: &str, id: &str) {
        self.running.insert(request.to_string(), id.to_string());
    }

    pub fn end(&mut self, request: &str) {
        self.running.remove(request);
    }

    #[must_use]
    pub fn installing(&self) -> Vec<String> {
        let open = self.items.iter().filter(|pending| pending.installs()).map(|pending| &pending.request.plugin);
        self.running.values().chain(open).cloned().collect()
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
        if matches!(pending.kind, Kind::Grant { .. }) && pending.request.request.starts_with(AUTO) {
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

    #[must_use]
    pub fn installs(&self) -> bool {
        self.request.install && self.request.update_from.is_none()
    }

    pub fn preset(&mut self, names: &[String]) {
        self.request.preset = self
            .asked
            .iter()
            .filter(|capability| capability.sensitive() && names.iter().any(|name| name == capability.name()))
            .map(|capability| capability.name().to_string())
            .collect();
    }
}

const AUTO: &str = "consent:";

pub fn offer_install(root: &Path, arg: &str, request: String) -> Result<Offer, String> {
    let (source, _) = Source::resolve(arg, crate::CHANNEL)?;
    offer_source(root, &source, request)
}

pub fn offer_update(root: &Path, id: &str, channel: &str, request: String) -> Result<Offer, String> {
    let catalog = Catalog::bundled()?;
    match catalog.replacing(id) {
        Some(entry) => offer_replacement(root, id, &entry.id, &catalog, request),
        None => offer_staged(root, &super::hub::follow(root, id, channel)?, &catalog, request, None),
    }
}

pub fn offer_replacement(
    root: &Path,
    old: &str,
    new: &str,
    catalog: &Catalog,
    request: String,
) -> Result<Offer, String> {
    if let Ok(plugin) = load(&root.join(new)) {
        let retired = super::hub::retire(root, old, &plugin)?;
        return Ok(Offer::Installed(format!("{} {} was already installed; {retired}", plugin.name, plugin.version)));
    }
    offer_staged(root, &Source::Catalog(new.to_string()), catalog, request, Some(old))
}

pub fn offer_source(root: &Path, source: &Source, request: String) -> Result<Offer, String> {
    offer_staged(root, source, &Catalog::bundled()?, request, None)
}

fn offer_staged(
    root: &Path,
    source: &Source,
    catalog: &Catalog,
    request: String,
    replaces: Option<&str>,
) -> Result<Offer, String> {
    let mut staged = stage(root, source, catalog).map_err(|errors| errors.join("; "))?;
    staged.replaces = replaces.map(str::to_string);
    let plugin = staged.plugin();
    let old = load(&root.join(&plugin.id)).ok();
    let asked = asked(plugin, staged.tree(), staged.origin(), &Grants::load(root)?, old.as_ref());
    let translation = staged.translation();
    let shown = notable(plugin, translation.as_ref());
    let new = match &old {
        None => translation.is_some() || !shown.is_empty(),
        Some(old) => {
            let before = notable(old, report(&old.dir).as_ref());
            shown.iter().any(|item| !before.contains(item))
        }
    };
    if asked.is_empty() && !new {
        let installed = staged.commit_with(root, Some((&[], &[]))).map_err(|errors| errors.join("; "))?;
        let message = summary(&installed.plugin, &installed.consent, Some(old.is_some()), 0);
        return Ok(Offer::Installed(
            installed.note.map_or_else(|| message.clone(), |note| format!("{message}; {note}")),
        ));
    }
    let (from, pin) = match source {
        Source::Dir(_) => ("a local folder".to_string(), None),
        Source::Git { url, rev } | Source::Follow { url, rev, .. } => (url.clone(), Some(rev.clone())),
        Source::Catalog(id) => {
            ("the Local Studio catalog".to_string(), catalog.entry(id).map(|entry| entry.rev.clone()))
        }
    };
    let (_, findings) = scanned(plugin, translation.as_ref())?;
    let mut request = sheet(request, plugin, (from, pin), staged.files(), findings, &asked, translation.as_ref());
    request.install = true;
    request.unpinned = !source.pinned();
    request.update_from = old.map(|old| old.version);
    Ok(Offer::Ask(Box::new(Pending { request, asked, kind: Kind::Install(staged) })))
}

#[must_use]
pub fn offer_grant(plugin: &Plugin, missing: &[Capability], grants: &Grants) -> Option<Pending> {
    if !missing.iter().any(|capability| !capability.sensitive()) {
        return None;
    }
    offer_capabilities(plugin, missing, grants, grant_request(plugin)).ok()
}

pub fn offer_capabilities(
    plugin: &Plugin,
    missing: &[Capability],
    grants: &Grants,
    request: String,
) -> Result<Pending, String> {
    if let Some(extra) = missing.iter().find(|capability| !plugin.asks(**capability)) {
        return Err(format!("{} does not ask for {}", plugin.name, extra.name()));
    }
    if missing.is_empty() {
        return Err(format!("{} asks for nothing more", plugin.name));
    }
    let tree = live_hash(&plugin.dir)?;
    let translation = report(&plugin.dir);
    let (files, findings) = scanned(plugin, translation.as_ref())?;
    let from = match grants.granted(&plugin.id).map(|(_, source, _)| source).unwrap_or_default() {
        source if source.starts_with("catalog ") => "the Local Studio catalog".to_string(),
        source if source.starts_with("git ") => source.trim_start_matches("git ").to_string(),
        "" => "a folder Local Studio did not install".to_string(),
        _ => "a local folder".to_string(),
    };
    let request = sheet(request, plugin, (from, None), files, findings, missing, translation.as_ref());
    Ok(Pending { request, asked: missing.to_vec(), kind: Kind::Grant { plugin: plugin.clone(), tree } })
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
            if scanned(staged.plugin(), staged.translation().as_ref())?.1 != pending.request.findings {
                return Err(format!(
                    "{} changed after the sheet opened, so nothing was installed or granted",
                    staged.plugin().id
                ));
            }
            let installed =
                staged.commit_with(root, Some((&pending.asked, &chosen))).map_err(|errors| errors.join("; "))?;
            let message = summary(
                &installed.plugin,
                &installed.consent,
                Some(pending.request.update_from.is_some()),
                chosen.len(),
            );
            Ok(installed.note.map_or_else(|| message.clone(), |note| format!("{message}; {note}")))
        }
        Kind::Grant { plugin, tree } => {
            let _lock = Grants::lock(root)?;
            let mut grants = Grants::load(root)?;
            grants.grant(&plugin, &chosen, Some(&tree))?;
            grants.save(root)?;
            Ok(summary(&plugin, &grants.consent(&plugin), None, chosen.len()))
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

fn summary(plugin: &Plugin, consent: &Consent, install: Option<bool>, granted: usize) -> String {
    let count = match granted {
        0 => "no new permissions".to_string(),
        1 => "1 new permission".to_string(),
        granted => format!("{granted} new permissions"),
    };
    let done = match install {
        Some(false) => format!("Installed {} {} with {count}", plugin.name, plugin.version),
        Some(true) => format!("Updated {} to {} with {count}", plugin.name, plugin.version),
        None => format!("{} {} has {count}", plugin.name, plugin.version),
    };
    match consent {
        Consent::Ready if plugin.told() => {
            let off: Vec<&str> = plugin
                .capabilities
                .iter()
                .filter(|capability| capability.sensitive() && !granted_now(plugin, **capability))
                .map(|capability| capability.name())
                .collect();
            if off.is_empty() {
                done
            } else {
                let them = if off.len() == 1 { "it" } else { "them" };
                format!("{done}; it runs with {} off until you turn {them} on in Settings, Plugins", off.join(", "))
            }
        }
        Consent::Ready => done,
        Consent::NeedsConsent { missing } => {
            let names: Vec<&str> = missing.iter().map(|capability| capability.name()).collect();
            format!(
                "{done}, but it is off: its tools stay off until every permission it asks for is on (local-studio plugin grant {} {})",
                plugin.id,
                names.join(" ")
            )
        }
        Consent::Refused { reason } => format!("{done}, but it is off: {reason}"),
    }
}

fn granted_now(plugin: &Plugin, capability: Capability) -> bool {
    let root = plugin.dir.parent().map(Path::to_path_buf).unwrap_or_default();
    Grants::load(&root).is_ok_and(|grants| grants.standing(plugin).1.contains(&capability))
}

fn scanned(plugin: &Plugin, translation: Option<&Translation>) -> Result<(usize, Vec<String>), String> {
    let (files, executables) = survey(plugin)?;
    let mut findings: Vec<String> =
        executables.into_iter().map(|path| format!("executable not named in the manifest: {path}")).collect();
    findings.extend(translation.into_iter().flat_map(|translation| &translation.findings).map(ToString::to_string));
    findings.extend(text_findings(plugin).into_iter().map(|finding| finding.to_string()));
    Ok((files, findings))
}

fn notable(plugin: &Plugin, translation: Option<&Translation>) -> BTreeSet<String> {
    let mut items: BTreeSet<String> = text_findings(plugin).iter().map(super::compat::Finding::key).collect();
    if let Some(translation) = translation {
        items.extend(translation.findings.iter().map(super::compat::Finding::key));
        items.extend(translation.not_used.iter().cloned());
    }
    items
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
    translation: Option<&Translation>,
) -> ConsentRequest {
    let program = plugin
        .mcp
        .as_ref()
        .map(|mcp| mcp.command.strip_prefix(&plugin.dir).unwrap_or(&mcp.command).to_string_lossy().replace('\\', "/"));
    let command = plugin.mcp.as_ref().zip(program.as_ref()).map(|(mcp, program)| {
        let real = std::iter::once(program.clone())
            .chain(mcp.args.iter().map(|arg| arg.replace(super::GRANTED, "<allowed>")))
            .collect::<Vec<_>>()
            .join(" ");
        match translation.and_then(|translation| translation.server.as_ref()) {
            Some(server) => format!("{real} (from {}: {})", server.from, server.original),
            None => real,
        }
    });
    let not_used = translation
        .map(|translation| {
            translation
                .not_used
                .iter()
                .filter(|item| program.as_ref().is_none_or(|program| !names(item, program)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
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
        findings,
        rows,
        unpinned: false,
        told: plugin.told(),
        preset: Vec::new(),
        translated: translation.map(Translation::summary),
        not_used,
    }
}

fn names(item: &str, file: &str) -> bool {
    item.split_whitespace().any(|word| word.trim_end_matches([':', ',', ';', ')']).trim_start_matches('(') == file)
}
