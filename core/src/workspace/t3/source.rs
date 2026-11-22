use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;

const STATE_DIRS: [&str; 2] = ["userdata", "dev"];
const MARKERS: [&str; 3] = ["settings.json", "client-settings.json", "keybindings.json"];
const DATABASES: [&str; 2] = ["statev2.sqlite", "server-owner.sqlite"];
const WATCHED: [&str; 4] = ["settings.json", "client-settings.json", "keybindings.json", "statev2.sqlite"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Presence(bool);

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Seen;
        impl<'de> Visitor<'de> for Seen {
            type Value = Presence;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any value")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Presence, E> {
                Ok(Presence(!value.trim().is_empty()))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Presence, E> {
                Ok(Presence(true))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Presence, E> {
                Ok(Presence(true))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Presence, E> {
                Ok(Presence(true))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Presence, E> {
                Ok(Presence(true))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Presence, E> {
                Ok(Presence(false))
            }
            fn visit_none<E: de::Error>(self) -> Result<Presence, E> {
                Ok(Presence(false))
            }
            fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Presence, D::Error> {
                deserializer.deserialize_any(Seen)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Presence, A::Error> {
                let mut any = false;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(Presence(any))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Presence, A::Error> {
                let mut any = false;
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(Presence(any))
            }
        }
        deserializer.deserialize_any(Seen)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ConfigFile {
    home_path: Option<String>,
    launch_args: Option<Value>,
    api_key: Presence,
    server_password: Presence,
    token: Presence,
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct InstanceFile {
    driver: String,
    display_name: Option<String>,
    enabled: bool,
    accent_color: Option<String>,
    config: ConfigFile,
}

impl Default for InstanceFile {
    fn default() -> Self {
        Self {
            driver: String::new(),
            display_name: None,
            enabled: true,
            accent_color: None,
            config: ConfigFile::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Selection {
    pub(crate) instance_id: String,
    pub(crate) model: String,
    pub(crate) options: Vec<SelectionOption>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SelectionOption {
    pub(crate) id: String,
    pub(crate) value: Value,
}

impl Selection {
    pub(crate) fn effort(&self) -> Option<String> {
        self.options
            .iter()
            .find(|option| matches!(option.id.as_str(), "effort" | "reasoningEffort" | "thinking"))
            .and_then(|option| option.value.as_str())
            .map(str::to_string)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Favorite {
    pub(crate) provider: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ModelPreferences {
    hidden_models: Vec<String>,
    model_order: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProjectOverride {
    default_model_selection: Option<Selection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SettingsFile {
    provider_instances: BTreeMap<String, InstanceFile>,
    default_model_selection: Option<Selection>,
    text_generation_model_selection: Option<Selection>,
    source_control_writer_model_selection: Option<Selection>,
    favorites: Vec<Favorite>,
    provider_model_preferences: BTreeMap<String, ModelPreferences>,
    project_settings_overrides: BTreeMap<String, ProjectOverride>,
    default_theme: Option<String>,
    default_appearance: Option<String>,
    add_project_base_directory: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct BindingFile {
    key: String,
    command: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ThemeFile {
    pub(crate) name: Option<String>,
    pub(crate) appearance: Option<String>,
    pub(crate) colors: BTreeMap<String, String>,
    pub(crate) variants: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Instance {
    pub(crate) id: String,
    pub(crate) driver: String,
    pub(crate) display_name: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) accent: Option<String>,
    pub(crate) home_path: Option<String>,
    pub(crate) launch_args: bool,
    pub(crate) secrets: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) archived: bool,
    pub(crate) model: Option<Selection>,
}

#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) key: String,
    pub(crate) command: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct T3Data {
    pub(crate) dir: PathBuf,
    pub(crate) instances: Vec<Instance>,
    pub(crate) default_model: Option<Selection>,
    pub(crate) text_model: Option<Selection>,
    pub(crate) source_control_model: Option<Selection>,
    pub(crate) favorites: Vec<Favorite>,
    pub(crate) hidden: Vec<(String, String)>,
    pub(crate) ordered: Vec<String>,
    pub(crate) project_models: BTreeMap<String, Selection>,
    pub(crate) projects: Vec<Project>,
    pub(crate) projects_read: bool,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) client: serde_json::Map<String, Value>,
    pub(crate) appearance: Option<String>,
    pub(crate) theme: Option<String>,
    pub(crate) themes: BTreeMap<String, ThemeFile>,
    pub(crate) add_project_base: Option<String>,
    pub(crate) problems: Vec<String>,
}

pub(crate) fn locate(home: &Path) -> Option<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("T3CODE_HOME").filter(|value| !value.is_empty()) {
        bases.push(PathBuf::from(explicit));
    }
    bases.push(home.join(".t3"));
    bases
        .iter()
        .flat_map(|base| STATE_DIRS.iter().map(move |state| base.join(state)))
        .find(|dir| MARKERS.iter().any(|marker| dir.join(marker).is_file()))
}

pub(crate) fn fingerprint(dir: &Path) -> Vec<(String, u64, u64)> {
    let mut paths: Vec<PathBuf> = WATCHED.iter().map(|name| dir.join(name)).collect();
    let mut themes: Vec<PathBuf> = std::fs::read_dir(dir.join("themes"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect();
    themes.sort();
    paths.extend(themes);
    paths
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| u64::try_from(time.as_millis()).unwrap_or(u64::MAX));
            Some((path.to_string_lossy().into_owned(), modified, metadata.len()))
        })
        .collect()
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path, problems: &mut Vec<String>) -> T {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            problems.push(format!("{} could not be read: {error}", path.display()));
            T::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => {
            problems.push(format!("{} could not be read: {error}", path.display()));
            T::default()
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn instance(id: String, instance: InstanceFile) -> Instance {
    let config = instance.config;
    let mut secrets = Vec::new();
    for (present, name) in
        [(config.api_key, "API key"), (config.server_password, "server password"), (config.token, "token")]
    {
        if present.0 {
            secrets.push(name);
        }
    }
    let launch_args = match config.launch_args {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        _ => false,
    };
    Instance {
        id,
        driver: instance.driver,
        display_name: non_empty(instance.display_name),
        enabled: instance.enabled,
        accent: non_empty(instance.accent_color),
        home_path: non_empty(config.home_path),
        launch_args,
        secrets,
    }
}

fn read_themes(dir: &Path, problems: &mut Vec<String>) -> BTreeMap<String, ThemeFile> {
    let mut themes = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("themes")).into_iter().flatten().filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(id) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        let file: ThemeFile = read_json(&path, problems);
        if !file.colors.is_empty() {
            themes.insert(id, file);
        }
    }
    themes
}

pub(crate) fn read(dir: &Path) -> T3Data {
    let mut problems = Vec::new();
    let settings: SettingsFile = read_json(&dir.join("settings.json"), &mut problems);
    let client: Value = read_json(&dir.join("client-settings.json"), &mut problems);
    let client = match client {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let bindings: Vec<BindingFile> = read_json(&dir.join("keybindings.json"), &mut problems);
    let instances = settings.provider_instances.into_iter().map(|(id, file)| instance(id, file)).collect();
    let mut favorites = settings.favorites;
    if let Some(Value::Array(extra)) = client.get("favorites") {
        for favorite in extra.iter().filter_map(|value| serde_json::from_value::<Favorite>(value.clone()).ok()) {
            if !favorites.iter().any(|known| known.provider == favorite.provider && known.model == favorite.model) {
                favorites.push(favorite);
            }
        }
    }
    let mut preferences = settings.provider_model_preferences;
    if let Some(Value::Object(extra)) = client.get("providerModelPreferences") {
        for (instance, value) in extra {
            let Ok(extra) = serde_json::from_value::<ModelPreferences>(value.clone()) else { continue };
            let entry = preferences.entry(instance.clone()).or_default();
            for model in extra.hidden_models {
                if !entry.hidden_models.contains(&model) {
                    entry.hidden_models.push(model);
                }
            }
            if entry.model_order.is_empty() {
                entry.model_order = extra.model_order;
            }
        }
    }
    let mut hidden = Vec::new();
    let mut ordered = Vec::new();
    for (instance, preference) in preferences {
        hidden.extend(preference.hidden_models.into_iter().map(|model| (instance.clone(), model)));
        if !preference.model_order.is_empty() {
            ordered.push(instance);
        }
    }
    let theme = non_empty(settings.default_theme);
    let themes = read_themes(dir, &mut problems);
    let (projects, projects_read) = match read_projects(dir) {
        Ok(Some(projects)) => (projects, true),
        Ok(None) => (Vec::new(), false),
        Err(error) => {
            problems.push(error);
            (Vec::new(), false)
        }
    };
    T3Data {
        dir: dir.to_path_buf(),
        instances,
        default_model: settings.default_model_selection,
        text_model: settings.text_generation_model_selection,
        source_control_model: settings.source_control_writer_model_selection,
        favorites,
        hidden,
        ordered,
        project_models: settings
            .project_settings_overrides
            .into_iter()
            .filter_map(|(id, entry)| entry.default_model_selection.map(|selection| (id, selection)))
            .collect(),
        projects,
        projects_read,
        bindings: bindings
            .into_iter()
            .filter(|binding| !binding.key.trim().is_empty() && !binding.command.trim().is_empty())
            .map(|binding| Binding { key: binding.key, command: binding.command })
            .collect(),
        client,
        appearance: non_empty(settings.default_appearance),
        theme,
        themes,
        add_project_base: non_empty(settings.add_project_base_directory),
        problems,
    }
}

fn read_projects(dir: &Path) -> Result<Option<Vec<Project>>, String> {
    use rusqlite::{Connection, OpenFlags};
    for name in DATABASES {
        let path = dir.join(name);
        let Ok(metadata) = std::fs::metadata(&path) else { continue };
        if !metadata.is_file() || metadata.len() == 0 {
            continue;
        }
        let uri = format!("file:{}?mode=ro&immutable=1", uri_path(&path));
        let connection = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("{} could not be opened: {error}", path.display()))?;
        let has_table: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'projection_projects')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        if !has_table {
            continue;
        }
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('projection_projects')")
            .and_then(|mut statement| {
                statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        let has = |column: &str| columns.iter().any(|known| known == column);
        let archived = if has("archived_at") { "archived_at IS NOT NULL" } else { "0" };
        let model = if has("default_model_selection_json") { "default_model_selection_json" } else { "NULL" };
        let query = format!(
            "SELECT project_id, title, workspace_root, {archived}, {model} FROM projection_projects \
             WHERE deleted_at IS NULL ORDER BY updated_at DESC"
        );
        let projects = connection
            .prepare(&query)
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| {
                        let model: Option<String> = row.get(4)?;
                        Ok(Project {
                            id: row.get(0)?,
                            title: row.get(1)?,
                            path: row.get(2)?,
                            archived: row.get(3)?,
                            model: model.and_then(|text| serde_json::from_str::<Selection>(&text).ok()),
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        return Ok(Some(projects));
    }
    Ok(None)
}

fn uri_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            other => out.push(other),
        }
    }
    out
}
