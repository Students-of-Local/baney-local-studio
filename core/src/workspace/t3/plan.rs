use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::source::{Instance, Selection, T3Data, ThemeFile};
use crate::harness::{self, accounts, CLAUDE_PROVIDER, CODEX_PROVIDER};
use crate::model::{Harness, ModelKey};
use crate::store::StoredProject;
use crate::workspace::model::{
    AccountLabel, ProjectSettings, Settings, T3Choices, T3Group, T3Item, T3ItemState, T3Kind,
};
use crate::workspace::settings;

pub(crate) const SOURCE: &str = "T3 Code";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SyncState {
    pub(crate) enabled: bool,
    pub(crate) offered: bool,
    pub(crate) last_synced_at: Option<i64>,
    pub(crate) choices: T3Choices,
    pub(crate) imported: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ls {
    pub(crate) settings: Settings,
    pub(crate) keybindings: BTreeMap<String, Option<String>>,
    pub(crate) projects: Vec<StoredProject>,
    pub(crate) favorites: Vec<ModelKey>,
    pub(crate) hidden: Vec<ModelKey>,
    pub(crate) project_settings: BTreeMap<String, ProjectSettings>,
}

const APPEARANCE: [(&str, &str, &str); 14] = [
    ("contrast", "appearanceContrast", "Contrast"),
    ("glassOpacity", "glassOpacity", "Glass opacity"),
    ("chatWidth", "chatWidth", "Chat width"),
    ("diffColors", "diffColorScheme", "Diff colors"),
    ("interfaceFont", "fontFamilySans", "Interface font"),
    ("interfaceFontSize", "fontSizeInterface", "Interface font size"),
    ("promptFont", "fontFamilyComposer", "Prompt font"),
    ("promptFontSize", "fontSizePrompt", "Prompt font size"),
    ("codeFont", "fontFamilyCode", "Code font"),
    ("codeFontSize", "fontSizeCode", "Code font size"),
    ("terminalFont", "fontFamilyTerminal", "Terminal font"),
    ("terminalFontSize", "fontSizeTerminal", "Terminal font size"),
    ("fontSmoothing", "fontSmoothing", "Font smoothing"),
    ("wordWrap", "wordWrap", "Word wrap"),
];

const PREFERENCES: [(&str, &str, &str); 22] = [
    ("diffLayout", "diffLayout", "Diff layout"),
    ("diffFilesCollapsed", "diffFilesCollapsed", "Collapse diff files"),
    ("hideWhitespace", "diffIgnoreWhitespace", "Hide whitespace changes"),
    ("timeFormat", "timestampFormat", "Time format"),
    ("sendShortcut", "sendShortcut", "Send shortcut"),
    ("followUpBehavior", "followUpBehavior", "Follow-up behavior"),
    ("richTextComposer", "composerRichTextEnabled", "Rich text composer"),
    ("showSkillsInSlashMenu", "showSkillsInSlashMenu", "Skills in the slash menu"),
    ("proactivePanels", "proactivePanelsEnabled", "Proactive panels"),
    ("contextWindowMeter", "contextWindowMeterEnabled", "Context window meter"),
    ("quitConfirmation", "confirmQuit", "Quit confirmation"),
    ("confirmArchive", "confirmThreadArchive", "Confirm archive"),
    ("confirmDelete", "confirmThreadDelete", "Confirm delete"),
    ("confirmUnpin", "confirmThreadUnpin", "Confirm unpin"),
    ("notifications", "notificationMode", "Notifications"),
    ("inAppNotifications", "inAppNotificationsEnabled", "In-app notifications"),
    ("openLinksIn", "browserLinkTarget", "Open links in"),
    ("snapshotEnabled", "snapShotEnabled", "SnapShot"),
    ("snapshotPlaySound", "snapShotPlaySound", "SnapShot sound"),
    ("snapshotSound", "snapShotSound", "SnapShot sound choice"),
    ("snapshotFlash", "snapShotFlash", "SnapShot flash"),
    ("snapshotAnimations", "snapShotAnimations", "SnapShot animations"),
];

fn chosen(choices: T3Choices, kind: T3Kind) -> bool {
    match kind {
        T3Kind::Accounts => choices.accounts,
        T3Kind::Projects => choices.projects,
        T3Kind::Models => choices.models,
        T3Kind::Keybindings => choices.keybindings,
        T3Kind::Appearance => choices.appearance,
        T3Kind::Preferences => choices.preferences,
    }
}

fn title(kind: T3Kind) -> &'static str {
    match kind {
        T3Kind::Accounts => "Sign-ins",
        T3Kind::Projects => "Projects",
        T3Kind::Models => "Models",
        T3Kind::Keybindings => "Keyboard shortcuts",
        T3Kind::Appearance => "Appearance",
        T3Kind::Preferences => "Preferences",
    }
}

pub(crate) struct Outcome {
    pub(crate) groups: Vec<T3Group>,
    pub(crate) problems: Vec<String>,
}

struct Engine<'a> {
    imported: &'a mut BTreeMap<String, Value>,
    choices: T3Choices,
    apply: bool,
    groups: BTreeMap<T3Kind, Vec<T3Item>>,
}

impl Engine<'_> {
    fn live(&self, kind: T3Kind) -> bool {
        self.apply && chosen(self.choices, kind)
    }

    fn scalar(&mut self, kind: T3Kind, key: &str, desired: &Value, current: &Value, default: &Value) -> T3ItemState {
        let prior = self.imported.get(key).cloned();
        let state = if current == desired {
            T3ItemState::Synced
        } else if prior.as_ref().map_or(current == default, |prior| prior == current) {
            T3ItemState::Import
        } else {
            T3ItemState::Kept
        };
        if self.live(kind) && state != T3ItemState::Kept {
            self.imported.insert(key.to_string(), desired.clone());
        }
        state
    }

    fn member(&mut self, kind: T3Kind, key: &str, present: bool) -> T3ItemState {
        let state = if present {
            T3ItemState::Synced
        } else if self.imported.contains_key(key) {
            T3ItemState::Kept
        } else {
            T3ItemState::Import
        };
        if self.live(kind) && state != T3ItemState::Kept {
            self.imported.insert(key.to_string(), Value::Bool(true));
        }
        state
    }

    fn item(&mut self, kind: T3Kind, key: String, label: String, detail: String, state: T3ItemState) {
        self.groups.entry(kind).or_default().push(T3Item { key, label, detail, state });
    }
}

fn worst(states: &[T3ItemState]) -> T3ItemState {
    if states.contains(&T3ItemState::Import) {
        T3ItemState::Import
    } else if states.contains(&T3ItemState::Kept) {
        T3ItemState::Kept
    } else {
        T3ItemState::Synced
    }
}

fn harness_of(driver: &str) -> Option<Harness> {
    match driver {
        "claudeAgent" => Some(Harness::ClaudeCode),
        "codex" => Some(Harness::Codex),
        _ => None,
    }
}

fn unsupported_reason(driver: &str) -> String {
    let name = match driver {
        "cursor" => "Cursor Agent",
        "grok" => "Grok CLI",
        "opencode" => "OpenCode",
        "acpRegistry" => "ACP agent",
        "antigravity" => "Antigravity",
        "" => "this",
        other => other,
    };
    format!("Not supported yet: Local Studio has no {name} harness. Claude Code and Codex are supported.")
}

fn driver_name(driver: &str) -> &str {
    match driver {
        "claudeAgent" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "opencode" => "OpenCode",
        "acpRegistry" => "ACP agent",
        "antigravity" => "Antigravity",
        other => other,
    }
}

fn display(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    let home = crate::platform::home().map(|home| home.to_string_lossy().into_owned());
    match home {
        Some(home) if !home.is_empty() => match text.strip_prefix(home.as_str()) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => text,
        },
        _ => text,
    }
}

fn account_dir(instance: &Instance, harness: Harness) -> Option<PathBuf> {
    match instance.home_path.as_deref() {
        Some(path) => Some(accounts::expand(path)),
        None => accounts::home_dir_of(harness),
    }
}

fn kebab_to_camel(text: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for character in text.chars() {
        if character == '-' || character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

fn convert(ls_key: &str, value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            let mapped = match (ls_key, text) {
                ("chatWidth", "comfortable") => "normal".to_string(),
                ("timeFormat", "12-hour") => "twelveHour".to_string(),
                ("timeFormat", "24-hour") => "twentyFourHour".to_string(),
                ("colorScheme", "") => return None,
                (key, text) if key.ends_with("Font") || key == "addProjectBaseDirectory" => text.to_string(),
                (_, text) => kebab_to_camel(text),
            };
            Some(Value::String(mapped))
        }
        Value::Number(_) | Value::Bool(_) => Some(value.clone()),
        _ => None,
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) if text.is_empty() => "System default".to_string(),
        Value::String(text) => text.clone(),
        Value::Bool(true) => "On".to_string(),
        Value::Bool(false) => "Off".to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

fn settings_map(settings: &Settings) -> Map<String, Value> {
    match serde_json::to_value(settings) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn custom_themes(settings: &Settings) -> Vec<Value> {
    serde_json::from_str::<Value>(&settings.custom_themes)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

fn trim_color(value: &str) -> String {
    let value = value.trim();
    match value.get(..7) {
        Some(color) if value.len() == 9 && value.starts_with('#') => color.to_string(),
        _ => value.to_string(),
    }
}

fn theme_entry(id: &str, file: &ThemeFile) -> Value {
    let name = file.name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| id.to_string());
    let appearance = match file.appearance.as_deref() {
        Some("light") => "light",
        _ => "dark",
    };
    let pick = |keys: &[&str], fallback: &str| {
        keys.iter()
            .find_map(|key| file.colors.get(*key))
            .map_or_else(|| fallback.to_string(), |value| trim_color(value))
    };
    let canvas = pick(&["canvas", "surface"], if appearance == "light" { "#ffffff" } else { "#0a0a0a" });
    let mut entry = json!({
        "id": id,
        "label": name.chars().take(48).collect::<String>(),
        "appearance": appearance,
        "colors": file.colors,
        "name": name,
        "dark": appearance == "dark",
        "sidebar": pick(&["sidebar", "chrome", "canvas"], &canvas),
        "card": pick(&["surfaceRaised", "surface", "canvas"], &canvas),
        "text": pick(&["text", "messageForeground"], if appearance == "light" { "#1f2328" } else { "#ececec" }),
        "accent": pick(&["accent", "focus"], "#346bf1"),
        "canvas": canvas,
    });
    let variants: BTreeMap<&String, &BTreeMap<String, String>> =
        file.variants.iter().filter(|(mode, roles)| *mode != appearance && !roles.is_empty()).collect();
    if let (false, Some(object)) = (variants.is_empty(), entry.as_object_mut()) {
        object.insert("variants".to_string(), json!(variants));
    }
    entry
}

fn theme_value(entry: &Value) -> Value {
    let key = if cfg!(target_os = "macos") { "id" } else { "name" };
    entry.get(key).cloned().unwrap_or(Value::Null)
}

fn same_theme(known: &Value, entry: &Value) -> bool {
    let id = |value: &Value| value.get("id").and_then(Value::as_str).map(str::to_string);
    let name = |value: &Value| value.get("name").and_then(Value::as_str).map(str::to_string);
    (id(known).is_some() && id(known) == id(entry)) || (name(known).is_some() && name(known) == name(entry))
}

pub(crate) struct Mapped {
    pub(crate) provider: String,
    pub(crate) model: String,
}

fn map_model(t3: &T3Data, instance_id: &str, model: &str) -> Result<Mapped, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("No model is set in T3 Code.".to_string());
    }
    let driver = t3
        .instances
        .iter()
        .find(|instance| instance.id == instance_id)
        .map_or_else(|| instance_id.to_string(), |instance| instance.driver.clone());
    match harness_of(&driver) {
        Some(Harness::ClaudeCode) => {
            if harness::models(Harness::ClaudeCode).iter().any(|option| option.id == model) {
                Ok(Mapped { provider: CLAUDE_PROVIDER.to_string(), model: model.to_string() })
            } else {
                Err(format!("Local Studio's Claude Code list has no {model}."))
            }
        }
        Some(Harness::Codex) => Ok(Mapped { provider: CODEX_PROVIDER.to_string(), model: model.to_string() }),
        Some(Harness::Eukhe) | None => Err(unsupported_reason(&driver)),
    }
}

fn model_label(t3: &T3Data, instance_id: &str, model: &str) -> String {
    let driver =
        t3.instances.iter().find(|instance| instance.id == instance_id).map(|instance| instance.driver.as_str());
    format!("{model} ({})", driver_name(driver.unwrap_or(instance_id)))
}

pub(crate) fn sync(t3: &T3Data, ls: &mut Ls, state: &mut SyncState, apply: bool) -> Outcome {
    let choices = state.choices;
    let mut engine = Engine { imported: &mut state.imported, choices, apply, groups: BTreeMap::new() };
    let mut problems = t3.problems.clone();
    accounts_group(&mut engine, t3, ls);
    projects_group(&mut engine, t3, ls, &mut problems);
    models_group(&mut engine, t3, ls);
    keybindings_group(&mut engine, t3, ls);
    settings_groups(&mut engine, t3, ls);
    let groups = [
        T3Kind::Accounts,
        T3Kind::Projects,
        T3Kind::Models,
        T3Kind::Keybindings,
        T3Kind::Appearance,
        T3Kind::Preferences,
    ]
    .into_iter()
    .map(|kind| T3Group {
        kind,
        title: title(kind).to_string(),
        chosen: chosen(choices, kind),
        items: engine.groups.remove(&kind).unwrap_or_default(),
    })
    .collect();
    Outcome { groups, problems }
}

fn accounts_group(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Accounts;
    let mut instances: Vec<&Instance> = t3.instances.iter().collect();
    let first = t3.default_model.as_ref().map(|selection| selection.instance_id.clone());
    instances.sort_by_key(|instance| (Some(&instance.id) != first.as_ref(), harness_of(&instance.driver).is_none()));
    for instance in instances {
        let name = instance.display_name.clone().unwrap_or_else(|| instance.id.clone());
        let label = format!("{}: {name}", driver_name(&instance.driver));
        let key = format!("account:{}", instance.id);
        let mut notes: Vec<String> = instance.secrets.iter().map(|secret| format!("{secret} set in T3 Code")).collect();
        let Some(harness) = harness_of(&instance.driver) else {
            notes.insert(0, unsupported_reason(&instance.driver));
            engine.item(kind, key, label, notes.join(" "), T3ItemState::Unsupported);
            continue;
        };
        if !instance.enabled {
            notes.insert(0, "Turned off in T3 Code.".to_string());
            engine.item(kind, key, label, notes.join(" "), T3ItemState::Unsupported);
            continue;
        }
        let Some(dir) = account_dir(instance, harness) else {
            engine.item(kind, key, label, "HOME is not set.".to_string(), T3ItemState::Unsupported);
            continue;
        };
        if !dir.is_dir() {
            notes.insert(0, format!("{} does not exist.", display(&dir)));
            engine.item(kind, key, label, notes.join(" "), T3ItemState::Unsupported);
            continue;
        }
        let dir_text = dir.to_string_lossy().into_owned();
        let listed = match harness {
            Harness::ClaudeCode => ls.settings.harness_accounts.claude.clone(),
            Harness::Codex | Harness::Eukhe => ls.settings.harness_accounts.codex.clone(),
        };
        let effective = accounts::list(harness, &listed);
        let membership =
            engine.member(kind, &format!("account:{}:{dir_text}", harness_key(harness)), effective.contains(&dir));
        if engine.live(kind) && membership == T3ItemState::Import {
            let mut list = if listed.iter().any(|entry| !entry.trim().is_empty()) {
                listed
            } else {
                effective.iter().map(|path| path.to_string_lossy().into_owned()).collect()
            };
            list.push(dir_text.clone());
            match harness {
                Harness::ClaudeCode => ls.settings.harness_accounts.claude = list,
                Harness::Codex | Harness::Eukhe => ls.settings.harness_accounts.codex = list,
            }
        }
        let desired = json!({ "name": name, "accent": instance.accent });
        let current = ls
            .settings
            .harness_accounts
            .labels
            .iter()
            .find(|known| known.dir == dir_text)
            .map_or(Value::Null, |known| json!({ "name": known.name, "accent": known.accent }));
        let naming = engine.scalar(kind, &format!("accountLabel:{dir_text}"), &desired, &current, &Value::Null);
        if engine.live(kind) && naming == T3ItemState::Import {
            ls.settings.harness_accounts.labels.retain(|known| known.dir != dir_text);
            ls.settings.harness_accounts.labels.push(AccountLabel {
                dir: dir_text.clone(),
                name: name.clone(),
                accent: instance.accent.clone(),
                source: Some(SOURCE.to_string()),
            });
        }
        let signed = if accounts::signed_in(harness, &dir) { "signed in" } else { "no sign-in found there yet" };
        let mut detail = format!("{}, {signed}.", display(&dir));
        if instance.launch_args {
            detail.push_str(" Launch arguments stay in T3 Code.");
        }
        for note in notes {
            detail.push(' ');
            detail.push_str(&note);
        }
        engine.item(kind, key, label, detail, worst(&[membership, naming]));
    }
}

fn harness_key(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude",
        Harness::Codex => "codex",
        Harness::Eukhe => "eukhe",
    }
}

fn projects_group(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls, problems: &mut Vec<String>) {
    let kind = T3Kind::Projects;
    if !t3.projects_read {
        problems.push("No T3 Code project database was found, so no projects were read.".to_string());
    }
    let mut seen = BTreeSet::new();
    for project in &t3.projects {
        let label = if project.title.trim().is_empty() { project.path.clone() } else { project.title.clone() };
        let key = format!("project:{}", project.path);
        if project.archived {
            engine.item(kind, key, label, "Archived in T3 Code.".to_string(), T3ItemState::Unsupported);
            continue;
        }
        let canonical = match std::fs::canonicalize(&project.path) {
            Ok(path) if path.is_dir() => path.to_string_lossy().into_owned(),
            _ => {
                engine.item(
                    kind,
                    key,
                    label,
                    format!("{} is not a folder on this machine.", display(Path::new(&project.path))),
                    T3ItemState::Unsupported,
                );
                continue;
            }
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let present = ls.projects.iter().any(|known| known.path == canonical);
        let state = engine.member(kind, &format!("project:{canonical}"), present);
        if engine.live(kind) && state == T3ItemState::Import {
            ls.projects.push(StoredProject { path: canonical.clone(), name: label.clone(), icon: None });
        }
        engine.item(kind, format!("project:{canonical}"), label, display(Path::new(&canonical)), state);
    }
}

fn selection_value(mapped: &Mapped, thinking: Option<&str>, with_thinking: bool) -> Value {
    if with_thinking {
        json!({ "provider": mapped.provider, "model": mapped.model, "thinking": thinking })
    } else {
        json!({ "provider": mapped.provider, "model": mapped.model })
    }
}

fn models_group(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    model_picks(engine, t3, ls);
    model_lists(engine, t3, ls);
    project_models(engine, t3, ls);
}

fn model_picks(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Models;
    let picks: [(&str, &str, Option<&Selection>, bool); 3] = [
        ("model.default", "Default model", t3.default_model.as_ref(), true),
        ("model.textGeneration", "Text generation model", t3.text_model.as_ref(), false),
        ("model.sourceControl", "Commit and pull request writer", t3.source_control_model.as_ref(), false),
    ];
    for (key, label, selection, with_thinking) in picks {
        let Some(selection) = selection else { continue };
        let shown_model = model_label(t3, &selection.instance_id, &selection.model);
        let mapped = match map_model(t3, &selection.instance_id, &selection.model) {
            Ok(mapped) => mapped,
            Err(reason) => {
                engine.item(
                    kind,
                    key.to_string(),
                    label.to_string(),
                    format!("{shown_model}. {reason}"),
                    T3ItemState::Unsupported,
                );
                continue;
            }
        };
        let thinking = selection.effort();
        let desired = selection_value(&mapped, thinking.as_deref(), with_thinking);
        let settings = &ls.settings;
        let current = match key {
            "model.default" => json!({
                "provider": settings.default_provider,
                "model": settings.default_model,
                "thinking": settings.default_thinking_level,
            }),
            "model.textGeneration" => {
                json!({ "provider": settings.text_generation_provider, "model": settings.text_generation_model })
            }
            _ => json!({ "provider": settings.source_control_provider, "model": settings.source_control_model }),
        };
        let default = if with_thinking {
            json!({ "provider": null, "model": null, "thinking": null })
        } else {
            json!({ "provider": null, "model": null })
        };
        let state = engine.scalar(kind, key, &desired, &current, &default);
        if engine.live(kind) && state == T3ItemState::Import {
            let settings = &mut ls.settings;
            match key {
                "model.default" => {
                    settings.default_provider = Some(mapped.provider.clone());
                    settings.default_model = Some(mapped.model.clone());
                    settings.default_thinking_level.clone_from(&thinking);
                }
                "model.textGeneration" => {
                    settings.text_generation_provider = Some(mapped.provider.clone());
                    settings.text_generation_model = Some(mapped.model.clone());
                }
                _ => {
                    settings.source_control_provider = Some(mapped.provider.clone());
                    settings.source_control_model = Some(mapped.model.clone());
                }
            }
        }
        let detail = match &thinking {
            Some(level) if with_thinking => format!("{shown_model}, {level} effort"),
            _ => shown_model,
        };
        engine.item(kind, key.to_string(), label.to_string(), detail, state);
    }
}

fn model_lists(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Models;
    let mut seen = BTreeSet::new();
    for favorite in &t3.favorites {
        let shown_model = model_label(t3, &favorite.provider, &favorite.model);
        let mapped = match map_model(t3, &favorite.provider, &favorite.model) {
            Ok(mapped) => mapped,
            Err(reason) => {
                let key = format!("favorite:{}/{}", favorite.provider, favorite.model);
                engine.item(kind, key, format!("Favorite {shown_model}"), reason, T3ItemState::Unsupported);
                continue;
            }
        };
        let model_key = ModelKey { provider: mapped.provider.clone(), model: mapped.model.clone() };
        let key = format!("favorite:{}/{}", mapped.provider, mapped.model);
        if !seen.insert(key.clone()) {
            continue;
        }
        let state = engine.member(kind, &key, ls.favorites.contains(&model_key));
        if engine.live(kind) && state == T3ItemState::Import {
            ls.favorites.push(model_key);
        }
        engine.item(kind, key, format!("Favorite {shown_model}"), String::new(), state);
    }
    let mut skipped: Vec<String> = Vec::new();
    for (instance, model) in &t3.hidden {
        let Ok(mapped) = map_model(t3, instance, model) else {
            skipped.push(model.clone());
            continue;
        };
        let model_key = ModelKey { provider: mapped.provider.clone(), model: mapped.model.clone() };
        let key = format!("hidden:{}/{}", mapped.provider, mapped.model);
        if !seen.insert(key.clone()) {
            continue;
        }
        let state = engine.member(kind, &key, ls.hidden.contains(&model_key));
        if engine.live(kind) && state == T3ItemState::Import {
            ls.hidden.push(model_key);
        }
        engine.item(kind, key, format!("Hidden {}", model_label(t3, instance, model)), String::new(), state);
    }
    if !skipped.is_empty() {
        skipped.sort();
        skipped.dedup();
        engine.item(
            kind,
            "hidden:unlisted".to_string(),
            format!("{} hidden models", skipped.len()),
            format!("Local Studio does not list these, so there is nothing to hide: {}.", skipped.join(", ")),
            T3ItemState::Unsupported,
        );
    }
    if !t3.ordered.is_empty() {
        engine.item(
            kind,
            "modelOrder".to_string(),
            "Model order".to_string(),
            "Local Studio orders models itself; favorites stay on top.".to_string(),
            T3ItemState::Unsupported,
        );
    }
}

fn project_models(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Models;
    for project in &t3.projects {
        let Some(selection) = t3.project_models.get(&project.id).or(project.model.as_ref()) else { continue };
        let Ok(canonical) = std::fs::canonicalize(&project.path) else { continue };
        let canonical = canonical.to_string_lossy().into_owned();
        if !ls.projects.iter().any(|known| known.path == canonical) {
            continue;
        }
        let label = format!("{} default model", project.title);
        let key = format!("projectModel:{canonical}");
        let shown_model = model_label(t3, &selection.instance_id, &selection.model);
        let mapped = match map_model(t3, &selection.instance_id, &selection.model) {
            Ok(mapped) => mapped,
            Err(reason) => {
                engine.item(kind, key, label, format!("{shown_model}. {reason}"), T3ItemState::Unsupported);
                continue;
            }
        };
        let thinking = selection.effort();
        let desired = selection_value(&mapped, thinking.as_deref(), true);
        let entry = ls.project_settings.get(&canonical).cloned().unwrap_or_default();
        let current = json!({ "provider": entry.default_provider, "model": entry.default_model, "thinking": entry.default_thinking_level });
        let default = json!({ "provider": null, "model": null, "thinking": null });
        let state = engine.scalar(kind, &key, &desired, &current, &default);
        if engine.live(kind) && state == T3ItemState::Import {
            let entry = ls.project_settings.entry(canonical.clone()).or_default();
            entry.default_provider = Some(mapped.provider.clone());
            entry.default_model = Some(mapped.model.clone());
            entry.default_thinking_level.clone_from(&thinking);
        }
        engine.item(kind, key, label, shown_model, state);
    }
}

fn keybindings_group(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Keybindings;
    let defaults: BTreeMap<String, Option<String>> = settings::keybindings(&BTreeMap::new())
        .into_iter()
        .map(|binding| (binding.command, binding.default_shortcut))
        .collect();
    let mut seen = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    for binding in &t3.bindings {
        if !settings::is_known_command(&binding.command) {
            unknown.insert(binding.command.clone());
            continue;
        }
        if !seen.insert(binding.command.clone()) {
            continue;
        }
        let key = format!("keybinding:{}", binding.command);
        let label = settings::command_title(&binding.command);
        let shortcut = match settings::normalize_shortcut(&binding.key) {
            Ok(shortcut) => shortcut,
            Err(reason) => {
                engine.item(kind, key, label, reason, T3ItemState::Unsupported);
                continue;
            }
        };
        let default = defaults.get(&binding.command).cloned().flatten();
        let current = ls.keybindings.get(&binding.command).cloned().unwrap_or_else(|| default.clone());
        let as_value = |shortcut: Option<&String>| shortcut.map_or(Value::Null, |text| Value::String(text.clone()));
        let desired = Value::String(shortcut.clone());
        let state = engine.scalar(kind, &key, &desired, &as_value(current.as_ref()), &as_value(default.as_ref()));
        if engine.live(kind) && state == T3ItemState::Import {
            if Some(&shortcut) == default.as_ref() {
                ls.keybindings.remove(&binding.command);
            } else {
                ls.keybindings.insert(binding.command.clone(), Some(shortcut.clone()));
            }
        }
        engine.item(kind, key, label, shortcut, state);
    }
    if !unknown.is_empty() {
        let names: Vec<String> = unknown.into_iter().collect();
        engine.item(
            kind,
            "keybinding:unknown".to_string(),
            format!("{} T3 Code commands", names.len()),
            format!("Local Studio has no such command yet: {}.", names.join(", ")),
            T3ItemState::Unsupported,
        );
    }
}

fn setting(engine: &mut Engine<'_>, ls: &mut Ls, kind: T3Kind, ls_key: &str, label: &str, raw: &Value) {
    let key = format!("setting.{ls_key}");
    let Some(desired) = convert(ls_key, raw) else { return };
    let mut map = settings_map(&ls.settings);
    let current = map.get(ls_key).cloned().unwrap_or(Value::Null);
    let default = settings_map(&Settings::default()).get(ls_key).cloned().unwrap_or(Value::Null);
    map.insert(ls_key.to_string(), desired.clone());
    let Ok(candidate) = serde_json::from_value::<Settings>(Value::Object(map)) else {
        engine.item(
            kind,
            key,
            label.to_string(),
            format!("Local Studio has no {} setting.", shown(raw)),
            T3ItemState::Unsupported,
        );
        return;
    };
    let candidate = settings::sanitize(candidate);
    let desired = settings_map(&candidate).get(ls_key).cloned().unwrap_or(desired);
    let state = engine.scalar(kind, &key, &desired, &current, &default);
    if engine.live(kind) && state == T3ItemState::Import {
        ls.settings = candidate;
    }
    engine.item(kind, key, label.to_string(), shown(&desired), state);
}

fn settings_groups(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    if let Some(appearance) = &t3.appearance {
        setting(engine, ls, T3Kind::Appearance, "colorScheme", "Light or dark", &Value::String(appearance.clone()));
    }
    theme(engine, t3, ls);
    for (ls_key, t3_key, label) in APPEARANCE {
        if let Some(raw) = t3.client.get(t3_key) {
            setting(engine, ls, T3Kind::Appearance, ls_key, label, raw);
        }
    }
    for (ls_key, t3_key, label) in PREFERENCES {
        if let Some(raw) = t3.client.get(t3_key) {
            setting(engine, ls, T3Kind::Preferences, ls_key, label, raw);
        }
    }
    if let Some(base) = &t3.add_project_base {
        setting(
            engine,
            ls,
            T3Kind::Preferences,
            "addProjectBaseDirectory",
            "New project folder",
            &Value::String(base.clone()),
        );
    }
}

fn theme(engine: &mut Engine<'_>, t3: &T3Data, ls: &mut Ls) {
    let kind = T3Kind::Appearance;
    let Some(id) = &t3.theme else { return };
    let key = "setting.theme".to_string();
    let Some(file) = t3.themes.get(id) else {
        engine.item(
            kind,
            key,
            "Theme".to_string(),
            format!("{id} is a T3 Code built-in theme with no Local Studio match."),
            T3ItemState::Unsupported,
        );
        return;
    };
    let entry = theme_entry(id, file);
    let name = entry.get("name").and_then(Value::as_str).unwrap_or(id).to_string();
    let themes = custom_themes(&ls.settings);
    let present = themes.iter().any(|known| same_theme(known, &entry));
    let stored = engine.member(kind, &format!("themeFile:{id}"), present);
    if engine.live(kind) && stored == T3ItemState::Import {
        let mut themes = themes;
        themes.push(entry.clone());
        ls.settings.custom_themes = Value::Array(themes).to_string();
    }
    let desired = theme_value(&entry);
    let current = ls.settings.theme.clone().map_or(Value::Null, Value::String);
    let chosen = engine.scalar(kind, &key, &desired, &current, &Value::Null);
    if engine.live(kind) && chosen == T3ItemState::Import {
        ls.settings.theme = desired.as_str().map(str::to_string);
    }
    engine.item(kind, key, "Theme".to_string(), name, worst(&[stored, chosen]));
}
