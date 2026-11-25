use std::fs::{create_dir, create_dir_all, remove_dir_all, rename};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::store::{Store, BESIDE, SKILL};
use super::text;
use crate::frontmatter;

const MOST_TEXT: usize = 64 * 1024;
const MOST_FILES: usize = 20;
const MOST_ALL: usize = 256 * 1024;
const FOLDER: usize = 60;
const LONG: usize = 24;
const NAMED: [&str; 7] = ["password", "passwd", "secret", "token", "cookie", "api key", "api_key"];

pub const CREATE: &str = "Turn what you did in this thread into one skill that a small local model can follow without this thread.
Reply with one block opened and closed by four backticks:

````skill
---
name: a short name
description: what it does and when to use it, in one line
---
What it is for. Then numbered steps under `##` headings: exact addresses, API calls with method and body, form fields
by their label (a selector only as a fallback), how to see that each step worked, what went wrong on the way and what
fixed it, and when to stop and ask the person.
````

Then, each in its own block: `asks` (five or more ways a person would ask for it, one a line), `where` (the sites or
apps it is for, one host or app name a line) and `file scripts/name.py` for a script it needs. Write only what you saw
work. No passwords, tokens, cookies, account numbers or other people's details: use <placeholders> the person fills in.";

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub name: String,
    pub description: String,
    pub folder: String,
    pub text: String,
    pub files: Vec<(String, String)>,
    pub marked: Vec<String>,
}

impl Draft {
    pub fn parse(reply: &str) -> Result<Self, String> {
        let blocks = blocks(reply);
        let mut skills = blocks.iter().filter(|block| block.info == "skill");
        let (Some(skill), None) = (skills.next(), skills.next()) else {
            return Err("The reply does not hold one skill block. Ask for it again.".to_string());
        };
        if skill.long < 4 {
            return Err("The skill block needs four backticks, so a code block inside it does not end it.".to_string());
        }
        let field = |key: &str| frontmatter::field(&skill.text, key).map(|value| text::line(&value));
        let name = field("name").unwrap_or_default();
        let description = field("description").unwrap_or_default();
        let folder = folder(&name);
        if folder.is_empty() || description.is_empty() {
            return Err("The skill needs a name and a description in its front matter.".to_string());
        }
        let mut files: Vec<(String, String)> = Vec::new();
        for block in &blocks {
            let path = match block.info.split_once(' ').unwrap_or((&block.info, "")) {
                (kind @ ("asks" | "where"), "") => format!("{kind}.txt"),
                ("file", path) => beside(path)?,
                _ => continue,
            };
            if files.iter().any(|(used, _)| used.eq_ignore_ascii_case(&path)) {
                return Err(format!("The reply holds {path} twice."));
            }
            files.push((path, block.text.clone()));
        }
        let texts = || files.iter().map(|(_, text)| text).chain([&skill.text]);
        if texts().any(|text| text.len() > MOST_TEXT) || texts().map(String::len).sum::<usize>() > MOST_ALL {
            return Err(format!("A skill is at most {} KB, {} KB a file.", MOST_ALL / 1024, MOST_TEXT / 1024));
        }
        if files.len() > MOST_FILES {
            return Err(format!("A skill has at most {MOST_FILES} files."));
        }
        let marked = texts().flat_map(|text| text.lines()).filter(|line| private(line)).map(text::line).collect();
        Ok(Self { name, description, folder, text: skill.text.clone(), files, marked })
    }
}

impl Store {
    pub fn save(&mut self, draft: &Draft) -> Result<PathBuf, String> {
        let skills = self.own()?;
        self.checked = None;
        let target = skills.join(&draft.folder);
        let mut old = Vec::new();
        for skill in self.list()? {
            let same = skill.source == "yours" && skill.name.to_lowercase() == draft.name.to_lowercase();
            let Some(folder) = skill.path.parent().filter(|_| same) else { continue };
            if folder.parent() != Some(skills.as_path()) {
                return Err(format!("{} already has this name. Move or rename it first.", skill.path.display()));
            }
            old.push(folder.to_path_buf());
        }
        if target.symlink_metadata().is_ok() && !old.contains(&target) {
            old.push(target.clone());
        }
        let staged = skills.join(format!(".making-{}", std::process::id()));
        let _ = remove_dir_all(&staged);
        let outcome = stage(&staged, draft).and_then(|()| swap(&skills, &staged, &target, &old));
        if outcome.is_err() {
            let _ = remove_dir_all(&staged);
        }
        self.checked = None;
        outcome.map(|()| target)
    }
}

struct Block {
    info: String,
    long: usize,
    text: String,
}

fn blocks(reply: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<(char, Block)> = None;
    for line in reply.lines() {
        let trimmed = line.trim_start();
        let fence = text::run(trimmed);
        match open.take() {
            Some((mark, block)) => {
                let ends = fence
                    .is_some_and(|(end, long)| end == mark && long >= block.long && trimmed[long..].trim().is_empty());
                if ends {
                    blocks.push(block);
                } else {
                    let text = block.text + line + "\n";
                    open = Some((mark, Block { text, ..block }));
                }
            }
            None => {
                if let Some((mark, long)) = fence {
                    let info = text::line(&trimmed[long..]);
                    open = Some((mark, Block { info, long, text: String::new() }));
                }
            }
        }
    }
    blocks
}

fn folder(name: &str) -> String {
    let name: String = name.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let words: Vec<&str> = name.split('-').filter(|word| !word.is_empty()).collect();
    words.join("-").chars().take(FOLDER).collect::<String>().trim_end_matches('-').to_string()
}

fn beside(path: &str) -> Result<String, String> {
    let plain = |part: Component| matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.'));
    let taken = BESIDE.iter().chain([&SKILL]).any(|name| path.eq_ignore_ascii_case(name));
    let odd = path.contains(['\\', ':']) || path.chars().any(char::is_control);
    if path.is_empty() || taken || odd || !Path::new(path).components().all(plain) {
        return Err(format!("The reply names a file a skill cannot hold: {}", text::plain(path)));
    }
    Ok(path.to_string())
}

fn private(line: &str) -> bool {
    let lower = line.to_lowercase();
    let filled = |value: &str| {
        let value = value.trim_start_matches(['"', '\'', '`', ' ']);
        value.split_whitespace().next().is_some_and(|word| word.len() > 5 && !word.starts_with(['<', '{', '$', '[']))
    };
    let named = NAMED.iter().any(|word| {
        lower.split_once(word).is_some_and(|(_, after)| {
            after.trim_start_matches(['"', '\'', '`', ' ']).strip_prefix([':', '=']).is_some_and(filled)
        })
    });
    let key = |word: &str| {
        word.len() >= LONG
            && word.contains(|c: char| c.is_ascii_digit())
            && word.contains(|c: char| c.is_ascii_alphabetic())
    };
    let words = line.split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '+' | '='));
    named || lower.split_once("bearer ").is_some_and(|(_, after)| filled(after)) || words.into_iter().any(key)
}

fn failed(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

fn stage(staged: &Path, draft: &Draft) -> Result<(), String> {
    if let Some(dir) = staged.parent() {
        create_dir_all(dir).map_err(failed(dir))?;
    }
    create_dir(staged).map_err(failed(staged))?;
    private_mode(staged, 0o700)?;
    let files = draft.files.iter().map(|(path, text)| (path.as_str(), text));
    for (name, text) in [(SKILL, &draft.text)].into_iter().chain(files) {
        let path = staged.join(name);
        if let Some(dir) = path.parent() {
            create_dir_all(dir).map_err(failed(dir))?;
        }
        let mut file = std::fs::File::create_new(&path).map_err(failed(&path))?;
        private_mode(&path, 0o600)?;
        file.write_all(text.as_bytes()).and_then(|()| file.sync_all()).map_err(failed(&path))?;
    }
    Ok(())
}

#[cfg(unix)]
fn private_mode(path: &Path, mode: u32) -> Result<(), String> {
    let mode = std::os::unix::fs::PermissionsExt::from_mode(mode);
    std::fs::set_permissions(path, mode).map_err(failed(path))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn private_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

fn swap(skills: &Path, staged: &Path, target: &Path, old: &[PathBuf]) -> Result<(), String> {
    let replaced = skills.join(".replaced");
    let mut moved: Vec<(&Path, PathBuf)> = Vec::new();
    let mut step = || -> Result<(), String> {
        for from in old {
            create_dir_all(&replaced).map_err(failed(&replaced))?;
            let name = from.file_name().unwrap_or_default().to_string_lossy();
            let mut kept = (1..1000).map(|n| replaced.join(format!("{name}-{n}")));
            let to = kept.find(|to| to.symlink_metadata().is_err());
            let to = to.ok_or_else(|| format!("{}: too many old copies of {name}", replaced.display()))?;
            rename(from, &to).map_err(failed(from))?;
            moved.push((from, to));
        }
        rename(staged, target).map_err(failed(target))
    };
    let outcome = step();
    if outcome.is_err() {
        for (from, to) in moved.iter().rev() {
            rename(to, from).map_err(failed(to))?;
        }
    }
    outcome
}
