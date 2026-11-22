use std::path::{Path, PathBuf};

use serde_json::Value;

pub const DEFAULT_CONTROLLER_PORT: u16 = 18091;

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

fn read(path: &Path) -> Value {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null)
}

#[must_use]
pub fn settings(data_dir: &Path) -> Value {
    read(&data_dir.join("settings.json"))
}

#[must_use]
pub fn controller_url(data_dir: &Path) -> String {
    std::env::var("LOCAL_STUDIO_CONTROLLER_URL")
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .or_else(|| text(&settings(data_dir), "localControllerUrl"))
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_CONTROLLER_PORT}"))
        .trim_end_matches('/')
        .to_string()
}

#[must_use]
pub fn controller_port() -> u16 {
    std::env::var("LOCAL_STUDIO_CONTROLLER_PORT")
        .ok()
        .and_then(|port| port.trim().parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_CONTROLLER_PORT)
}

#[must_use]
pub fn key(data_dir: &Path) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let homes = [
        std::env::var_os("LOCAL_STUDIO_CONTROLLER_HOME").filter(|home| !home.is_empty()).map(PathBuf::from),
        Some(data_dir.join("controller")),
        home.as_ref().map(|home| home.join(".local-studio-controller")),
        home.as_ref().map(|home| home.join(".local-studio-t3")),
    ];
    text(&settings(data_dir), "fleetKey")
        .or_else(|| homes.into_iter().flatten().find_map(|home| text(&read(&home.join("config.json")), "fleetKey")))
}
