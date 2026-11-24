use std::collections::BTreeMap;

const PART: usize = 1800;
const WORDS: usize = 64;

const STOP: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "another", "any", "are", "as", "at", "be", "been",
    "before", "being", "but", "by", "can", "could", "did", "do", "does", "done", "for", "from", "get", "got", "had",
    "has", "have", "he", "her", "here", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "let",
    "make", "may", "me", "might", "mine", "more", "most", "much", "must", "my", "need", "no", "nor", "not", "of", "on",
    "only", "or", "other", "our", "ours", "over", "own", "please", "same", "shall", "she", "should", "so", "some",
    "such", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "to", "too",
    "under", "us", "use", "used", "using", "very", "want", "was", "we", "were", "what", "when", "where", "which",
    "while", "who", "whom", "whose", "why", "will", "with", "would", "you", "your", "yours",
];

pub(super) fn key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

pub(super) fn line(text: &str) -> String {
    let words = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|word| !word.is_empty());
    words.collect::<Vec<_>>().join(" ")
}

pub(super) fn cut(text: &str, most: usize) -> String {
    let end = text.floor_char_boundary(most);
    let end = if end < text.len() { text[..end].rfind(' ').unwrap_or(end) } else { end };
    if end < text.len() {
        format!("{}...", &text[..end])
    } else {
        text.to_string()
    }
}

pub(super) fn words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()) {
        let word = word.to_lowercase();
        if STOP.binary_search(&word.as_str()).is_err() && !words.contains(&word) {
            words.push(word);
        }
        if words.len() == WORDS {
            break;
        }
    }
    words
}

pub(super) fn unspaced(c: char) -> bool {
    matches!(c, '\u{2e80}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..)
}

pub(super) fn matching(words: &[String]) -> String {
    words.iter().map(|word| format!("\"{word}\"")).collect::<Vec<_>>().join(" OR ")
}

pub(super) struct Part {
    pub(super) head: String,
    pub(super) text: String,
}

pub(super) fn sections(body: &str) -> Vec<Part> {
    let mut parts = vec![Part { head: String::new(), text: String::new() }];
    let mut fence: Option<&str> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        match fence {
            Some(open) if trimmed.starts_with(open) => fence = None,
            Some(_) => {}
            None => {
                fence = ["```", "~~~"].into_iter().find(|mark| trimmed.starts_with(mark));
                if let Some(head) = line.strip_prefix("## ") {
                    parts.push(Part { head: self::line(head), text: String::new() });
                    continue;
                }
            }
        }
        if let Some(part) = parts.last_mut() {
            part.text.push_str(line);
            part.text.push('\n');
        }
    }
    parts.retain(|part| !part.head.is_empty() || !part.text.trim().is_empty());
    parts
}

pub(super) fn pieces(text: &str) -> Vec<&str> {
    let mut rest = text.trim();
    let mut pieces = Vec::new();
    while rest.len() > PART {
        let end = rest.floor_char_boundary(PART);
        let cut = rest[..end].rfind('\n').filter(|cut| *cut > PART / 3).unwrap_or(end);
        let (piece, after) = rest.split_at(cut);
        pieces.push(piece);
        rest = after.trim_start();
    }
    pieces.push(rest);
    pieces
}

pub(super) fn place(text: &str) -> Option<String> {
    let text = text.trim();
    let host = match text.split_once("://") {
        Some((_, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let host = authority.rsplit('@').next().unwrap_or_default();
            host.split(':').next().unwrap_or_default()
        }
        None => text.split('/').next().unwrap_or_default(),
    };
    let host = host.trim_end_matches('.').to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    (!host.is_empty()).then_some(host)
}

pub(super) fn listed_places(sections: &[Part], listed: Option<&str>) -> Vec<String> {
    let lines: Vec<&str> = match listed {
        Some(text) => text.lines().collect(),
        None => sections
            .iter()
            .filter(|part| part.head.eq_ignore_ascii_case("sites"))
            .flat_map(|part| part.text.lines())
            .filter_map(|line| line.trim().strip_prefix("- "))
            .collect(),
    };
    lines.into_iter().filter_map(|line| place(line.trim_matches(['`', ' ']))).collect()
}

pub(super) fn linked_places(body: &str) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (index, _) in body.match_indices("://") {
        let rest = &body[index + 3..];
        let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '-')).unwrap_or(rest.len());
        let host = rest[..end].to_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host).trim_end_matches('.');
        let example = host.split('.').any(|label| matches!(label, "example" | "localhost")) || host.starts_with("127.");
        if host.contains('.') && !example {
            *counts.entry(host.to_string()).or_default() += 1;
        }
    }
    counts.into_iter().filter(|(_, count)| *count > 1).map(|(host, _)| host).collect()
}

pub(super) fn at(place: &str, wanted: &str) -> bool {
    place == wanted || (place.contains('.') && wanted.strip_suffix(place).is_some_and(|rest| rest.ends_with('.')))
}

#[must_use]
pub fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}
