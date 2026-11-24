use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::Serialize;

use super::store::{Listed, Store, BESIDE, SKILL};
use super::text;
use crate::frontmatter;

const WHOLE: usize = 6000;

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
        let (words, name) = (text::words(name), text::plain(name));
        let mut near: Vec<(usize, &str)> = self
            .seen
            .values()
            .map(|seen| (words.iter().filter(|word| seen.name.contains(word.as_str())).count(), seen.name.as_str()))
            .filter(|(shared, _)| *shared > 0)
            .collect();
        near.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let near: Vec<&str> = near.into_iter().take(3).map(|(_, name)| name).collect();
        if near.is_empty() {
            format!("no skill named {name}")
        } else {
            format!("no skill named {name}; near it: {}", near.join(", "))
        }
    }
}

fn reading(seen: Listed, body: &str, part: Option<&str>) -> Result<Reading, String> {
    let sections = text::sections(body);
    let parts: Vec<String> =
        sections.iter().filter(|part| !part.head.is_empty()).map(|part| part.head.clone()).collect();
    let show = |part: &text::Part| format!("## {}\n{}", part.head, part.text.trim_end());
    let (part, text) = match part {
        Some(wanted) => {
            let found: Vec<&text::Part> =
                sections.iter().filter(|part| text::key(&part.head) == text::key(wanted)).collect();
            let Some(first) = found.first() else {
                return Err(format!("{} has no part named {wanted}; its parts: {}", seen.name, parts.join(", ")));
            };
            (Some(first.head.clone()), found.iter().map(|part| show(part)).collect::<Vec<_>>().join("\n\n"))
        }
        None if body.len() <= WHOLE => (None, body.trim().to_string()),
        None => {
            let intro = sections.iter().find(|part| part.head.is_empty()).map(|part| part.text.trim().to_string());
            let first = sections.iter().find(|part| !part.head.is_empty()).map(show);
            (None, intro.into_iter().chain(first).collect::<Vec<_>>().join("\n\n"))
        }
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
        .take(50)
        .collect();
    files.sort();
    files
}
