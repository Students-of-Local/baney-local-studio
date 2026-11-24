use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::Serialize;

use super::store::{Listed, Store, BESIDE, SKILL};
use super::text;
use crate::frontmatter;

const WHOLE: usize = 6000;
const FILES: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct Reading {
    pub name: String,
    pub part: Option<String>,
    pub text: String,
    pub parts: Vec<String>,
    pub files: Vec<String>,
    pub source: String,
    pub path: PathBuf,
}

impl Reading {
    #[must_use]
    pub fn shown(&self) -> String {
        let mut text = self.text.clone();
        for (label, list) in [("Parts", &self.parts), ("Files", &self.files)] {
            if !list.is_empty() {
                text = format!("{text}\n\n{label}: {}", list.join("; "));
            }
        }
        format!("{text}\n\nFolder: {}", text::plain(&self.path.with_file_name("").to_string_lossy()))
    }
}

impl Store {
    pub fn read(&mut self, name: &str, part: Option<&str>) -> Result<Reading, String> {
        let wanted = self.sound(|store| {
            store.refresh()?;
            store.named(name)
        })?;
        let Some(seen) = wanted.and_then(|id| self.seen.get(&id).cloned()) else {
            return Err(self.unknown(name));
        };
        let content =
            std::fs::read_to_string(&seen.path).map_err(|error| format!("{}: {error}", seen.path.display()))?;
        reading(seen, &frontmatter::strip(&content), part)
    }

    pub(super) fn unknown(&self, name: &str) -> String {
        let (wanted, name) = (text::key(name), text::plain(name));
        if let Some(hidden) = self.hidden.iter().find(|skill| text::key(&skill.name) == wanted) {
            return format!("{name}: {}", hidden.problem.as_deref().unwrap_or_default());
        }
        let wanted = grams(&wanted);
        let shared = |skill: &str| grams(&text::key(skill)).intersection(&wanted).count();
        let mut near: Vec<(usize, &str)> =
            self.seen.values().map(|seen| (shared(&seen.name), seen.name.as_str())).filter(|(n, _)| *n > 0).collect();
        near.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        near.dedup();
        let near: Vec<&str> = near.into_iter().take(3).map(|(_, name)| name).collect();
        if near.is_empty() {
            format!("no skill named {name}")
        } else {
            format!("no skill named {name}; near it: {}", near.join(", "))
        }
    }
}

fn grams(key: &str) -> BTreeSet<Vec<char>> {
    let chars: Vec<char> = key.chars().collect();
    chars.windows(chars.len().clamp(1, 3)).map(<[char]>::to_vec).collect()
}

fn start(sections: &[text::Part], path: &Path) -> String {
    let heads = sections.iter().filter(|part| part.head.is_empty()).map(|part| part.text.trim().to_string());
    let first = sections.iter().find(|part| !part.head.is_empty()).map(show);
    let mut text = heads.chain(first).collect::<Vec<_>>().join("\n\n");
    if let Some((end, _)) = text.char_indices().nth(WHOLE) {
        text.truncate(end);
    }
    format!("{text}\n\n[Shortened. The rest is in its other parts and in {}]", path.display())
}

fn show(part: &text::Part) -> String {
    format!("## {}\n{}", part.head, part.text.trim_end())
}

fn named<'a>(sections: &'a [text::Part], wanted: &str) -> Vec<&'a text::Part> {
    let headed = || sections.iter().filter(|part| !part.head.is_empty());
    let exact: Vec<&text::Part> =
        headed().filter(|part| part.head.to_lowercase() == wanted.trim().to_lowercase()).collect();
    let key = text::key(wanted);
    if exact.is_empty() && !key.is_empty() {
        headed().filter(|part| text::key(&part.head) == key).collect()
    } else {
        exact
    }
}

fn reading(seen: Listed, body: &str, part: Option<&str>) -> Result<Reading, String> {
    let sections = text::sections(body);
    let parts: Vec<String> =
        sections.iter().filter(|part| !part.head.is_empty()).map(|part| part.head.clone()).collect();
    let (part, text) = match part {
        Some(wanted) => {
            let found = named(&sections, wanted);
            let Some(first) = found.first() else {
                let wanted = text::plain(wanted);
                return Err(format!("{} has no part named {wanted}; its parts: {}", seen.name, parts.join(", ")));
            };
            (Some(first.head.clone()), found.iter().map(|part| show(part)).collect::<Vec<_>>().join("\n\n"))
        }
        None if body.chars().count() <= WHOLE => (None, body.trim().to_string()),
        None => (None, start(&sections, &seen.path)),
    };
    let text = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    let files = seen.path.parent().map(beside).unwrap_or_default();
    Ok(Reading { name: seen.name, part, text, parts, files, source: seen.source, path: seen.path })
}

fn beside(dir: &Path) -> Vec<String> {
    let walk = WalkBuilder::new(dir).standard_filters(false).hidden(true).max_depth(Some(3)).build();
    let mut files: Vec<String> = walk
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| Some(text::plain(&entry.path().strip_prefix(dir).ok()?.to_string_lossy())))
        .filter(|file| file != SKILL && !BESIDE.contains(&file.as_str()))
        .collect();
    files.sort();
    let more = files.len().saturating_sub(FILES);
    if more > 0 {
        files.truncate(FILES);
        files.push(format!("and {more} more"));
    }
    files
}
