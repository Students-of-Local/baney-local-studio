use std::fs::{create_dir, create_dir_all, read_to_string, remove_dir_all, rename};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use super::store::{Store, BESIDE, SKILL};
use super::text;
use crate::frontmatter;
use crate::SkillFile;

const MOST_TEXT: usize = 64 * 1024;
const MOST_FILES: usize = 20;
const MOST_ALL: usize = 256 * 1024;
const FOLDER: usize = 60;
const LONG: usize = 24;
const NAMED: [&str; 9] = ["password", "passwd", "secret", "token", "cookie", "api key", "api_key", "api-key", "apikey"];
const KINDS: [&str; 4] = ["skill", "asks", "where", "file"];
const QUOTES: [char; 5] = ['"', '\'', '`', '*', ' '];
static MADE: AtomicU64 = AtomicU64::new(0);
const NAMES: &str = "names.txt";
const ASKS: &str = "asks.txt";

pub const MAKE: &str = "@create";
pub const CREATE: &str = "Turn what you did in this thread into one skill that a small local model can follow without this thread.
Reply with one block opened and closed by four backticks, or one more than the longest run of backticks inside it:

````skill
---
name: a short name
description: what it does and when to use it, in one line
---
What it is for. Then numbered steps under `##` headings: exact addresses, API calls with method and body, form fields
by their label (a selector only as a fallback), how to see that each step worked, what went wrong on the way and what
fixed it, and when to stop and ask the person.
````

Then, each in its own block opened and closed the same way: `asks` (five or more ways a person would ask for it, one a
line), `where` (the sites or apps it is for, one host or app name a line) and `file scripts/name.py` for a script it
needs. Write only what you saw work. No passwords, tokens, cookies, account numbers or other people's details: use <placeholders> the person fills in.";

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub name: String,
    pub description: String,
    pub folder: String,
    pub text: String,
    pub files: Vec<(String, String)>,
    pub marked: Vec<String>,
    pub left: Vec<String>,
}

#[derive(Debug)]
pub struct Saved {
    pub path: PathBuf,
    pub kept: Vec<PathBuf>,
    pub hides: Vec<PathBuf>,
}

#[must_use]
pub fn asked(text: &str) -> bool {
    let rest = text.trim_start().strip_prefix(MAKE);
    rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

impl Draft {
    pub fn parse(reply: &str) -> Result<Self, String> {
        let blocks = blocks(reply)?;
        let mut skills = blocks.iter().filter(|block| block.info == "skill");
        let (Some(skill), None) = (skills.next(), skills.next()) else {
            return Err("The reply does not hold one skill block. Ask for it again.".to_string());
        };
        if skill.long < 4 {
            return Err("The skill block needs four backticks, so a code block inside it does not end it.".to_string());
        }
        let (mut files, mut left): (Vec<(String, String)>, Vec<String>) = Default::default();
        for block in &blocks {
            let (path, text) = match block.info.split_once(' ').unwrap_or((&block.info, "")) {
                ("skill", "") => continue,
                ("asks", "") => (ASKS.to_string(), listed(&block.text)),
                ("where", "") => ("where.txt".to_string(), listed(&block.text).lines().map(place).collect()),
                ("file", path) if block.long > 3 => (beside(path)?, block.text.clone()),
                ("file", path) => return Err(format!("The file {} needs four backticks.", text::plain(path))),
                ("", "") if block.long > 3 => {
                    return Err("A block in four backticks has no label, so one may have ended early.".to_string());
                }
                _ => {
                    left.push(other(&block.info)?);
                    continue;
                }
            };
            if files.iter().any(|(used, _)| used.eq_ignore_ascii_case(&path)) {
                return Err(format!("The reply holds {path} twice."));
            }
            if !text.is_empty() || block.info.starts_with("file") {
                files.push((path, text));
            }
        }
        Self::of(skill.text.clone(), files, left)
    }

    pub(crate) fn trial(self) -> (String, String) {
        let asks = self.files.iter().find(|(path, _)| path == ASKS).and_then(|(_, text)| text.lines().next());
        let ask = asks.unwrap_or_default().to_string();
        (self.name, ask)
    }

    pub fn with_text(self, text: String) -> Result<Self, String> {
        Self::of(text, self.files, self.left)
    }

    fn of(text: String, files: Vec<(String, String)>, left: Vec<String>) -> Result<Self, String> {
        let field = |key: &str| frontmatter::field(&text, key).map(|value| text::line(&value));
        let name = field("name").unwrap_or_default();
        let description = field("description").unwrap_or_default();
        let folder = folder(&name);
        if folder.is_empty() || description.is_empty() {
            return Err("The skill needs a name and a description in its front matter.".to_string());
        }
        let texts = || files.iter().map(|(_, text)| text).chain([&text]);
        if texts().any(|text| text.len() > MOST_TEXT) || texts().map(String::len).sum::<usize>() > MOST_ALL {
            return Err(format!("A skill is at most {} KB, {} KB a file.", MOST_ALL / 1024, MOST_TEXT / 1024));
        }
        if files.len() > MOST_FILES {
            return Err(format!("A skill has at most {MOST_FILES} files."));
        }
        let marked = marks(texts());
        Ok(Self { name, description, folder, text, files, marked, left })
    }

    pub(crate) fn replaces(&self, data: &Path) -> bool {
        let same = |entry: std::fs::DirEntry| {
            let folder = entry.file_name().to_string_lossy().into_owned();
            let Ok(content) = read_to_string(entry.path().join(SKILL)) else { return false };
            let name = frontmatter::field(&content, "name").unwrap_or_else(|| folder.clone());
            !folder.starts_with('.') && text::line(&name).to_lowercase() == self.name.to_lowercase()
        };
        std::fs::read_dir(data.join("skills")).into_iter().flatten().filter_map(Result::ok).any(same)
    }
}

impl Store {
    pub fn save(&mut self, draft: &Draft) -> Result<Saved, String> {
        if draft.name.to_lowercase().split_whitespace().next() == Some("create") {
            return Err("A skill's name can't start with create. Give it another name.".to_string());
        }
        let skills = self.own()?;
        self.checked = None;
        let (mut old, mut hides) = (Vec::new(), Vec::new());
        for skill in self.list()? {
            let read = !skill.description.is_empty();
            if !read || skill.name.to_lowercase() != draft.name.to_lowercase() {
                continue;
            }
            if skill.source != "yours" {
                hides.extend(skill.problem.is_none().then_some(skill.path));
                continue;
            }
            let Some(folder) = skill.path.parent() else { continue };
            if folder.parent() != Some(skills.as_path()) {
                return Err(format!("{} already has this name. Move or rename it first.", skill.path.display()));
            }
            if folder.is_symlink() {
                return Err(format!("{} is a link. Remove it first.", folder.display()));
            }
            old.push(folder.to_path_buf());
        }
        let same = |path: &Path| path.canonicalize().is_ok_and(|real| old.contains(&real));
        let mut names = (1..1000).map(|n| if n == 1 { draft.folder.clone() } else { format!("{}-{n}", draft.folder) });
        let free = names.find_map(|name| {
            let target = skills.join(name);
            (target.symlink_metadata().is_err() || same(&target)).then_some(target)
        });
        let target = free.ok_or_else(|| format!("{}: too many skills named {}", skills.display(), draft.folder))?;
        let mut files = draft.files.clone();
        let names = old.iter().map(|old| old.join(NAMES)).find(|path| path.is_file() && !path.is_symlink());
        if let Some(path) = names {
            files.push((NAMES.to_string(), read_to_string(&path).map_err(failed(&path))?));
        }
        let made = MADE.fetch_add(1, Ordering::Relaxed);
        let staged = skills.join(format!(".making-{}-{made}", std::process::id()));
        let _ = remove_dir_all(&staged);
        let outcome = stage(&staged, &draft.text, &files).and_then(|()| swap(&skills, &staged, &target, &old));
        if outcome.is_err() {
            let _ = remove_dir_all(&staged);
        }
        self.checked = None;
        outcome.map(|kept| Saved { path: target, kept, hides })
    }
}

struct Block {
    info: String,
    long: usize,
    text: String,
}

fn blocks(reply: &str) -> Result<Vec<Block>, String> {
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
                if let Some((mark, long)) = fence.filter(|(mark, long)| *mark == '~' || !trimmed[*long..].contains('`'))
                {
                    let info = text::line(&trimmed[long..]);
                    open = Some((mark, Block { info, long, text: String::new() }));
                }
            }
        }
    }
    match open {
        Some((_, block)) => {
            Err(format!("The {} block never ends, so the reply may be cut short.", text::plain(&block.info)))
        }
        None => Ok(blocks),
    }
}

fn other(info: &str) -> Result<String, String> {
    let info = text::plain(info);
    let first = info.to_lowercase();
    if first.split(|c: char| !c.is_alphanumeric()).next().is_some_and(|word| KINDS.contains(&word)) {
        return Err(format!(
            "The reply holds a block labeled \"{info}\". It can be skill, asks, where or file and a path."
        ));
    }
    Ok(info)
}

fn listed(text: &str) -> String {
    let item = |line: &str| {
        let line = line.trim();
        let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
        let rest = if rest == line { line.strip_prefix(['-', '*', '+']) } else { rest.strip_prefix(['.', ')']) };
        let line = rest.filter(|rest| rest.is_empty() || rest.starts_with(' ')).unwrap_or(line);
        let line = line.trim_matches([' ', '`']);
        (!line.is_empty()).then(|| format!("{line}\n"))
    };
    text.lines().filter_map(item).collect()
}

fn place(line: &str) -> String {
    let ends = ["`", " (", ": ", " - "];
    format!("{}\n", ends.iter().fold(line, |line, end| line.split(end).next().unwrap_or(line)))
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

fn marks<'a>(texts: impl Iterator<Item = &'a String>) -> Vec<String> {
    texts.flat_map(|text| text.lines()).filter(|line| private(line)).map(text::line).collect()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn skill_marks(text: &str, files: &[SkillFile]) -> Vec<String> {
    marks(files.iter().map(|file| &file.text).chain([&text.to_string()]))
}

fn private(line: &str) -> bool {
    let lower = line.to_lowercase();
    let filled = |value: &str| {
        let value = value.trim_start_matches(QUOTES);
        value.split_whitespace().next().is_some_and(|word| word.len() > 5 && !word.starts_with(['<', '{', '$', '[']))
    };
    let named = NAMED.iter().any(|word| {
        lower.match_indices(word).any(|(at, _)| {
            let after =
                lower[at + word.len()..].trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
            let flag = lower[..at].rsplit(char::is_whitespace).next().is_some_and(|word| word.starts_with("--"));
            after
                .trim_start_matches(QUOTES)
                .strip_prefix([':', '='])
                .or_else(|| after.strip_prefix(' ').filter(|_| flag))
                .is_some_and(filled)
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

fn stage(staged: &Path, text: &String, files: &[(String, String)]) -> Result<(), String> {
    if let Some(dir) = staged.parent() {
        create_dir_all(dir).map_err(failed(dir))?;
    }
    create_dir(staged).map_err(failed(staged))?;
    private_mode(staged, 0o700)?;
    let files = files.iter().map(|(path, text)| (path.as_str(), text));
    for (name, text) in [(SKILL, text)].into_iter().chain(files) {
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

fn swap(skills: &Path, staged: &Path, target: &Path, old: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
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
    match step() {
        Ok(()) => Ok(moved.into_iter().map(|(_, to)| to).collect()),
        Err(mut error) => {
            for (from, to) in moved.iter().rev() {
                if let Err(stuck) = rename(to, from) {
                    error = format!("{error}; {} is now {} ({stuck})", from.display(), to.display());
                }
            }
            Err(error)
        }
    }
}
