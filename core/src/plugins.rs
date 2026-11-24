use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::frontmatter;
use crate::model::{CommandKind, SlashCommand};

pub mod ask;
pub mod compat;
mod consent;
mod grants;
pub mod hub;
mod install;
pub mod market;
#[cfg(all(test, unix))]
mod tests;

pub use consent::{answer, offer_capabilities, offer_grant, offer_install, offer_source, Offer, Pending, Pendings};
pub use grants::{Consent, Grants, GRANTS};
pub use install::{
    install, resolve_channel, stage, tree_hash, Catalog, CatalogEntry, Installed, Origin, Origins, Source, Staged,
    MAX_FILE, ORIGINS,
};

pub const MANIFEST: &str = "plugin.toml";
pub const MCP_PREFIX: &str = "plugin-";
pub const MCP_TAG: &str = "localStudioPlugin";
/// Eukhe's settings take env values only as host variable names; Python needs any non-empty value, and every kernel has PATH.
pub const NO_BYTECODE: (&str, &str) = ("PYTHONDONTWRITEBYTECODE", "PATH");
pub const LOCAL_ONLY_ENV: &str = "LOCAL_STUDIO_LOCAL_ONLY";
pub const LOCAL_ONLY_LABEL: &str = "Runs only on local models";
pub const SESSION_OWNER: &str = "local-studio";
pub const GRANTED: &str = "{granted}";

pub type Standing<'a> = (&'a Plugin, Consent, Vec<Capability>);

pub const CALLER_ENV: &str = "LOCAL_STUDIO_CALLER";
pub const CALLER_NAME_ENV: &str = "LOCAL_STUDIO_CALLER_NAME";
pub const HOST_PREFIX: &str = "local:";
pub const DEVICE_PREFIX: &str = "device:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    pub id: String,
    pub name: String,
}

impl Caller {
    #[must_use]
    pub fn device(id: &str, name: &str) -> Self {
        let name = if name.trim().is_empty() { id } else { name.trim() };
        Self { id: format!("{DEVICE_PREFIX}{id}"), name: name.to_string() }
    }

    #[must_use]
    pub fn host(user: &str) -> Self {
        let name = ["USER", "LOGNAME", "USERNAME"]
            .iter()
            .find_map(|key| std::env::var(key).ok().filter(|value| !value.trim().is_empty()))
            .unwrap_or_else(|| user.to_string());
        Self { id: format!("{HOST_PREFIX}{user}"), name }
    }

    pub(crate) fn insert(&self, env: &mut Map<String, Value>) {
        env.insert(CALLER_ENV.to_string(), json!(self.id));
        env.insert(CALLER_NAME_ENV.to_string(), json!(self.name));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    McpServer,
    NativeHost,
    AskUser,
    SiteSecrets,
    Accessibility,
    SyntheticInput,
    ScreenCapture,
}

impl Capability {
    pub const ALL: [Capability; 7] = [
        Capability::McpServer,
        Capability::NativeHost,
        Capability::AskUser,
        Capability::SiteSecrets,
        Capability::Accessibility,
        Capability::SyntheticInput,
        Capability::ScreenCapture,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Capability::McpServer => "mcp-server",
            Capability::NativeHost => "native-host",
            Capability::AskUser => "ask-user",
            Capability::SiteSecrets => "site-secrets",
            Capability::Accessibility => "accessibility",
            Capability::SyntheticInput => "synthetic-input",
            Capability::ScreenCapture => "screen-capture",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Capability::McpServer => "Run its own tool server for your sessions",
            Capability::NativeHost => "Register a helper that one browser extension can talk to",
            Capability::AskUser => "Ask you questions and ask before risky steps",
            Capability::SiteSecrets => "Fill your saved secrets into the sites they belong to",
            Capability::Accessibility => "Read and operate the windows and controls of other apps",
            Capability::SyntheticInput => "Move the pointer and press keys for you",
            Capability::ScreenCapture => "Take screenshots of app windows",
        }
    }

    #[must_use]
    pub fn sensitive(self) -> bool {
        matches!(
            self,
            Capability::ScreenCapture
                | Capability::SyntheticInput
                | Capability::SiteSecrets
                | Capability::Accessibility
        )
    }

    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Capability::McpServer => "Give agents tools",
            Capability::NativeHost => "Talk to its browser extension",
            Capability::AskUser => "Ask you questions in a thread",
            Capability::ScreenCapture => "See the windows or tabs it opened",
            Capability::SyntheticInput => "Click and type in those tabs",
            Capability::SiteSecrets => "Fill sign-ins you approve",
            Capability::Accessibility => "Read and control other apps",
        }
    }

    #[must_use]
    pub fn scope(self) -> &'static str {
        match self {
            Capability::McpServer => "Starts {command} when a thread uses it.",
            Capability::NativeHost => "Through a local helper that only its pinned extension can reach.",
            Capability::AskUser => "Shows a question card; it never answers for you.",
            Capability::ScreenCapture => "Screenshots of its own tabs or windows, never your other windows.",
            Capability::SyntheticInput => "Input goes only to what the plugin opened.",
            Capability::SiteSecrets => "Reads one saved login per site, after you allow that site.",
            Capability::Accessibility => "Uses the system accessibility API; macOS asks you separately.",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|capability| capability.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub version: String,
    pub description: String,
    pub gates: BTreeMap<Capability, String>,
    pub dir: PathBuf,
    pub skills: Vec<PluginFile>,
    pub prompts: Vec<PluginFile>,
    pub commands: Vec<PluginCommand>,
    pub capabilities: Vec<Capability>,
    pub mcp: Option<McpServer>,
    pub local_only: bool,
    pub skipped: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFile {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    pub name: String,
    pub description: String,
    pub target: CommandTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandTarget {
    Prompt(PathBuf),
    Skill(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFailure {
    pub dir: PathBuf,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub plugins: Vec<Plugin>,
    pub failures: Vec<PluginFailure>,
}

impl Plugin {
    #[must_use]
    pub fn source(&self) -> String {
        format!("plugin:{}", self.id)
    }

    #[must_use]
    pub fn skill(&self, name: &str) -> Option<&PluginFile> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    #[must_use]
    pub fn asks(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    #[must_use]
    pub fn mcp_name(&self) -> String {
        format!("{MCP_PREFIX}{}", self.id)
    }

    #[must_use]
    pub fn told(&self) -> bool {
        self.mcp.as_ref().is_some_and(|mcp| mcp.args.iter().any(|arg| arg.contains(GRANTED)))
    }

    #[must_use]
    pub fn args(&self, held: &[Capability]) -> Vec<String> {
        let names: Vec<&str> =
            held.iter().filter(|capability| capability.sensitive()).map(|capability| capability.name()).collect();
        let list = names.join(",");
        self.mcp
            .as_ref()
            .map(|mcp| mcp.args.iter().map(|arg| arg.replace(GRANTED, &list)).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: Option<String>,
    icon: Option<String>,
    version: String,
    description: String,
    #[serde(default)]
    gates: BTreeMap<String, String>,
    skills: Option<String>,
    prompts: Option<String>,
    #[serde(default)]
    commands: Vec<ManifestCommand>,
    #[serde(default)]
    capabilities: Vec<String>,
    mcp: Option<ManifestMcp>,
    #[serde(default)]
    local_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestMcp {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCommand {
    name: String,
    description: String,
    prompt: Option<String>,
    skill: Option<String>,
}

#[must_use]
pub fn plugins_dir(explicit_data_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(explicit) = explicit_data_dir.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(explicit).join("plugins"));
    }
    if let Some(value) = std::env::var_os("LOCAL_STUDIO_DATA_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(value).join("plugins"));
    }
    crate::store::dot_dir().ok().map(|dir| dir.join("plugins"))
}

#[must_use]
pub fn scan(root: &Path) -> Scan {
    let mut scan = Scan::default();
    let Ok(entries) = std::fs::read_dir(root) else { return scan };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| !path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with('.')))
        .collect();
    dirs.sort();
    let mut seen: HashSet<String> = HashSet::new();
    for dir in dirs {
        match load(&dir) {
            Ok(plugin) if !seen.insert(plugin.id.clone()) => {
                scan.failures.push(PluginFailure { dir, errors: vec![format!("duplicate plugin id: {}", plugin.id)] });
            }
            Ok(plugin) => scan.plugins.push(plugin),
            Err(errors) => scan.failures.push(PluginFailure { dir, errors }),
        }
    }
    scan
}

pub fn load(dir: &Path) -> Result<Plugin, Vec<String>> {
    let linked = std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink());
    if linked {
        linked_inside_home(dir).map_err(|error| vec![error])?;
    }
    let manifest_path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| vec![format!("cannot read {}: {error}", manifest_path.display())])?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|error| vec![format!("invalid {MANIFEST}: {}", error.message())])?;
    let mut errors: Vec<String> = Vec::new();
    if !valid_id(&manifest.id) {
        errors.push(format!("bad id: {:?} (use lowercase letters, digits and single hyphens)", manifest.id));
    }
    let folder = dir
        .canonicalize()
        .ok()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()).map(str::to_string))
        .unwrap_or_default();
    if folder != manifest.id {
        errors.push(format!("id {:?} does not match the folder name {folder:?}", manifest.id));
    }
    if !valid_semver(&manifest.version) {
        errors.push(format!("bad version: {:?} (use semver, such as 1.0.0)", manifest.version));
    }
    if manifest.description.trim().is_empty() {
        errors.push("description is empty".to_string());
    }
    if linked && !manifest.capabilities.is_empty() {
        errors.push(format!(
            "the plugin folder is a symlink and the plugin asks for capabilities ({}): {} (install the folder it points at with local-studio plugin install <dir>)",
            manifest.capabilities.join(", "),
            dir.display()
        ));
    }
    let capabilities = capabilities_in(&manifest.capabilities, &mut errors);
    let gates = gates_in(&manifest.gates, &capabilities, &mut errors);
    let icon = manifest.icon.as_deref().and_then(|relative| match inside(dir, relative) {
        Ok(path) if path.is_file() => Some(path),
        Ok(path) => {
            errors.push(format!("icon is missing: {}", path.display()));
            None
        }
        Err(error) => {
            errors.push(format!("icon: {error}"));
            None
        }
    });
    let name = manifest.name.map(|name| name.trim().to_string()).unwrap_or_default();
    let mcp = match (manifest.mcp, capabilities.contains(&Capability::McpServer)) {
        (Some(mcp), true) => mcp_in(dir, &manifest.id, mcp, &mut errors),
        (Some(_), false) => {
            errors.push("[mcp] needs the mcp-server capability".to_string());
            None
        }
        (None, true) => {
            errors.push("the mcp-server capability needs an [mcp] table".to_string());
            None
        }
        (None, false) => None,
    };
    let skills_dir = match &manifest.skills {
        Some(relative) => listed_dir(dir, relative, "skills", &mut errors),
        None => Some(dir.join("skills")).filter(|path| path.is_dir()),
    };
    let mut skipped: Vec<PathBuf> = Vec::new();
    let skills = skills_dir.map(|path| skills_in(dir, &path, &mut errors, &mut skipped)).unwrap_or_default();
    let prompts = manifest
        .prompts
        .as_deref()
        .and_then(|relative| listed_dir(dir, relative, "prompts", &mut errors))
        .map(|path| prompts_in(dir, &path, &mut skipped))
        .unwrap_or_default();
    let commands = commands_in(dir, manifest.commands, &skills, &mut errors, &mut skipped);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Plugin {
        name: if name.is_empty() { manifest.id.clone() } else { name },
        id: manifest.id,
        icon,
        version: manifest.version,
        description: manifest.description,
        gates,
        dir: dir.to_path_buf(),
        skills,
        prompts,
        commands,
        capabilities,
        mcp,
        local_only: manifest.local_only,
        skipped,
    })
}

fn linked_inside_home(dir: &Path) -> Result<(), String> {
    let install = "install the folder it points at with local-studio plugin install <dir>";
    let target = dir
        .canonicalize()
        .map_err(|error| format!("the plugin folder is a symlink that points nowhere: {} ({error})", dir.display()))?;
    let home = crate::store::home_dir().and_then(|home| home.canonicalize().ok());
    if home.is_some_and(|home| target != home && target.starts_with(&home)) {
        Ok(())
    } else {
        Err(format!(
            "the plugin folder is a symlink to {}, outside your home folder: {} ({install})",
            target.display(),
            dir.display()
        ))
    }
}

fn capabilities_in(names: &[String], errors: &mut Vec<String>) -> Vec<Capability> {
    let mut capabilities: Vec<Capability> = Vec::new();
    for name in names {
        match Capability::parse(name) {
            Some(capability) if capabilities.contains(&capability) => {
                errors.push(format!("duplicate capability: {name}"));
            }
            Some(capability) => capabilities.push(capability),
            None => errors.push(format!("unknown capability: {name}")),
        }
    }
    capabilities
}

fn gates_in(
    listed: &BTreeMap<String, String>,
    capabilities: &[Capability],
    errors: &mut Vec<String>,
) -> BTreeMap<Capability, String> {
    let mut gates: BTreeMap<Capability, String> = BTreeMap::new();
    for (name, what) in listed {
        match Capability::parse(name) {
            Some(capability) if !capabilities.contains(&capability) => {
                errors.push(format!("gates {name}: the plugin does not ask for {name}"));
            }
            Some(capability) if !capability.sensitive() => {
                errors.push(format!("gates {name}: only a sensitive capability can be switched off"));
            }
            Some(_) if what.trim().is_empty() => errors.push(format!("gates {name} is empty")),
            Some(capability) => {
                gates.insert(capability, what.trim().to_string());
            }
            None => errors.push(format!("gates: unknown capability: {name}")),
        }
    }
    gates
}

fn mcp_in(dir: &Path, id: &str, mcp: ManifestMcp, errors: &mut Vec<String>) -> Option<McpServer> {
    let command = match inside(dir, &mcp.command) {
        Ok(path) if path.is_file() => path,
        Ok(path) => {
            errors.push(format!("[mcp] command is missing: {}", path.display()));
            return None;
        }
        Err(error) => {
            errors.push(format!("[mcp] command: {error}"));
            return None;
        }
    };
    let before = errors.len();
    let names = |path: &Path| path.components().any(|part| install::named(part.as_os_str(), install::UNHASHED));
    let base = std::fs::canonicalize(dir).ok();
    let unhashed = std::iter::once(&mcp.command).chain(&mcp.args).flat_map(|path| path.split('=')).any(|part| {
        names(Path::new(part))
            || base.as_ref().is_some_and(|base| {
                std::fs::canonicalize(dir.join(part)).is_ok_and(|real| real.strip_prefix(base).is_ok_and(&names))
            })
    });
    if unhashed {
        errors.push(format!("[mcp] runs a {} file, which no hash covers", install::UNHASHED));
    }
    let prefix = env_prefix(id);
    for (name, host) in &mcp.env {
        if !valid_env_name(name) {
            errors.push(format!("[mcp] env: bad variable name: {name:?}"));
        }
        if [CALLER_ENV, CALLER_NAME_ENV].contains(&name.as_str()) {
            errors.push(format!("[mcp] env: {name} is set by Local Studio"));
        }
        if !valid_env_name(host) || !host.starts_with(&prefix) {
            errors.push(format!("[mcp] env {name}: the host variable must start with {prefix}, not {host:?}"));
        }
    }
    (errors.len() == before).then_some(McpServer { command, args: mcp.args, env: mcp.env })
}

#[must_use]
pub fn env_prefix(id: &str) -> String {
    format!("LOCAL_STUDIO_PLUGIN_{}__", id.to_ascii_uppercase().replace('-', "_"))
}

pub(crate) fn valid_env_name(name: &str) -> bool {
    name.chars().next().is_some_and(|first| first.is_ascii_uppercase() || first == '_')
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[must_use]
pub fn ready<'a>(plugins: &'a [Plugin], grants: &Grants) -> Vec<Standing<'a>> {
    plugins
        .iter()
        .map(|plugin| {
            let (consent, held) = grants.standing(plugin);
            (plugin, consent, held)
        })
        .collect()
}

fn mcp_servers(ready: &[Standing]) -> Map<String, Value> {
    ready
        .iter()
        .filter(|(plugin, consent, _)| {
            !plugin.local_only && *consent == Consent::Ready && plugin.asks(Capability::McpServer)
        })
        .filter_map(|(plugin, _, held)| {
            let mcp = plugin.mcp.as_ref()?;
            let dir = absolute(&plugin.dir);
            let mut env: Map<String, Value> =
                mcp.env.iter().map(|(name, host)| (name.clone(), json!({ "env": host }))).collect();
            env.insert(NO_BYTECODE.0.to_string(), json!({ "env": NO_BYTECODE.1 }));
            let mut entry = json!({
                "type": "stdio",
                "command": absolute(&mcp.command).to_string_lossy(),
                "args": plugin.args(held),
                "cwd": dir.to_string_lossy(),
                MCP_TAG: true,
            });
            entry["env"] = Value::Object(env);
            Some((plugin.mcp_name(), entry))
        })
        .collect()
}

pub fn session_servers(
    plugins: &[Plugin],
    grants: &Grants,
    wanted: &[String],
    caller: Option<&Caller>,
) -> Result<Vec<Value>, Vec<String>> {
    let mut servers = Vec::new();
    let mut missing = Vec::new();
    for plugin in plugins.iter().filter(|plugin| plugin.local_only && wanted.contains(&plugin.id)) {
        let (consent, held) = grants.standing(plugin);
        if consent != Consent::Ready || !plugin.asks(Capability::McpServer) {
            continue;
        }
        let Some(mcp) = plugin.mcp.as_ref() else { continue };
        let mut env = Map::new();
        for (name, host) in &mcp.env {
            match std::env::var(host) {
                Ok(value) => {
                    env.insert(name.clone(), Value::String(value));
                }
                Err(_) => missing.push(format!("{} needs {host}", plugin.id)),
            }
        }
        env.insert(NO_BYTECODE.0.to_string(), json!("1"));
        env.insert(LOCAL_ONLY_ENV.to_string(), json!("1"));
        if let Some(caller) = caller {
            caller.insert(&mut env);
        }
        servers.push(json!({
            "type": "stdio",
            "name": plugin.mcp_name(),
            "command": absolute(&mcp.command).to_string_lossy(),
            "args": plugin.args(&held),
            "cwd": absolute(&plugin.dir).to_string_lossy(),
            "env": env,
        }));
    }
    if missing.is_empty() {
        Ok(servers)
    } else {
        Err(missing)
    }
}

fn written_by_us(root: &Path, name: &str, entry: &Value) -> bool {
    name.starts_with(MCP_PREFIX)
        && match entry.get(MCP_TAG) {
            Some(tag) => tag == &Value::Bool(true),
            None => entry.get("command").and_then(Value::as_str).map(Path::new).is_some_and(|command| {
                command.starts_with(root) && !command.components().any(|part| part == std::path::Component::ParentDir)
            }),
        }
}

#[must_use]
pub fn foreign_servers(root: &Path, agent: &Path) -> Vec<String> {
    let root = absolute(root);
    let settings = crate::workspace::providers::read_object(&agent.join("settings.json")).unwrap_or_default();
    let servers = settings.get("mcpServers").and_then(Value::as_object).cloned().unwrap_or_default();
    servers.into_iter().filter(|(name, entry)| !written_by_us(&root, name, entry)).map(|(name, _)| name).collect()
}

pub fn sync_mcp_servers(root: &Path, agent: &Path, ready: &[Standing]) -> Result<bool, String> {
    let path = agent.join("settings.json");
    let mut settings = crate::workspace::providers::read_object(&path)?;
    let mut servers = match settings.remove("mcpServers") {
        None => Map::new(),
        Some(Value::Object(servers)) => servers,
        Some(_) => return Err(format!("{}: mcpServers is not a JSON object", path.display())),
    };
    let root = absolute(root);
    let ours = |name: &str, entry: &Value| written_by_us(&root, name, entry);
    let (wanted, taken): (Map<String, Value>, Map<String, Value>) =
        mcp_servers(ready).into_iter().partition(|(name, _)| servers.get(name).is_none_or(|entry| ours(name, entry)));
    let current: Map<String, Value> =
        servers.iter().filter(|(name, entry)| ours(name, entry)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let changed = current != wanted;
    if changed {
        servers.retain(|name, entry| !ours(name, entry));
        servers.extend(wanted);
        settings.insert("mcpServers".to_string(), Value::Object(servers));
        crate::workspace::providers::write_private(&path, &Value::Object(settings))?;
    }
    if taken.is_empty() {
        return Ok(changed);
    }
    let names: Vec<&str> = taken.keys().map(String::as_str).collect();
    Err(format!(
        "{}: mcpServers already has {} that Local Studio did not write, so it left them alone and that plugin MCP server is off",
        path.display(),
        names.join(", ")
    ))
}

#[must_use]
pub fn agent_dir() -> Option<PathBuf> {
    crate::daemon::agent_dir()
}

pub fn sync_agent(root: &Path, agent: &Path) -> Result<bool, String> {
    let grants = Grants::load(root)?;
    let plugins = scan(root).plugins;
    sync_mcp_servers(root, agent, &ready(&plugins, &grants))
}

pub(crate) fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().or_else(|_| std::path::absolute(path)).unwrap_or_else(|_| path.to_path_buf())
}

fn listed_dir(dir: &Path, relative: &str, what: &str, errors: &mut Vec<String>) -> Option<PathBuf> {
    match inside(dir, relative) {
        Ok(path) if path.is_dir() => Some(path),
        Ok(path) => {
            errors.push(format!("missing {what} dir: {}", path.display()));
            None
        }
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn commands_in(
    dir: &Path,
    listed: Vec<ManifestCommand>,
    skills: &[PluginFile],
    errors: &mut Vec<String>,
    skipped: &mut Vec<PathBuf>,
) -> Vec<PluginCommand> {
    let mut names: HashSet<String> = HashSet::new();
    let mut commands: Vec<PluginCommand> = Vec::new();
    for command in listed {
        if !valid_command_name(&command.name) {
            errors.push(format!("bad command name: {:?}", command.name));
            continue;
        }
        if !names.insert(command.name.clone()) {
            errors.push(format!("duplicate command name: {}", command.name));
            continue;
        }
        let target = match (command.prompt, command.skill) {
            (Some(prompt), None) => match inside(dir, &prompt) {
                Ok(path) => match instruction(dir, &path) {
                    Ok(Some(_)) => CommandTarget::Prompt(path),
                    Ok(None) => {
                        errors.push(format!("command {}: missing prompt file: {}", command.name, path.display()));
                        continue;
                    }
                    Err(_) => {
                        skipped.push(path);
                        continue;
                    }
                },
                Err(error) => {
                    errors.push(format!("command {}: {error}", command.name));
                    continue;
                }
            },
            (None, Some(skill)) if skills.iter().any(|entry| entry.name == skill) => CommandTarget::Skill(skill),
            (None, Some(skill)) => {
                errors.push(format!("command {}: missing skill: {skill}", command.name));
                continue;
            }
            _ => {
                errors.push(format!("command {}: set exactly one of prompt or skill", command.name));
                continue;
            }
        };
        commands.push(PluginCommand { name: command.name, description: command.description, target });
    }
    commands
}

#[must_use]
pub fn commands(plugins: &[Plugin]) -> Vec<SlashCommand> {
    plugins
        .iter()
        .flat_map(|plugin| {
            plugin.commands.iter().map(|command| SlashCommand {
                name: command.name.clone(),
                description: if plugin.local_only {
                    format!("{} ({LOCAL_ONLY_LABEL})", command.description)
                } else {
                    command.description.clone()
                },
                source: plugin.source(),
                kind: CommandKind::Plugin,
                origin: plugin.name.clone(),
            })
        })
        .collect()
}

pub(crate) fn inside(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|part| !matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir))
    {
        return Err(format!("path must stay inside the plugin: {relative}"));
    }
    Ok(dir.join(path))
}

pub(crate) fn instruction(dir: &Path, path: &Path) -> Result<Option<String>, String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return Ok(None) };
    let linked = meta.file_type().is_symlink();
    let real = match path.canonicalize() {
        Ok(real) => real,
        Err(_) if linked => return Err("a symlink that points nowhere; Local Studio skips it".to_string()),
        Err(_) => return Ok(None),
    };
    let base = dir.canonicalize().map_err(|error| format!("cannot resolve {}: {error}", dir.display()))?;
    if !real.starts_with(&base) {
        return Err("a link that leaves the plugin; Local Studio skips it".to_string());
    }
    let target = std::fs::metadata(&real).map_err(|error| format!("cannot read it: {error}"))?;
    if !target.is_file() {
        return if linked {
            Err("a symlink to something other than a regular file; Local Studio skips it".to_string())
        } else {
            Ok(None)
        };
    }
    let mut bytes: Vec<u8> = Vec::new();
    std::fs::File::open(&real)
        .and_then(|file| std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_FILE + 1), &mut bytes))
        .map_err(|error| format!("cannot read it: {error}"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("over 10 MB; Local Studio skips it".to_string());
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn skills_in(plugin: &Path, dir: &Path, errors: &mut Vec<String>, skipped: &mut Vec<PathBuf>) -> Vec<PluginFile> {
    let mut skills: Vec<PluginFile> = Vec::new();
    for path in sorted_entries(dir).into_iter().filter(|path| path.is_dir()) {
        let file = path.join("SKILL.md");
        let content = match instruction(plugin, &file) {
            Ok(Some(content)) => content,
            Ok(None) => {
                errors.push(format!("missing skill file: {}", file.display()));
                continue;
            }
            Err(_) => {
                skipped.push(file);
                continue;
            }
        };
        let folder = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
        let name = frontmatter::field(&content, "name").unwrap_or(folder);
        let description = frontmatter::field(&content, "description").unwrap_or_default();
        skills.push(PluginFile { name, description, path: file });
    }
    skills.dedup_by(|a, b| a.name == b.name);
    skills
}

fn prompts_in(plugin: &Path, dir: &Path, skipped: &mut Vec<PathBuf>) -> Vec<PluginFile> {
    let mut prompts: Vec<PluginFile> = Vec::new();
    for path in sorted_entries(dir).into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "md")) {
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()).map(str::to_string) else { continue };
        let content = match instruction(plugin, &path) {
            Ok(Some(content)) => content,
            Ok(None) => continue,
            Err(_) => {
                skipped.push(path);
                continue;
            }
        };
        let description = frontmatter::field(&content, "description").unwrap_or_else(|| {
            frontmatter::strip(&content)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or_default()
                .to_string()
        });
        prompts.push(PluginFile { name, description, path });
    }
    prompts
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    entries
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("skill:")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

pub(crate) fn valid_semver(version: &str) -> bool {
    let (core, build) = version.split_once('+').map_or((version, None), |(core, build)| (core, Some(build)));
    let (core, pre) = core.split_once('-').map_or((core, None), |(core, pre)| (core, Some(pre)));
    let numbers: Vec<&str> = core.split('.').collect();
    let number = |part: &&str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) && (*part == "0" || !part.starts_with('0'))
    };
    let ident = |text: &str| {
        text.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    };
    numbers.len() == 3 && numbers.iter().all(number) && pre.is_none_or(ident) && build.is_none_or(ident)
}
