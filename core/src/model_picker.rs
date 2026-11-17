use std::collections::{HashMap, HashSet};

use crate::model::{ModelKey, ModelOption};
use crate::workspace::model::ProviderStatus;

const FAVORITES: &str = "favorites";
const HIDDEN: &str = "hidden";
const JUMPS: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelEntry {
    pub model: ModelOption,
    pub favorite: bool,
    pub hidden: bool,
    pub jump: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelSection {
    pub id: String,
    pub title: String,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelSections {
    pub sections: Vec<ModelSection>,
    pub jumps: Vec<ModelKey>,
    pub providers: Vec<String>,
    pub configured: u32,
    pub hidden: u32,
    pub empty: Option<String>,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn model_sections(
    models: Vec<ModelOption>,
    favorites: Vec<ModelKey>,
    hidden: Vec<ModelKey>,
    providers: Vec<ProviderStatus>,
    query: String,
    show_hidden: bool,
    loaded: bool,
) -> ModelSections {
    let has = |keys: &[ModelKey], model: &ModelOption| {
        keys.iter().any(|key| key.provider == model.provider && key.model == model.id)
    };
    let configured: Vec<&ModelOption> = models.iter().filter(|model| model.configured).collect();
    let hidden_count = configured.iter().filter(|model| has(&hidden, model)).count();
    let needle = query.trim().to_lowercase();
    let matching: Vec<&ModelOption> = configured
        .iter()
        .copied()
        .filter(|model| {
            needle.is_empty()
                || [&model.name, &model.id, &model.provider].iter().any(|text| text.to_lowercase().contains(&needle))
        })
        .collect();
    let visible: Vec<&ModelOption> = matching.iter().copied().filter(|model| !has(&hidden, model)).collect();
    let name = |provider: &str| {
        providers
            .iter()
            .find(|status| status.id == provider)
            .map_or_else(|| capitalized(provider), |status| status.name.clone())
    };

    let mut groups: Vec<(String, String, Vec<&ModelOption>)> = Vec::new();
    let favored: Vec<&ModelOption> = visible.iter().copied().filter(|model| has(&favorites, model)).collect();
    if !favored.is_empty() {
        groups.push((FAVORITES.to_string(), "Favorites".to_string(), favored));
    }
    let mut order: Vec<&str> = Vec::new();
    let mut grouped: HashMap<&str, Vec<&ModelOption>> = HashMap::new();
    for model in &visible {
        let entry = grouped.entry(model.provider.as_str()).or_insert_with(|| {
            order.push(model.provider.as_str());
            Vec::new()
        });
        entry.push(model);
    }
    let mut titled: Vec<(&str, String)> = order.into_iter().map(|provider| (provider, name(provider))).collect();
    titled.sort_by(|left, right| left.1.cmp(&right.1));
    for (provider, title) in titled {
        groups.push((provider.to_string(), title, grouped.remove(provider).unwrap_or_default()));
    }
    let concealed: Vec<&ModelOption> = matching.iter().copied().filter(|model| has(&hidden, model)).collect();
    if show_hidden && !concealed.is_empty() {
        groups.push((HIDDEN.to_string(), "Hidden".to_string(), concealed));
    }

    let mut seen = HashSet::new();
    let jumps: Vec<ModelKey> = groups
        .iter()
        .filter(|(id, _, _)| id != HIDDEN)
        .flat_map(|(_, _, models)| models.iter())
        .map(|model| ModelKey { provider: model.provider.clone(), model: model.id.clone() })
        .filter(|key| seen.insert(key.clone()))
        .collect();
    let sections: Vec<ModelSection> = groups
        .into_iter()
        .map(|(id, title, members)| {
            let models = members
                .into_iter()
                .map(|model| {
                    let favorite = has(&favorites, model);
                    let first = id == FAVORITES || !favorite;
                    let jump = jumps
                        .iter()
                        .position(|key| key.provider == model.provider && key.model == model.id)
                        .filter(|index| first && *index < JUMPS)
                        .and_then(|index| u32::try_from(index + 1).ok());
                    ModelEntry { model: model.clone(), favorite, hidden: has(&hidden, model), jump }
                })
                .collect();
            ModelSection { id, title, models }
        })
        .collect();
    let providers = sections
        .iter()
        .filter(|section| section.id != FAVORITES && section.id != HIDDEN)
        .map(|section| section.id.clone())
        .collect();
    let empty = sections.is_empty().then(|| {
        if !loaded {
            "Loading models…".to_string()
        } else if configured.is_empty() {
            "No configured providers. Add a key or an endpoint to choose a model.".to_string()
        } else if hidden_count == configured.len() && !show_hidden {
            "Every model is hidden.".to_string()
        } else {
            format!("No models match “{query}”.")
        }
    });
    ModelSections {
        sections,
        jumps,
        providers,
        configured: u32::try_from(configured.len()).unwrap_or(u32::MAX),
        hidden: u32::try_from(hidden_count).unwrap_or(u32::MAX),
        empty,
    }
}

fn capitalized(text: &str) -> String {
    let mut start = true;
    text.chars()
        .flat_map(|character| {
            let word_start = start;
            start = character.is_whitespace();
            if word_start {
                character.to_uppercase().collect::<Vec<_>>()
            } else {
                character.to_lowercase().collect::<Vec<_>>()
            }
        })
        .collect()
}
