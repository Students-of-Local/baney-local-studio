use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::store::{Listed, Store};
use super::text;

const SHOWN: usize = 240;
const SHOWN_ITEM: usize = 80;
const SHOWN_PLACES: usize = 5;
const TRIES: usize = 4;

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
        for (label, list, most) in [("parts", &hit.parts, hit.parts.len()), ("for", &hit.places, SHOWN_PLACES)] {
            if !list.is_empty() {
                let items: Vec<_> = list.iter().take(most).map(|item| text::cut(item, SHOWN_ITEM)).collect();
                line = format!("{line} | {label}: {}", items.join("; "));
            }
        }
        line
    };
    hits.iter().map(line).collect::<Vec<_>>().join("\n")
}

impl Store {
    pub fn search(&mut self, query: &Query) -> Result<Vec<Hit>, String> {
        let found = self.settled(|store| store.find(query));
        found?.ok_or_else(|| self.unknown(query.within.unwrap_or_default()))
    }

    pub(super) fn settled<T>(
        &mut self,
        step: impl Fn(&Self) -> rusqlite::Result<Option<T>>,
    ) -> Result<Option<T>, String> {
        self.sound(|store| {
            for left in (0..TRIES).rev() {
                store.refresh()?;
                let read = store.db.unchecked_transaction()?;
                if left == 0 || store.current()? {
                    return step(store);
                }
                drop(read);
                store.checked = None;
            }
            Ok(None)
        })
    }

    fn find(&self, query: &Query) -> rusqlite::Result<Option<Vec<Hit>>> {
        let within = match query.within.map(|name| self.named(name)).transpose()? {
            Some(None) => return Ok(None),
            within => within.flatten(),
        };
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
        for piece in text::unspaced_pieces(&words) {
            let mut rows = self.db.prepare_cached(
                "SELECT id FROM skills WHERE instr(lower(name || ' ' || descr), ?1)
                 UNION SELECT rowid >> 20 FROM parts WHERE instr(lower(head || ' ' || body), ?1)",
            )?;
            for id in rows.query_map([piece], |row| row.get(0))? {
                *scores.entry(id?).or_default() += 1.0;
            }
        }
        for (id, ranked) in &parts {
            let more: f64 = ranked.iter().skip(1).take(2).map(|(score, _)| score).sum();
            *scores.entry(*id).or_default() += ranked.first().map_or(0.0, |(score, _)| *score) + 0.3 * more;
        }
        let (mut listing, mut linking) = (HashSet::new(), HashSet::new());
        let wanted = query.place.and_then(text::place);
        if let Some(wanted) = &wanted {
            let seen = scores.iter().filter(|(id, _)| self.seen.contains_key(id));
            let top = seen.map(|(_, score)| *score).fold(1.0, f64::max);
            let mut rows = self.db.prepare_cached("SELECT skill, place, listed FROM places")?;
            let mut rows = rows.query([])?;
            while let Some(row) = rows.next()? {
                if text::at(&row.get::<_, String>(1)?, wanted) {
                    let (id, own) = (row.get(0)?, row.get(2)?);
                    if own {
                        listing.insert(id);
                    } else if linking.insert(id) && (words.is_empty() || scores.contains_key(&id)) {
                        *scores.entry(id).or_default() += top * 0.5;
                    }
                }
            }
        }
        let named = self.named(query.text)?;
        for id in listing.iter().chain(&named) {
            scores.entry(*id).or_default();
        }
        let mut ranked: Vec<(bool, bool, f64, i64)> = scores
            .into_iter()
            .filter(|(id, _)| within.map_or_else(|| self.seen.contains_key(id), |within| within == *id))
            .map(|(id, score)| (named == Some(id), listing.contains(&id), score, id))
            .collect();
        ranked.sort_by(|a, b| {
            let first = (b.0, b.1).cmp(&(a.0, a.1)).then_with(|| b.2.total_cmp(&a.2));
            first.then_with(|| self.seen[&a.3].name.cmp(&self.seen[&b.3].name))
        });
        let heads = if within.is_some() { query.limit } else { 2 };
        let hits = ranked.into_iter().take(query.limit).map(|(.., id)| self.hit(id, parts.remove(&id), heads));
        let mut hits = hits.collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some(wanted) = &wanted {
            for hit in &mut hits {
                hit.places.sort_by_key(|place| !text::at(place, wanted));
            }
        }
        Ok(Some(hits))
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
