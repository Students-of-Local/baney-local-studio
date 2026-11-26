use std::rc::Rc;

use local_studio_core::{
    CatalogPlugin, ConsentNeed, PluginEntry, PluginGrant, PluginRow, PluginState, PluginUpdate, Update,
};

use crate::app::App;
use crate::fixtures::conversation;

pub const SCENES: [&str; 3] = ["plugins-pane", "plugins-pane-light", "plugins-empty"];

fn grant(id: &str, need: ConsentNeed, sentence: &str, gates: Option<&str>, granted: bool) -> PluginGrant {
    PluginGrant { id: id.to_string(), need, sentence: sentence.to_string(), gates: gates.map(str::to_string), granted }
}

fn entry(name: &str, description: &str) -> PluginEntry {
    PluginEntry { name: name.to_string(), description: description.to_string() }
}

fn folder(id: &str) -> String {
    format!("/home/demo/.local/share/local-studio/plugins/{id}")
}

fn rows() -> Vec<PluginRow> {
    vec![field_notes(), hildegard(), ledger(), broken()]
}

fn field_notes() -> PluginRow {
    PluginRow {
        id: "field-notes".to_string(),
        name: "Field Notes".to_string(),
        version: "1.4.0".to_string(),
        description: "Reads pages in its own tabs and keeps notes for a thread".to_string(),
        icon: None,
        state: PluginState::Running,
        detail: "Its tools are on in every thread; it cannot click and type in those tabs until you turn that on"
            .to_string(),
        grants: vec![
            grant("mcp-server", ConsentNeed::Needed, "Give agents tools", None, true),
            grant("ask-user", ConsentNeed::Needed, "Ask you questions in a thread", None, true),
            grant(
                "screen-capture",
                ConsentNeed::Sensitive,
                "See the windows or tabs it opened",
                Some("page screenshots for the agent"),
                true,
            ),
            grant(
                "synthetic-input",
                ConsentNeed::Sensitive,
                "Click and type in those tabs",
                Some("clicking and typing in its own tabs"),
                false,
            ),
        ],
        told: true,
        commands: vec![entry("notes", "Summarize what this thread has read")],
        skills: vec![entry("field-notes", "Keep dated notes from pages you read")],
        source: "https://github.com/example/field-notes".to_string(),
        revision: Some("3f9a2c1".to_string()),
        pinned: false,
        channel: Some("stable".to_string()),
        update: Some(PluginUpdate {
            version: "1.5.0".to_string(),
            revision: "8b21e0d".to_string(),
            asks: vec!["accessibility".to_string()],
        }),
        folder: folder("field-notes"),
        local_only: false,
    }
}

fn hildegard() -> PluginRow {
    PluginRow {
        id: "hildegard".to_string(),
        name: "hildegard".to_string(),
        version: "0.1.17".to_string(),
        description:
            "Hildegard for Local Studio: make images, video, speech, designed voices and cleaned audio on your own GPU"
                .to_string(),
        icon: None,
        state: PluginState::Running,
        detail: "Its commands and skills are on".to_string(),
        grants: Vec::new(),
        told: false,
        commands: vec![
            entry("image", "Draw an image on your GPU with Hildegard"),
            entry("speak", "Turn a script into speech in one of your voices"),
        ],
        skills: vec![entry("hildegard-image", "Draw an image with the local image model")],
        source: "Local Studio catalog".to_string(),
        revision: Some("504d949".to_string()),
        pinned: true,
        channel: Some("stable".to_string()),
        update: None,
        folder: folder("hildegard"),
        local_only: false,
    }
}

fn ledger() -> PluginRow {
    PluginRow {
        id: "ledger".to_string(),
        name: "Ledger".to_string(),
        version: "0.3.0".to_string(),
        description: "Files a private ledger entry".to_string(),
        icon: None,
        state: PluginState::Waiting,
        detail: "Waiting for you to allow it to give agents tools".to_string(),
        grants: vec![grant("mcp-server", ConsentNeed::Needed, "Give agents tools", None, false)],
        told: false,
        commands: vec![entry("ledger", "File today's entry")],
        skills: Vec::new(),
        source: "/home/demo/Projects/ledger".to_string(),
        revision: None,
        pinned: false,
        channel: None,
        update: None,
        folder: folder("ledger"),
        local_only: true,
    }
}

fn broken() -> PluginRow {
    PluginRow {
        id: "broken".to_string(),
        name: "broken".to_string(),
        version: String::new(),
        description: String::new(),
        icon: None,
        state: PluginState::Failed,
        detail: "It did not load: bad version: \"1\" (use semver, such as 1.0.0)".to_string(),
        grants: Vec::new(),
        told: false,
        commands: Vec::new(),
        skills: Vec::new(),
        source: String::new(),
        revision: None,
        pinned: false,
        channel: None,
        update: None,
        folder: folder("broken"),
        local_only: false,
    }
}

fn catalog(installed: bool) -> Vec<CatalogPlugin> {
    [
        ("gantrel", "Gantrel", "Design 3D printable parts with a local model, then slice, upload and watch them on your printers", "5cdd089", None),
        ("hildegard", "Hildegard", "Make images, video, speech, designed voices and cleaned audio on your own GPU, each one checked before you see it", "504d949", Some("0.1.17")),
        ("smoothstone", "Smoothstone", "Make 3D printable parts with the stone CLI and its tested parametric recipes", "715e0ac", None),
        ("browser-use", "Local Studio browser use", "Work in Chromium tabs in the background through the Local Studio extension", "f5d7a0a", None),
        ("computer-use", "Local Studio computer use", "Read and operate desktop apps through accessibility APIs, with the Local Studio extension's helper", "f5d7a0a", None),
    ]
    .into_iter()
    .map(|(id, name, description, revision, version)| CatalogPlugin {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        grants: Vec::new(),
        source: match id {
            "browser-use" | "computer-use" => "https://github.com/Students-of-Local/baney-local-studio".to_string(),
            _ => format!("https://github.com/baney75/{id}"),
        },
        revision: revision.to_string(),
        local_only: false,
        installed: if installed { version.map(str::to_string) } else { None },
    })
    .collect()
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if !SCENES.contains(&scene) {
        return false;
    }
    conversation(app, false);
    if scene.ends_with("-light") {
        let settings = local_studio_core::Settings {
            color_scheme: local_studio_core::ColorScheme::Light,
            ..local_studio_core::Settings::default()
        };
        app.apply(vec![Update::Settings { settings }]);
    }
    let checked = Some(crate::format::now_ms() - 600_000);
    let update = if scene == "plugins-empty" {
        Update::Plugins { plugins: Vec::new(), catalog: catalog(false), channel: "stable".to_string(), checked: None }
    } else {
        crate::plugins::expand(&["field-notes", "hildegard"]);
        Update::Plugins { plugins: rows(), catalog: catalog(true), channel: "stable".to_string(), checked }
    };
    app.apply(vec![update]);
    app.open_settings(Some("plugins"));
    true
}
