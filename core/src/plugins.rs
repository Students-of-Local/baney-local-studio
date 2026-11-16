use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::{CommandKind, SlashCommand};

pub const MANIFEST: &str = "plugin.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub id: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    pub skills: Vec<PluginFile>,
    pub prompts: Vec<PluginFile>,
    pub commands: Vec<PluginCommand>,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    version: String,
    description: String,
    skills: Option<String>,
    prompts: Option<String>,
    #[serde(default)]
    commands: Vec<ManifestCommand>,
    #[serde(default)]
    capabilities: Vec<String>,
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
    crate::store::home_dir().map(|home| home.join(".local-studio").join("plugins"))
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
    let manifest_path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| vec![format!("cannot read {}: {error}", manifest_path.display())])?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|error| vec![format!("invalid {MANIFEST}: {}", error.message())])?;
    let mut errors: Vec<String> = Vec::new();
    if !valid_id(&manifest.id) {
        errors.push(format!("bad id: {:?} (use lowercase letters, digits and hyphens)", manifest.id));
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
    errors.extend(manifest.capabilities.iter().map(|name| format!("capability not supported yet: {name}")));
    let skills_dir = match &manifest.skills {
        Some(relative) => listed_dir(dir, relative, "skills", &mut errors),
        None => Some(dir.join("skills")).filter(|path| path.is_dir()),
    };
    let skills = skills_dir.map(|path| skills_in(&path, &mut errors)).unwrap_or_default();
    let prompts = manifest
        .prompts
        .as_deref()
        .and_then(|relative| listed_dir(dir, relative, "prompts", &mut errors))
        .map(|path| prompts_in(&path))
        .unwrap_or_default();
    let commands = commands_in(dir, manifest.commands, &skills, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Plugin {
        id: manifest.id,
        version: manifest.version,
        description: manifest.description,
        dir: dir.to_path_buf(),
        skills,
        prompts,
        commands,
    })
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
                Ok(path) if path.is_file() => CommandTarget::Prompt(path),
                Ok(path) => {
                    errors.push(format!("command {}: missing prompt file: {}", command.name, path.display()));
                    continue;
                }
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
                description: command.description.clone(),
                source: plugin.source(),
                kind: CommandKind::Plugin,
            })
        })
        .collect()
}

fn inside(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|part| !matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir))
    {
        return Err(format!("path must stay inside the plugin: {relative}"));
    }
    Ok(dir.join(path))
}

fn skills_in(dir: &Path, errors: &mut Vec<String>) -> Vec<PluginFile> {
    let mut skills: Vec<PluginFile> = sorted_entries(dir)
        .into_iter()
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let file = path.join("SKILL.md");
            if !file.is_file() {
                errors.push(format!("missing skill file: {}", file.display()));
                return None;
            }
            let content = std::fs::read_to_string(&file).unwrap_or_default();
            let folder = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
            let name = frontmatter(&content, "name").unwrap_or(folder);
            let description = frontmatter(&content, "description").unwrap_or_default();
            Some(PluginFile { name, description, path: file })
        })
        .collect();
    skills.dedup_by(|a, b| a.name == b.name);
    skills
}

fn prompts_in(dir: &Path) -> Vec<PluginFile> {
    sorted_entries(dir)
        .into_iter()
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_string();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let description = frontmatter(&content, "description").unwrap_or_else(|| {
                content
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty() && *line != "---")
                    .unwrap_or_default()
                    .to_string()
            });
            Some(PluginFile { name, description, path })
        })
        .collect()
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    entries
}

fn frontmatter(content: &str, key: &str) -> Option<String> {
    let normalized = content.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("skill:")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

fn valid_semver(version: &str) -> bool {
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
