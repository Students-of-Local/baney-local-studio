use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use super::store::{Listed, Store};
use super::text;

const SHOWN: usize = 240;
const SHOWN_LIST: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub name: String,
    pub description: String,
    pub parts: Vec<String>,
    pub source: String,
    pub places: Vec<String>,
    pub chars: u32,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Query<'a> {
    pub text: &'a str,
    pub place: Option<&'a str>,
    pub within: Option<&'a str>,
    pub limit: usize,
}

#[must_use]
pub fn lines(hits: &[Hit]) -> String {
    let line = |hit: &Hit| {
        let mut line = format!("{} ({})", hit.name, hit.source);
        if !hit.description.is_empty() {
            line = format!("{line}: {}", text::cut(&hit.description, SHOWN));
        }
        for (label, list) in [("parts", &hit.parts), ("for", &hit.places)] {
            if !list.is_empty() {
                line = format!(
                    "{line} | {label}: {}",
                    list.iter().take(SHOWN_LIST).cloned().collect::<Vec<_>>().join("; ")
                );
            }
        }
        line
    };
    hits.iter().map(line).collect::<Vec<_>>().join("\n")
}

impl Store {
    pub fn search(&mut self, query: &Query) -> Result<Vec<Hit>, String> {
        self.sound(|store| {
            store.refresh()?;
            store.find(query)
        })
    }

    fn find(&self, query: &Query) -> rusqlite::Result<Vec<Hit>> {
        let words = text::words(query.text);
        let mut scores: HashMap<i64, f64> = HashMap::new();
        let mut parts: HashMap<i64, Vec<(f64, String)>> = HashMap::new();
        if !words.is_empty() {
            let matching = text::matching(&words);
            let mut rows =
                self.db.prepare_cached("SELECT rowid, -bm25(whole, 3, 2, 2, 1) FROM whole WHERE whole MATCH ?1")?;
            let mut rows = rows.query([&matching])?;
            while let Some(row) = rows.next()? {
                scores.insert(row.get(0)?, row.get(1)?);
            }
            let mut rows = self.db.prepare_cached(
                "SELECT rowid >> 20, head, -bm25(parts, 1, 2, 1) FROM parts WHERE parts MATCH ?1 ORDER BY 3 DESC",
            )?;
            let mut rows = rows.query([&matching])?;
            while let Some(row) = rows.next()? {
                parts.entry(row.get(0)?).or_default().push((row.get(2)?, row.get(1)?));
            }
        }
        for word in words.iter().filter(|word| word.chars().any(text::unspaced)) {
            let mut rows = self.db.prepare_cached("SELECT id FROM skills WHERE instr(name || ' ' || descr, ?1)")?;
            for id in rows.query_map([word], |row| row.get(0))? {
                *scores.entry(id?).or_default() += 1.0;
            }
        }
        for (id, ranked) in &parts {
            let more: f64 = ranked.iter().skip(1).take(2).map(|(score, _)| score).sum();
            *scores.entry(*id).or_default() += ranked.first().map_or(0.0, |(score, _)| *score) + 0.3 * more;
        }
        if let Some(wanted) = query.place.and_then(text::place) {
            let top = scores.values().copied().fold(1.0, f64::max);
            let mut rows = self.db.prepare_cached("SELECT skill, place, listed FROM places")?;
            let mut rows = rows.query([])?;
            let mut here: HashMap<i64, f64> = HashMap::new();
            while let Some(row) = rows.next()? {
                if text::at(&row.get::<_, String>(1)?, &wanted) {
                    let share = if row.get(2)? { 1.0 } else { 0.5 };
                    let best = here.entry(row.get(0)?).or_default();
                    *best = best.max(share);
                }
            }
            for (id, share) in here {
                *scores.entry(id).or_default() += top * share;
            }
        }
        if let Some(id) = self.named(query.text)? {
            *scores.entry(id).or_default() += 1000.0;
        }
        let within = query.within.map(|name| self.named(name)).transpose()?;
        let mut ranked: Vec<(f64, i64)> = scores
            .into_iter()
            .filter(|(id, _)| within.map_or_else(|| self.seen.contains_key(id), |within| within == Some(*id)))
            .map(|(id, score)| (score, id))
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| self.seen[&a.1].name.cmp(&self.seen[&b.1].name)));
        let heads = if within.is_some() { query.limit } else { 2 };
        ranked.into_iter().take(query.limit).map(|(_, id)| self.hit(id, parts.remove(&id), heads)).collect()
    }

    fn hit(&self, id: i64, ranked: Option<Vec<(f64, String)>>, heads: usize) -> rusqlite::Result<Hit> {
        let chars = self.db.query_row("SELECT chars FROM skills WHERE id = ?1", [id], |row| row.get(0))?;
        let mut rows = self.db.prepare_cached("SELECT place FROM places WHERE skill = ?1 AND listed ORDER BY place")?;
        let places = rows.query_map([id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut parts: Vec<String> = Vec::new();
        for (_, head) in ranked.unwrap_or_default() {
            if !head.is_empty() && !parts.contains(&head) {
                parts.push(head);
            }
        }
        parts.truncate(heads);
        let Listed { name, description, source, path, .. } = self.seen[&id].clone();
        Ok(Hit { name, description, parts, source, places, chars, path })
    }
}
