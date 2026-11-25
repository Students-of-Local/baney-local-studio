use std::path::{Path, PathBuf};

use eukhe_types::slash_commands::parse_slash_command;
use serde_json::Value;

use crate::frontmatter;
use crate::model::{CommandKind, SkillEntry, SkillSource, SlashCommand};
use crate::plugins::{self, CommandTarget, Plugin};

mod create;
mod read;
mod search;
mod serve;
mod store;
mod text;

pub use create::{asked, skill_marks, Draft, CREATE, MAKE};
pub use read::Reading;
pub use search::{lines, Hit, Query};
pub(crate) use serve::{entry, TOOLS};
pub use serve::{serve, Server, SERVER};
pub use store::{roots, Listed, Roots, Store};
pub use text::plain;

const BLOCKS: [(&str, &str); 2] = [("<skill name=\"", "\n</skill>\n\n"), ("<plugin name=\"", "\n</plugin>\n\n")];

const SESSION_COMMANDS: [(&str, &str); 7] = [
    ("compact", "Compact the conversation context"),
    ("refine", "Refine the last answer"),
    ("goal", "Set, pause, resume or clear the session goal"),
    ("autonomous", "Run autonomously until the goal is met"),
    ("remind", "Schedule a reminder in this session"),
    ("force", "Force the next step past a guard"),
    ("dynamic-workflow", "Start a dynamic workflow"),
];

#[derive(Debug, Clone, Default)]
pub(crate) struct Resources {
    skills: Vec<Resource>,
    templates: Vec<Resource>,
    plugins: Vec<Plugin>,
}

#[derive(Debug, Clone)]
struct Resource {
    name: String,
    file_path: String,
    description: String,
    source: SkillSource,
    plugin: Option<(String, String)>,
}

impl Resource {
    fn label(&self) -> String {
        self.plugin.as_ref().map_or_else(|| source_label(self.source).to_string(), |(id, _)| format!("plugin:{id}"))
    }

    fn origin(&self) -> String {
        self.plugin.as_ref().map(|(_, name)| name.clone()).unwrap_or_default()
    }

    fn from_plugin(plugin: &Plugin, file: &plugins::PluginFile) -> Self {
        Self {
            name: file.name.clone(),
            file_path: file.path.display().to_string(),
            description: file.description.clone(),
            source: SkillSource::Personal,
            plugin: Some((plugin.id.clone(), plugin.name.clone())),
        }
    }
}

impl Resources {
    pub(crate) fn parse(data: &Value) -> Self {
        let list = |key: &str| -> Vec<Resource> {
            data.get(key)
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            Some(Resource {
                                name: entry.get("name")?.as_str()?.to_string(),
                                file_path: entry.get("filePath")?.as_str()?.to_string(),
                                description: entry
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                source: source(entry.get("sourceInfo")),
                                plugin: None,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Self { skills: list("skills"), templates: list("prompts"), plugins: Vec::new() }
    }

    pub(crate) fn with_plugins(mut self, plugins: Vec<Plugin>) -> Self {
        self.plugins = plugins;
        self
    }

    fn all_skills(&self) -> Vec<Resource> {
        let mut all = self.skills.clone();
        for plugin in &self.plugins {
            all.extend(plugin.skills.iter().map(|file| Resource::from_plugin(plugin, file)));
        }
        all
    }

    fn all_templates(&self) -> Vec<Resource> {
        let mut all = self.templates.clone();
        for plugin in &self.plugins {
            all.extend(plugin.prompts.iter().map(|file| Resource::from_plugin(plugin, file)));
        }
        all
    }

    pub(crate) fn skills(&self) -> Vec<SkillEntry> {
        self.all_skills()
            .iter()
            .map(|skill| SkillEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source: skill.source,
                path: skill.file_path.clone(),
            })
            .collect()
    }

    pub(crate) fn commands(&self) -> Vec<SlashCommand> {
        let mut commands: Vec<SlashCommand> = vec![SlashCommand {
            name: "model".to_string(),
            description: "Switch the response model for this thread".to_string(),
            source: "app".to_string(),
            kind: CommandKind::Command,
            origin: String::new(),
        }];
        commands.extend(SESSION_COMMANDS.iter().map(|(name, description)| SlashCommand {
            name: (*name).to_string(),
            description: (*description).to_string(),
            source: "session".to_string(),
            kind: CommandKind::Command,
            origin: String::new(),
        }));
        commands.extend(self.all_templates().iter().map(|template| SlashCommand {
            name: template.name.clone(),
            description: template.description.clone(),
            source: template.label(),
            kind: CommandKind::Template,
            origin: template.origin(),
        }));
        commands.extend(self.all_skills().iter().map(|skill| SlashCommand {
            name: format!("skill:{}", skill.name),
            description: skill.description.clone(),
            source: skill.label(),
            kind: CommandKind::Skill,
            origin: skill.origin(),
        }));
        commands.extend(plugins::commands(&self.plugins));
        commands
    }

    fn local_only<'a>(&'a self, text: &'a str, at: &'a At) -> impl Iterator<Item = String> + 'a {
        let named =
            move |plugin: &&Plugin| plugin.local_only && (at.names(text, &plugin.id) || at.names(text, &plugin.name));
        self.plugins.iter().filter(named).map(|plugin| plugin.id.clone())
    }

    pub(crate) fn expand(&self, text: &str, at: &At) -> Expansion {
        let mut expansion = self.expanded(text, at);
        expansion.plugins.extend(self.local_only(text, at));
        expansion
    }

    fn expanded(&self, text: &str, at: &At) -> Expansion {
        let skills = self.all_skills();
        let plain = || Expansion { text: text.to_string(), plugins: Vec::new() };
        if let Some((name, args)) = parse_slash_command(text) {
            if let Some(skill_name) = name.strip_prefix("skill:") {
                return skills
                    .iter()
                    .find(|skill| skill.name == skill_name)
                    .and_then(|skill| Some((skill_block(skill)?, skill)))
                    .map_or_else(plain, |(block, skill)| Expansion::from(with_args(block, &args), [skill]));
            }
            if let Some(template) = self.all_templates().iter().find(|template| template.name == name) {
                if let Ok(content) = std::fs::read_to_string(&template.file_path) {
                    return Expansion::from(substitute_args(&frontmatter::strip(&content), &words(&args)), [template]);
                }
            }
            return self.expand_plugin(&name, &args).unwrap_or_else(plain);
        }
        let listed = at.resources(text);
        let dollar: Vec<(&str, Named)> =
            skills.iter().map(|skill| (skill.name.as_str(), Named::Skill(skill))).collect();
        let mut named: Vec<(&str, Named)> = listed
            .iter()
            .chain(skills.iter().filter(|skill| skill.plugin.is_some()))
            .map(|skill| (skill.name.as_str(), Named::Skill(skill)))
            .collect();
        for plugin in &self.plugins {
            named.extend([&plugin.id, &plugin.name].map(|name| (name.as_str(), Named::Plugin(plugin))));
        }
        let mut used: Vec<PathBuf> = Vec::new();
        let mut blocks: Vec<String> = Vec::new();
        let mut plugins: Vec<String> = Vec::new();
        for (index, mark) in text.match_indices(['$', '@']).skip(usize::from(asked(text))) {
            let rest = &text[index + 1..];
            let found = if mark == "$" {
                longest(rest, &dollar, |after| !after.starts_with(in_name))
            } else if starts_word(&text[..index]) {
                longest(rest, &named, ends_word).filter(|(name, _)| !at.path(rest, name))
            } else {
                None
            };
            let Some((_, found)) = found else { continue };
            let (key, block, owner) = match found {
                Named::Skill(skill) => {
                    (file(&skill.file_path), skill_block(skill), skill.plugin.as_ref().map(|(id, _)| id.clone()))
                }
                Named::Plugin(plugin) => {
                    let (key, block) = match plugin.skills.as_slice() {
                        [only] => (file(&only.path), skill_block(&Resource::from_plugin(plugin, only))),
                        _ => (
                            PathBuf::from(&plugin.id),
                            Some(plugin_block(plugin, "Read a skill's file before you use it.")),
                        ),
                    };
                    (key, block, Some(plugin.id.clone()))
                }
            };
            plugins.extend(owner);
            let Some(block) = block.filter(|_| !used.contains(&key)) else { continue };
            used.push(key);
            blocks.push(block);
        }
        blocks.push(text.to_string());
        Expansion { text: blocks.join("\n\n"), plugins }
    }
}

#[derive(Debug, Default)]
pub(crate) struct At {
    pub skills: Vec<Listed>,
    pub here: Option<PathBuf>,
}

impl At {
    fn resources(&self, text: &str) -> Vec<Resource> {
        let own = self.skills.iter().filter(|skill| {
            !skill.source.starts_with("plugin ") && text.contains(&format!("@{}", skill.name)) && skill.unmoved()
        });
        own.map(|skill| Resource {
            name: skill.name.clone(),
            file_path: skill.path.display().to_string(),
            description: skill.description.clone(),
            source: SkillSource::Personal,
            plugin: None,
        })
        .collect()
    }

    fn path(&self, rest: &str, name: &str) -> bool {
        let longer = |entry: std::fs::DirEntry| {
            let file = entry.file_name();
            file.to_str().is_some_and(|file| file.starts_with(name) && rest.starts_with(file))
        };
        self.here.as_ref().is_some_and(|here| {
            here.join(name).exists() || std::fs::read_dir(here).is_ok_and(|all| all.flatten().any(longer))
        })
    }

    pub(crate) fn names(&self, text: &str, name: &str) -> bool {
        let is = |rest: &str| {
            let Some((head, after)) = rest.split_at_checked(name.len()) else { return false };
            head.eq_ignore_ascii_case(name) && ends_word(after) && !self.path(rest, head)
        };
        !name.is_empty() && mentions(text).any(|(_, rest)| is(rest))
    }
}

#[derive(Clone, Copy)]
enum Named<'a> {
    Skill(&'a Resource),
    Plugin(&'a Plugin),
}

fn longest<'a>(
    rest: &str,
    named: &[(&'a str, Named<'a>)],
    ends: impl Fn(&str) -> bool,
) -> Option<(&'a str, Named<'a>)> {
    named
        .iter()
        .filter(|(name, _)| !name.is_empty() && rest.strip_prefix(name).is_some_and(&ends))
        .copied()
        .reduce(|best, next| if next.0.len() > best.0.len() { next } else { best })
}

fn in_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_')
}

fn file(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().canonicalize().unwrap_or_else(|_| path.as_ref().to_path_buf())
}

pub(crate) fn mentions(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let marks = text.match_indices('@').filter(move |(index, _)| starts_word(&text[..*index]));
    marks.map(|(index, _)| (index, &text[index + 1..]))
}

fn starts_word(before: &str) -> bool {
    before.chars().next_back().is_none_or(|c| c.is_whitespace() || "([{\"'".contains(c))
}

fn ends_word(after: &str) -> bool {
    let mut next = after.chars();
    match next.next() {
        Some('.') => !next.next().is_some_and(char::is_alphanumeric),
        Some(c) => !in_name(c) && !matches!(c, '/' | '\\' | '@'),
        None => true,
    }
}

fn plugin_block(plugin: &Plugin, read: &str) -> String {
    let mut lines = vec![format!("<plugin name=\"{}\" id=\"{}\">", plugin.name, plugin.id), plugin.description.clone()];
    let skill =
        |file: &plugins::PluginFile| format!("- skill {}: {} ({})", file.name, file.description, file.path.display());
    lines.extend(plugin.skills.iter().map(skill));
    let command = |command: &plugins::PluginCommand| format!("- command /{}: {}", command.name, command.description);
    lines.extend(plugin.commands.iter().map(command));
    lines.extend((!plugin.skills.is_empty()).then(|| read.to_string()));
    lines.push("</plugin>".to_string());
    lines.join("\n")
}

pub(crate) struct Expansion {
    pub text: String,
    pub plugins: Vec<String>,
}

impl Expansion {
    fn from<'a>(text: String, resources: impl IntoIterator<Item = &'a Resource>) -> Self {
        Self {
            text,
            plugins: resources
                .into_iter()
                .filter_map(|resource| resource.plugin.as_ref().map(|(id, _)| id.clone()))
                .collect(),
        }
    }
}

impl Resources {
    fn expand_plugin(&self, name: &str, args: &str) -> Option<Expansion> {
        let (plugin, command) = self.plugins.iter().find_map(|plugin| {
            plugin.commands.iter().find(|command| command.name == name).map(|command| (plugin, command))
        })?;
        let text = match &command.target {
            CommandTarget::Prompt(path) => {
                let content = std::fs::read_to_string(path).ok()?;
                substitute_args(&frontmatter::strip(&content), &words(args))
            }
            CommandTarget::Skill(skill) => {
                let file = plugin.skill(skill)?;
                skill_block(&Resource::from_plugin(plugin, file)).map(|block| with_args(block, args))?
            }
        };
        Some(Expansion { text, plugins: vec![plugin.id.clone()] })
    }
}

pub(crate) fn typed(sent: &str) -> String {
    let mut text = sent;
    let made = text.strip_prefix(CREATE).filter(|rest| rest.is_empty() || rest.starts_with("\n\n"));
    text = made.map_or(text, str::trim_start);
    while let Some(after) = BLOCKS.iter().find_map(|(open, close)| {
        let block = text.strip_prefix(open)?;
        block.find(close).map(|at| &block[at + close.len()..])
    }) {
        if after.is_empty() {
            break;
        }
        text = after;
    }
    match made {
        Some(_) => format!("{MAKE} {text}").trim_end().to_string(),
        None => text.to_string(),
    }
}

fn with_args(block: String, args: &str) -> String {
    if args.trim().is_empty() {
        block
    } else {
        // Small models lose a request that sits after a long skill, so it also opens the prompt.
        format!("The person's request: {}\n\n{block}\n\n{args}", args.trim())
    }
}

fn skill_block(skill: &Resource) -> Option<String> {
    let content = std::fs::read_to_string(&skill.file_path).ok()?;
    let base = Path::new(&skill.file_path).parent().map(|dir| dir.display().to_string()).unwrap_or_default();
    Some(format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        base,
        frontmatter::strip(&content).trim()
    ))
}

fn source(info: Option<&Value>) -> SkillSource {
    let text = |key: &str| info.and_then(|info| info.get(key)).and_then(Value::as_str).unwrap_or_default();
    if text("origin") == "package" {
        return SkillSource::System;
    }
    match text("scope") {
        "user" => SkillSource::Personal,
        "project" => SkillSource::Repo,
        "temporary" => SkillSource::App,
        _ => SkillSource::System,
    }
}

fn source_label(source: SkillSource) -> &'static str {
    match source {
        SkillSource::App => "app",
        SkillSource::Repo => "repo",
        SkillSource::Project => "project",
        SkillSource::Personal => "personal",
        SkillSource::System => "system",
    }
}

fn words(args: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in args.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

fn substitute_args(content: &str, args: &[String]) -> String {
    let all = args.join(" ");
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        if c != '$' {
            out.push(c);
            index += 1;
            continue;
        }
        let rest: String = chars[index..].iter().take(10).collect();
        if rest.starts_with("$ARGUMENTS") {
            out.push_str(&all);
            index += "$ARGUMENTS".len();
            continue;
        }
        match chars.get(index + 1) {
            Some('@') => {
                out.push_str(&all);
                index += 2;
            }
            Some('{') => {
                let close = chars[index + 2..].iter().position(|c| *c == '}');
                let inner: Option<String> = close.map(|close| chars[index + 2..index + 2 + close].iter().collect());
                if let (Some(close), Some(range)) = (close, inner.as_deref().and_then(|inner| inner.strip_prefix("@:")))
                {
                    let mut parts = range.splitn(2, ':');
                    let start = parts.next().and_then(|start| start.parse::<usize>().ok()).unwrap_or(1);
                    let skip = start.saturating_sub(1);
                    let slice: Vec<&str> = match parts.next().and_then(|length| length.parse::<usize>().ok()) {
                        Some(length) => args.iter().skip(skip).take(length).map(String::as_str).collect(),
                        None => args.iter().skip(skip).map(String::as_str).collect(),
                    };
                    out.push_str(&slice.join(" "));
                    index += 2 + close + 1;
                } else {
                    out.push('$');
                    index += 1;
                }
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut end = index + 1;
                while chars.get(end).is_some_and(char::is_ascii_digit) {
                    end += 1;
                }
                let number: String = chars[index + 1..end].iter().collect();
                if let Some(value) = number.parse::<usize>().ok().and_then(|n| args.get(n.saturating_sub(1))) {
                    out.push_str(value);
                }
                index = end;
            }
            _ => {
                out.push('$');
                index += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod request_tests {
    use super::with_args;

    #[test]
    fn the_request_opens_and_closes_an_expanded_skill() {
        let text = with_args("<skill>body</skill>".into(), " a fan guard ");
        assert!(text.starts_with("The person's request: a fan guard\n"));
        assert!(text.ends_with("</skill>\n\n a fan guard "));
        assert_eq!(with_args("<skill>body</skill>".into(), "  "), "<skill>body</skill>");
    }
}
