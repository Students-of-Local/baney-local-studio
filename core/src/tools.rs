use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::workspace::model::Settings;

pub(crate) const SERVER: &str = "local-studio";
const PROGRAM: &str = "local-studio-browser";
const PLACES: [&str; 3] = ["browser", "lib/local-studio/browser", "Resources/browser"];
const PASSED: [&str; 10] = [
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "LOCAL_STUDIO_BROWSER_DIR",
    "SWAYSOCK",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_CONFIG_HOME",
    "XDG_CURRENT_DESKTOP",
    "XDG_RUNTIME_DIR",
];

fn program() -> Option<PathBuf> {
    let found = match std::env::var_os("LOCAL_STUDIO_BROWSER_BIN").filter(|value| !value.is_empty()) {
        Some(named) => PathBuf::from(named),
        None => crate::skills::app()?
            .ancestors()
            .skip(1)
            .take(2)
            .flat_map(|dir| PLACES.map(|place| dir.join(place).join(PROGRAM)))
            .find(|path| path.is_file())?,
    };
    found.is_file().then(|| std::path::absolute(&found).ok())?
}

pub(crate) fn holds(servers: &[Value]) -> bool {
    servers.iter().any(|server| server["name"] == SERVER)
}

pub(crate) fn keeps(sent: &Value, wanted: &[Value]) -> bool {
    wanted.contains(sent)
        || wanted.iter().any(|server| {
            let tools = |server: &Value| server["args"][4].as_str().map(str::to_string);
            server["name"] == SERVER
                && sent["name"] == SERVER
                && tools(server).zip(tools(sent)).is_some_and(|(wanted, sent)| wanted.contains(&sent))
        })
}

pub(crate) fn server(settings: &Settings, thread: &str, local: bool) -> Option<Value> {
    let tools = match (settings.browser_use, settings.computer_use) {
        (true, true) => "browser,computer",
        (true, false) => "browser",
        (false, true) => "computer",
        (false, false) => return None,
    };
    let program = program()?;
    let mut args = vec!["mcp", "--name", SERVER, "--tools", tools, "--session", thread];
    if local {
        args.extend(["--profile", "small"]);
    }
    let env: Map<String, Value> =
        PASSED.iter().filter_map(|name| Some(((*name).to_string(), json!(std::env::var(name).ok()?)))).collect();
    Some(json!({
        "type": "stdio",
        "name": SERVER,
        "command": program.to_str()?,
        "args": args,
        "cwd": program.parent().and_then(Path::to_str)?,
        "env": env,
    }))
}
