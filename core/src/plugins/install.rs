use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{inside, load, valid_id, Capability, Consent, Grants, Plugin, MANIFEST};

pub const MAX_FILE: u64 = 10 * 1024 * 1024;
pub const ORIGINS: &str = "origins.json";
const BUNDLED_CATALOG: &str = include_str!("../../../examples/plugins/catalog.toml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Dir(PathBuf),
    Git { url: String, rev: String },
    Catalog(String),
    Follow { origin: String, url: String, rev: String, subdir: Option<String>, exclude: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub local_only: bool,
    pub git: String,
    pub rev: String,
    #[serde(default)]
    pub subdir: Option<String>,
    pub tree: String,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub translated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origins {
    #[serde(default)]
    plugins: BTreeMap<String, Origin>,
}

impl Origins {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(ORIGINS);
        let object = crate::workspace::providers::read_object(&path)?;
        serde_json::from_value(Value::Object(object)).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        crate::workspace::providers::write_private(&root.join(ORIGINS), &value)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Origin> {
        self.plugins.get(id)
    }

    pub fn set(&mut self, id: &str, origin: Origin) {
        self.plugins.insert(id.to_string(), origin);
    }

    pub fn forget(&mut self, id: &str) -> bool {
        self.plugins.remove(id).is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub plugin: Plugin,
    pub replaced: Option<String>,
    pub consent: Consent,
    pub files: usize,
    pub tree: String,
    pub source: String,
    pub note: Option<String>,
}

impl Source {
    pub fn parse(arg: &str) -> Result<Self, String> {
        if valid_id(arg) {
            return Ok(Source::Catalog(arg.to_string()));
        }
        let path = Path::new(arg);
        if path.is_dir() {
            return Ok(Source::Dir(path.to_path_buf()));
        }
        match arg.rsplit_once('@') {
            Some((url, rev)) if !url.is_empty() && full_sha(rev) => {
                Ok(Source::Git { url: url.to_string(), rev: rev.to_string() })
            }
            _ if arg.contains("://")
                || arg.contains('@')
                || path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("git")) =>
            {
                Err(format!(
                    "a git source must be pinned to a full commit SHA (40 hex characters) as <url>@<sha>, not {arg}"
                ))
            }
            _ => Err(format!("not a plugin folder, a catalog id or a git source <url>@<sha>: {arg}")),
        }
    }

    #[must_use]
    pub fn origin(&self) -> String {
        match self {
            Source::Dir(dir) => format!("folder {}", super::absolute(dir).display()),
            Source::Git { url, .. } => format!("git {url}"),
            Source::Catalog(id) => format!("catalog {id}"),
            Source::Follow { origin, .. } => origin.clone(),
        }
    }

    pub fn resolve(arg: &str, channel: &str) -> Result<(Self, Option<String>), String> {
        let parsed = Self::parse(arg);
        let url = arg.trim();
        let git_like = url.contains("://")
            || url.starts_with("git@")
            || Path::new(url).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("git"));
        match parsed {
            Err(_) if git_like && !url.rsplit_once('@').is_some_and(|(_, rev)| full_sha(rev)) => {
                let (name, rev) = resolve_channel(url, channel)?;
                Ok((Source::Git { url: url.to_string(), rev }, Some(name)))
            }
            other => other.map(|source| (source, None)),
        }
    }

    #[must_use]
    pub fn pinned(&self) -> bool {
        matches!(self, Source::Catalog(_))
    }
}

pub(crate) fn remote(url: &str, args: &[&str]) -> Result<Vec<(String, String)>, String> {
    let output = Command::new("git")
        .arg("ls-remote")
        .args(args)
        .arg("--")
        .arg(url)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(format!("git ls-remote {url}: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(sha, name)| (name.to_string(), sha.to_string())))
        .collect())
}

fn head(url: &str) -> Result<String, String> {
    remote(url, &[])?
        .into_iter()
        .find(|(name, _)| name == "HEAD")
        .map(|(_, sha)| sha)
        .ok_or_else(|| format!("{url} has no default branch"))
}

fn release(name: &str) -> Option<(u64, u64, u64)> {
    let mut parts = name.strip_prefix("refs/tags/v")?.split('.');
    let number = |part: Option<&str>| {
        part.filter(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            .and_then(|part| part.parse().ok())
    };
    let version = (number(parts.next())?, number(parts.next())?, number(parts.next())?);
    parts.next().is_none().then_some(version)
}

pub fn resolve_channel(url: &str, channel: &str) -> Result<(String, String), String> {
    let refs = remote(url, &["--heads", "--tags"])?;
    let peeled = |name: &str| {
        let wanted = format!("{name}^{{}}");
        refs.iter()
            .find(|(other, _)| *other == wanted)
            .or_else(|| refs.iter().find(|(other, _)| other == name))
            .map(|(_, sha)| sha.clone())
    };
    let name = match channel {
        "stable" => refs
            .iter()
            .filter_map(|(name, _)| release(name).map(|version| (version, name.clone())))
            .max()
            .map(|(_, name)| name),
        "nightly" => Some("refs/tags/nightly".to_string()),
        _ => Some("refs/heads/main".to_string()),
    };
    let found = name.and_then(|name| {
        let sha = peeled(&name)?;
        Some((name.trim_start_matches("refs/tags/").trim_start_matches("refs/heads/").to_string(), sha))
    });
    match found {
        Some(found) => Ok(found),
        None => Ok(("HEAD".to_string(), head(url)?)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peeked {
    pub id: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub changed: bool,
}

pub fn peek(url: &str, rev: &str, subdir: Option<&str>, since: Option<&str>) -> Result<Peeked, String> {
    #[derive(Deserialize)]
    struct Head {
        id: String,
        version: String,
        #[serde(default)]
        capabilities: Vec<String>,
    }
    let root = std::env::temp_dir();
    let scratch = Scratch::new(&root, "ls-plugin-peek").map_err(|errors| errors.join("; "))?;
    let git = |args: &[&str]| -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&scratch.0)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| format!("cannot run git: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
        }
    };
    git(&["init", "-q"])?;
    git(&["remote", "add", "origin", url])?;
    git(&["config", "remote.origin.promisor", "true"])?;
    git(&["config", "remote.origin.partialclonefilter", "blob:none"])?;
    git(&["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", rev])?;
    let folder = subdir.map(|subdir| subdir.trim_end_matches('/'));
    let at = |file: &str| folder.map_or_else(|| file.to_string(), |folder| format!("{folder}/{file}"));
    let path = at(MANIFEST);
    let head: Head = if let Ok(text) = git(&["show", &format!("{rev}:{path}")]) {
        toml::from_str(&text).map_err(|error| format!("invalid {MANIFEST} at {rev}: {}", error.message()))?
    } else {
        let foreign = [".claude-plugin/plugin.json", "plugin.json", ".codex-plugin/plugin.json"]
            .iter()
            .find_map(|file| git(&["show", &format!("{rev}:{}", at(file))]).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        match foreign {
            Some(manifest) => Head {
                id: manifest.get("name").and_then(Value::as_str).map(super::compat::fold).unwrap_or_default(),
                version: manifest
                    .get("version")
                    .and_then(Value::as_str)
                    .filter(|version| super::valid_semver(version))
                    .unwrap_or("0.0.0")
                    .to_string(),
                capabilities: Vec::new(),
            },
            None if git(&["cat-file", "-e", &format!("{rev}:{}", at("skills"))]).is_ok() => {
                Head { id: String::new(), version: "0.0.0".to_string(), capabilities: Vec::new() }
            }
            None => return Err(format!("{rev} has no {path}")),
        }
    };
    let tree = |rev: &str| git(&["rev-parse", &format!("{rev}:{}", folder.unwrap_or_default())]);
    let changed = match since {
        Some(since) => git(&["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", since])
            .and_then(|_| Ok(tree(rev)? != tree(since)?))
            .unwrap_or(true),
        None => true,
    };
    Ok(Peeked { id: head.id, version: head.version, capabilities: head.capabilities, changed })
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self, String> {
        let catalog: Catalog = toml::from_str(text).map_err(|error| format!("invalid catalog: {}", error.message()))?;
        for entry in &catalog.plugins {
            if !valid_id(&entry.id) {
                return Err(format!("catalog: bad id {:?}", entry.id));
            }
            if !full_sha(&entry.rev) {
                return Err(format!("catalog {}: rev must be a full commit SHA", entry.id));
            }
            if !entry.tree.strip_prefix("sha256:").is_some_and(|hex| hex.len() == 64 && is_hex(hex)) {
                return Err(format!("catalog {}: tree must be sha256:<64 hex>", entry.id));
            }
            if let Some(name) = entry.capabilities.iter().find(|name| Capability::parse(name).is_none()) {
                return Err(format!("catalog {}: unknown capability {name}", entry.id));
            }
        }
        Ok(catalog)
    }

    pub fn bundled() -> Result<Self, String> {
        Self::parse(BUNDLED_CATALOG)
    }

    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&CatalogEntry> {
        self.plugins.iter().find(|entry| entry.id == id)
    }
}

fn is_hex(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

pub(crate) fn full_sha(rev: &str) -> bool {
    rev.len() == 40 && is_hex(rev)
}

pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn new(root: &Path, what: &str) -> Result<Self, Vec<String>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        let dir = root.join(format!(".{what}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|error| vec![format!("cannot create {}: {error}", dir.display())])?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn install(root: &Path, source: &Source, catalog: &Catalog) -> Result<Installed, Vec<String>> {
    stage(root, source, catalog)?.commit(root)
}

pub struct Staged {
    staging: Scratch,
    plugin: Plugin,
    files: usize,
    tree: String,
    origin: String,
    pin: Option<String>,
    record: Box<Origin>,
    translated: bool,
}

pub fn stage(root: &Path, source: &Source, catalog: &Catalog) -> Result<Staged, Vec<String>> {
    std::fs::create_dir_all(root).map_err(|error| vec![format!("cannot create {}: {error}", root.display())])?;
    let origin = source.origin();
    let git = |url: &str, rev: &str, subdir: Option<&String>, exclude: &[String], pinned: bool| Origin {
        git: Some(url.to_string()),
        rev: Some(rev.to_string()),
        subdir: subdir.cloned(),
        exclude: exclude.to_vec(),
        folder: None,
        pinned,
        translated: false,
    };
    let mut staged = match source {
        Source::Dir(dir) => place(root, dir, &[], None, &[], origin, &folder_name(dir))?,
        Source::Git { url, rev } => {
            let fetched = Scratch::new(root, "fetch")?;
            let flattened = fetch(url, rev, &fetched.0)?;
            let mut staged = place(root, &fetched.0, &[], None, &flattened, origin, &repo_name(url))?;
            staged.record = Box::new(git(url, rev, None, &[], false));
            staged
        }
        Source::Follow { url, rev, subdir, exclude, .. } => {
            let fetched = Scratch::new(root, "fetch")?;
            let flattened = fetch(url, rev, &fetched.0)?;
            let dir = match subdir {
                Some(subdir) => within(&fetched.0, subdir)?,
                None => fetched.0.clone(),
            };
            let hint = subdir.as_deref().map_or_else(|| repo_name(url), |subdir| folder_name(Path::new(subdir)));
            let mut staged = place(root, &dir, exclude, None, &flattened, origin, &hint)?;
            staged.record = Box::new(git(url, rev, subdir.as_ref(), exclude, false));
            staged
        }
        Source::Catalog(id) => {
            let entry = catalog
                .entry(id)
                .ok_or_else(|| vec![format!("{id} is not in the plugin catalog (use ./{id} for a folder)")])?;
            let fetched = Scratch::new(root, "fetch")?;
            let flattened = fetch(&entry.git, &entry.rev, &fetched.0)?;
            let dir = match &entry.subdir {
                Some(subdir) => inside(&fetched.0, subdir).map_err(|error| vec![error])?,
                None => fetched.0.clone(),
            };
            let mut staged = place(root, &dir, &entry.exclude, Some(&entry.tree), &flattened, origin, id)?;
            let asked: Vec<&str> = staged.plugin.capabilities.iter().map(|capability| capability.name()).collect();
            let listed: Vec<&str> = entry.capabilities.iter().map(String::as_str).collect();
            if asked.len() != listed.len() || asked.iter().any(|name| !listed.contains(name)) {
                return Err(vec![format!(
                    "the catalog lists {id} as asking for [{}], but its plugin.toml asks for [{}], so nothing was installed",
                    listed.join(", "),
                    asked.join(", ")
                )]);
            }
            staged.record = Box::new(git(&entry.git, &entry.rev, entry.subdir.as_ref(), &entry.exclude, true));
            staged
        }
    };
    if let Source::Dir(dir) = source {
        staged.record.folder = Some(super::absolute(dir).to_string_lossy().into_owned());
    }
    staged.record.translated = staged.translated;
    Ok(staged)
}

fn folder_name(dir: &Path) -> String {
    let dir = super::absolute(dir);
    dir.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string()
}

fn repo_name(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn within(base: &Path, subdir: &str) -> Result<PathBuf, Vec<String>> {
    let dir = inside(base, subdir).map_err(|error| vec![error])?;
    let real = dir.canonicalize().map_err(|error| vec![format!("{subdir}: {error}")])?;
    let base = base.canonicalize().map_err(|error| vec![format!("{}: {error}", base.display())])?;
    if real.starts_with(&base) {
        Ok(dir)
    } else {
        Err(vec![format!("{subdir} leaves the repository")])
    }
}

#[allow(clippy::too_many_arguments)]
fn place(
    root: &Path,
    source: &Path,
    exclude: &[String],
    expected: Option<&str>,
    flattened: &[PathBuf],
    origin: String,
    hint: &str,
) -> Result<Staged, Vec<String>> {
    let entries = walk(source, exclude, Walk::Install).map_err(|error| vec![error])?;
    let unlinked: Vec<String> = entries
        .iter()
        .filter(|entry| matches!(entry.kind, Kind::File { .. }) && flattened.contains(&source.join(&entry.path)))
        .map(|entry| {
            format!(
                "symlink came back as a regular file: {} (this system did not create the link; on Windows, turn on Developer Mode)",
                entry.path
            )
        })
        .collect();
    if !unlinked.is_empty() {
        return Err(unlinked);
    }
    let foreign = super::compat::foreign(source);
    if foreign && expected.is_some() {
        return Err(vec![format!("a catalog plugin ships its own {MANIFEST}, and this one has none")]);
    }
    let refused = refusals(source, &entries, Walk::Install, !foreign);
    if !refused.is_empty() {
        return Err(refused);
    }
    let tree = hash_entries(source, &entries).map_err(|error| vec![error])?;
    if let Some(expected) = expected.filter(|expected| *expected != tree) {
        return Err(vec![format!("tree hash mismatch: the catalog pins {expected}, the fetched plugin is {tree}")]);
    }
    let staging = Scratch::new(root, "install")?;
    let id = if foreign { ".translate".to_string() } else { manifest_id(source)? };
    let mut staged = staging.0.join(&id);
    copy(source, &staged, &entries).map_err(|error| vec![error])?;
    let copied = hash_entries(&staged, &entries).map_err(|error| vec![error])?;
    if copied != tree {
        return Err(vec![format!("the plugin changed while it was installed: hashed {tree}, copied {copied}")]);
    }
    let (entries, tree) = if foreign {
        let translation = super::compat::translate(&staged, hint)?;
        taken(root, &translation.id, &origin)?;
        let named = staging.0.join(&translation.id);
        std::fs::rename(&staged, &named).map_err(|error| vec![format!("cannot name the translated copy: {error}")])?;
        staged = named;
        let entries = walk(&staged, &[], Walk::Install).map_err(|error| vec![error])?;
        let refused = refusals(&staged, &entries, Walk::Install, true);
        if !refused.is_empty() {
            return Err(refused);
        }
        let tree = hash_entries(&staged, &entries).map_err(|error| vec![error])?;
        (entries, tree)
    } else {
        (entries, tree)
    };
    let plugin = load(&staged)?;
    let files = entries.iter().filter(|entry| !matches!(entry.kind, Kind::Dir)).count();
    Ok(Staged {
        staging,
        plugin,
        files,
        tree,
        origin,
        pin: expected.map(str::to_string),
        record: Box::default(),
        translated: foreign,
    })
}

fn taken(root: &Path, id: &str, origin: &str) -> Result<(), Vec<String>> {
    if std::fs::symlink_metadata(root.join(id)).is_err() {
        return Ok(());
    }
    let grants = Grants::load(root).map_err(|error| vec![error])?;
    let recorded = grants.granted(id).map(|(_, source, _)| source.to_string()).unwrap_or_default();
    if recorded == origin {
        return Ok(());
    }
    let from = if recorded.is_empty() { "a source Local Studio did not record".to_string() } else { recorded };
    Err(vec![format!(
        "{id} is already installed from {from}, and this plugin from {origin} translates to the same id, so nothing was installed (remove {id} first)"
    )])
}

impl Staged {
    #[must_use]
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    #[must_use]
    pub fn tree(&self) -> &str {
        &self.tree
    }

    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    #[must_use]
    pub fn files(&self) -> usize {
        self.files
    }

    #[must_use]
    pub fn record(&self) -> &Origin {
        &self.record
    }

    #[must_use]
    pub fn translation(&self) -> Option<super::compat::Translation> {
        super::compat::checked(&self.plugin.dir, self.translated)
    }

    fn verify(&self, staged: &Path) -> Result<(), Vec<String>> {
        let entries = walk(staged, &[], Walk::Consent).map_err(|error| vec![error])?;
        let mut refused = refusals(staged, &entries, Walk::Install, true);
        if refused.is_empty() {
            let tree = hash_entries(staged, &entries).map_err(|error| vec![error])?;
            if tree != self.tree || self.pin.as_ref().is_some_and(|pin| *pin != tree) {
                refused.push(format!(
                    "{} changed after it was checked ({} then, {tree} now), so nothing was installed or granted",
                    self.plugin.id, self.tree
                ));
            }
        }
        if refused.is_empty() {
            Ok(())
        } else {
            Err(refused)
        }
    }

    pub fn commit(self, root: &Path) -> Result<Installed, Vec<String>> {
        self.commit_with(root, None)
    }

    pub(crate) fn commit_with(
        self,
        root: &Path,
        sheet: Option<(&[Capability], &[Capability])>,
    ) -> Result<Installed, Vec<String>> {
        let _lock = Grants::lock(root).map_err(|error| vec![error])?;
        let id = &self.plugin.id;
        let staged = self.staging.0.join(id);
        self.verify(&staged)?;
        let target = root.join(id);
        let previous = load(&target).ok();
        let mut grants = Grants::load(root).map_err(|error| vec![error])?;
        if let Some((asked, _)) = sheet {
            if super::consent::asked(&self.plugin, &self.tree, &self.origin, &grants, previous.as_ref()) != asked {
                return Err(vec![format!(
                    "the permissions {id} needs changed after they were checked, so nothing was installed or granted; install it again to see them"
                )]);
            }
        }
        let replaced = previous.map(|old| old.version);
        let old = self.staging.0.join(".old");
        let moved = target.exists();
        if moved {
            std::fs::rename(&target, &old)
                .map_err(|error| vec![format!("cannot move the installed {id} aside: {error}")])?;
        }
        if let Err(error) = std::fs::rename(&staged, &target) {
            if moved {
                let _ = std::fs::rename(&old, &target);
            }
            return Err(vec![format!("cannot install {}: {error}", target.display())]);
        }
        let recorded = (|| -> Result<(Plugin, Option<String>), Vec<String>> {
            let plugin = load(&target)?;
            let note = grants.installed(&plugin, &self.tree, &self.origin);
            if let Some((_, chosen)) = sheet.filter(|(_, chosen)| !chosen.is_empty()) {
                grants.grant(&plugin, chosen, Some(&self.tree)).map_err(|error| vec![error])?;
            }
            grants.save(root).map_err(|error| vec![error])?;
            let mut origins = Origins::load(root).map_err(|error| vec![error])?;
            origins.set(&plugin.id, (*self.record).clone());
            origins.save(root).map_err(|error| vec![error])?;
            Ok((plugin, note))
        })();
        let (plugin, note) = recorded.inspect_err(|_| {
            let _ = std::fs::rename(&target, &staged);
            if moved {
                let _ = std::fs::rename(&old, &target);
            }
        })?;
        let consent = grants.consent(&plugin);
        Ok(Installed { plugin, replaced, consent, files: self.files, tree: self.tree, source: self.origin, note })
    }
}

/// The plugin's file count and the executables it ships besides the `[mcp]` command its manifest names.
pub(crate) fn survey(plugin: &Plugin) -> Result<(usize, Vec<String>), String> {
    let command = plugin.mcp.as_ref().and_then(|mcp| mcp.command.strip_prefix(&plugin.dir).ok().map(Path::to_path_buf));
    let entries = walk(&plugin.dir, &[], Walk::Consent)?;
    let files = entries.iter().filter(|entry| !matches!(entry.kind, Kind::Dir)).count();
    let binaries = entries
        .into_iter()
        .filter(|entry| matches!(entry.kind, Kind::File { executable: true, .. }))
        .filter(|entry| command.as_deref() != Some(Path::new(&entry.path)))
        .map(|entry| entry.path)
        .collect();
    Ok((files, binaries))
}

fn manifest_id(source: &Path) -> Result<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    let path = source.join(MANIFEST);
    let text =
        std::fs::read_to_string(&path).map_err(|error| vec![format!("cannot read {}: {error}", path.display())])?;
    let id: Id = toml::from_str(&text).map_err(|error| vec![format!("invalid {MANIFEST}: {}", error.message())])?;
    if valid_id(&id.id) {
        Ok(id.id)
    } else {
        Err(vec![format!("bad id: {:?} (use lowercase letters, digits and hyphens)", id.id)])
    }
}

fn fetch(url: &str, rev: &str, into: &Path) -> Result<Vec<PathBuf>, Vec<String>> {
    let git = |args: &[&str]| -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(into)
            .args([
                "-c",
                "advice.detachedHead=false",
                "-c",
                "core.symlinks=true",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.eol=lf",
            ])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| format!("cannot run git: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
        }
    };
    git(&["init", "-q"]).map_err(|error| vec![error])?;
    if git(&["fetch", "-q", "--depth", "1", "--", url, rev]).is_err() {
        git(&["fetch", "-q", "--", url, "+refs/heads/*:refs/remotes/source/*", "+refs/tags/*:refs/tags/*"])
            .map_err(|error| vec![error])?;
    }
    git(&["checkout", "-q", "--detach", rev]).map_err(|error| vec![error])?;
    let head = git(&["rev-parse", "HEAD"]).map_err(|error| vec![error])?;
    if head != rev {
        return Err(vec![format!("git checked out {head}, not the pinned {rev}")]);
    }
    let flattened = git(&["ls-tree", "-r", "-z", "HEAD"])
        .map_err(|error| vec![error])?
        .split('\0')
        .filter_map(|line| line.strip_prefix("120000 ")?.split_once('\t').map(|(_, path)| into.join(path)))
        .filter(|path| !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()))
        .collect();
    std::fs::remove_dir_all(into.join(".git")).map_err(|error| vec![format!("cannot drop .git: {error}")])?;
    Ok(flattened)
}

#[derive(Debug)]
enum Kind {
    File { size: u64, executable: bool },
    Link(PathBuf),
    Dir,
    Other,
}

#[derive(Debug)]
struct Entry {
    path: String,
    kind: Kind,
}

/// Finder writes `.DS_Store` into any folder it shows; no runtime loads one, so no hash covers it.
pub const UNHASHED: &str = ".DS_Store";
const GIT: &str = ".git";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    Install,
    Consent,
}

fn skipped(path: &Path, walk: Walk) -> bool {
    walk == Walk::Install && path.components().next().is_some_and(|part| named(part.as_os_str(), GIT))
}

pub(crate) fn named(part: &std::ffi::OsStr, name: &str) -> bool {
    part.to_str().is_some_and(|part| part.eq_ignore_ascii_case(name))
}

fn unhashed(real: &Path, walk: Walk) -> bool {
    real.components().any(|part| named(part.as_os_str(), UNHASHED))
        || (walk == Walk::Install && real.components().next().is_some_and(|part| named(part.as_os_str(), GIT)))
}

fn gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn walk(root: &Path, exclude: &[String], mode: Walk) -> Result<Vec<Entry>, String> {
    let excluded: Vec<PathBuf> = exclude
        .iter()
        .map(|relative| inside(Path::new(""), relative).map(|path| path.components().collect()))
        .collect::<Result<_, _>>()?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let dir = root.join(&relative);
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(error) if gone(&error) && !relative.as_os_str().is_empty() => continue,
            Err(error) => return Err(format!("cannot read {}: {error}", dir.display())),
        };
        for item in listing {
            let item = item.map_err(|error| error.to_string())?;
            let name = item.file_name();
            let path = relative.join(&name);
            if skipped(&path, mode) || excluded.contains(&path) {
                continue;
            }
            let meta = match std::fs::symlink_metadata(item.path()) {
                Ok(meta) => meta,
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(error.to_string()),
            };
            if named(&name, UNHASHED) && meta.is_file() {
                continue;
            }
            let text = path
                .components()
                .map(|part| part.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("not a UTF-8 path: {}", path.display()))?
                .join("/");
            let kind = if meta.file_type().is_symlink() {
                match std::fs::read_link(item.path()) {
                    Ok(target) => Kind::Link(target),
                    Err(error) if gone(&error) => continue,
                    Err(error) => return Err(error.to_string()),
                }
            } else if meta.is_dir() {
                pending.push(path);
                Kind::Dir
            } else if meta.is_file() {
                Kind::File { size: meta.len(), executable: crate::platform::is_executable(&meta) }
            } else {
                Kind::Other
            };
            entries.push(Entry { path: text, kind });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn stays_inside(link: &str, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth: usize = Path::new(link).components().count().saturating_sub(1);
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    true
}

fn resolved(root: &Path, link: &str) -> Option<PathBuf> {
    let base = std::fs::canonicalize(root).ok()?;
    let real = std::fs::canonicalize(root.join(link)).ok()?;
    real.strip_prefix(&base).ok().map(Path::to_path_buf)
}

fn link_refusal(root: &Path, link: &str, target: &Path, mode: Walk) -> Option<String> {
    let resolved = resolved(root, link).filter(|_| stays_inside(link, target));
    match resolved {
        None => Some(format!("symlink points outside the plugin or nowhere: {link} -> {}", target.display())),
        Some(real) if unhashed(&real, mode) => {
            Some(format!("symlink points at a file no hash covers: {link} -> {}", target.display()))
        }
        Some(_) => None,
    }
}

fn refusals(root: &Path, entries: &[Entry], mode: Walk, executables: bool) -> Vec<String> {
    let mut refusals: Vec<String> = Vec::new();
    for entry in entries {
        match &entry.kind {
            Kind::Link(target) => refusals.extend(link_refusal(root, &entry.path, target, mode)),
            Kind::File { size, .. } if *size > MAX_FILE => {
                refusals.push(format!("file over 10 MB: {} ({size} bytes)", entry.path));
            }
            Kind::File { executable: true, .. }
                if mode == Walk::Install
                    && executables
                    && !entry.path.starts_with("bin/")
                    && !super::compat::LAUNCHERS.contains(&entry.path.as_str()) =>
            {
                refusals.push(format!("executable outside bin/: {}", entry.path));
            }
            Kind::Other if mode == Walk::Install => {
                refusals.push(format!("not a regular file, folder or symlink: {}", entry.path));
            }
            _ => {}
        }
    }
    refusals
}

/// Reads a file only if it is still a regular file, without following a link or waiting on a FIFO.
/// `None` means the path is now something else (a FIFO, a folder or a link).
fn read_regular(path: &Path) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    let file = match crate::platform::open_without_waiting(path) {
        Ok(file) => file,
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(None);
    }
    let mut bytes: Vec<u8> = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    Ok(Some((bytes, crate::platform::is_executable(&meta))))
}

/// Test hook for scripts/tests/plugin-grants-test.sh, built only with the `test-hooks` feature: touch
/// `<gate>/<reached>` and wait for `<gate>/<resume>`, so the script can swap a file at that exact point.
#[cfg(feature = "test-hooks")]
fn test_gate(reached: &str, resume: &str) {
    let Some(gate) = std::env::var_os("LOCAL_STUDIO_TEST_HASH_GATE").map(PathBuf::from) else { return };
    let _ = std::fs::write(gate.join(reached), b"");
    let started = std::time::Instant::now();
    while !gate.join(resume).exists() && started.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn hash_entries(root: &Path, entries: &[Entry]) -> Result<String, String> {
    #[cfg(feature = "test-hooks")]
    test_gate("walked", "go");
    let mut listing = String::new();
    for entry in entries {
        match &entry.kind {
            Kind::File { .. } => {
                let path = root.join(&entry.path);
                let (bytes, executable) = match read_regular(&path) {
                    Ok(Some(read)) => read,
                    Ok(None) => {
                        return Err(format!("{} changed while it was read: it is no longer a regular file", entry.path))
                    }
                    Err(error) if gone(&error) => continue,
                    Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
                };
                if bytes.len() as u64 > MAX_FILE {
                    return Err(format!("file over 10 MB: {}", entry.path));
                }
                let mode = if executable { "x" } else { "-" };
                let _ = writeln!(listing, "F {mode} {} {}", hex(&Sha256::digest(&bytes)), entry.path);
            }
            Kind::Link(target) => {
                let digest = Sha256::digest(target.as_os_str().as_encoded_bytes());
                let _ = writeln!(listing, "L - {} {}", hex(&digest), entry.path);
            }
            Kind::Dir | Kind::Other => {}
        }
    }
    Ok(format!("sha256:{}", hex(&Sha256::digest(listing.as_bytes()))))
}

pub fn tree_hash(root: &Path, exclude: &[String]) -> Result<String, String> {
    let entries = walk(root, exclude, Walk::Install)?;
    let refused = refusals(root, &entries, Walk::Install, true);
    if !refused.is_empty() {
        return Err(refused.join("\n"));
    }
    hash_entries(root, &entries)
}

/// The folder consent checks: all of it as it is now, `.git` included, so code there is covered too.
pub(crate) struct Live<'a> {
    root: &'a Path,
    entries: Vec<Entry>,
}

impl<'a> Live<'a> {
    pub(crate) fn check(root: &'a Path) -> Result<Self, String> {
        if std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(format!("the plugin folder is a symlink: {}", root.display()));
        }
        let entries = walk(root, &[], Walk::Consent)?;
        let refused = refusals(root, &entries, Walk::Consent, true);
        if !refused.is_empty() {
            return Err(refused.join("; "));
        }
        Ok(Self { root, entries })
    }

    pub(crate) fn hash(&self) -> Result<String, String> {
        hash_entries(self.root, &self.entries)
    }
}

pub(crate) fn live_hash(root: &Path) -> Result<String, String> {
    Live::check(root)?.hash()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Copies through the same open as the hash, so a file swapped for a FIFO or a link after the hash
/// is refused instead of blocking or being followed. `place` then hashes the copy again.
fn copy(source: &Path, target: &Path, entries: &[Entry]) -> Result<(), String> {
    #[cfg(feature = "test-hooks")]
    test_gate("copying", "copy-go");
    std::fs::create_dir_all(target).map_err(|error| format!("cannot create {}: {error}", target.display()))?;
    for entry in entries {
        let from = source.join(&entry.path);
        let to = target.join(&entry.path);
        let result = match &entry.kind {
            Kind::Dir => std::fs::create_dir_all(&to),
            Kind::File { .. } => match read_regular(&from) {
                Ok(Some((bytes, executable))) => write_file(&to, &bytes, executable),
                Ok(None) => {
                    return Err(format!("{} changed while it was read: it is no longer a regular file", entry.path))
                }
                Err(error) if gone(&error) => Ok(()),
                Err(error) => Err(error),
            },
            Kind::Link(link) => crate::platform::symlink(link, &to, from.is_dir()),
            Kind::Other => Ok(()),
        };
        result.map_err(|error| format!("cannot copy {}: {error}", entry.path))?;
    }
    Ok(())
}

fn write_file(to: &Path, bytes: &[u8], executable: bool) -> std::io::Result<()> {
    std::fs::write(to, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}
