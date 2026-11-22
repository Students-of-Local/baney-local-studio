use std::path::{Path, PathBuf};

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("studio.lock")
}

pub fn recorded(data_dir: &Path) -> Option<u32> {
    std::fs::read_to_string(path(data_dir)).ok()?.trim().parse().ok()
}

pub enum Owner {
    Free,
    Held(Option<u32>),
}

pub fn holder(data_dir: &Path) -> Owner {
    let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(path(data_dir)) else { return Owner::Free };
    if matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)) {
        Owner::Held(recorded(data_dir))
    } else {
        Owner::Free
    }
}

pub fn describe(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => match local_studio_core::process_name(pid) {
            Some(name) => format!("process {pid} ({name}, {})", kind(&name)),
            None => format!("process {pid}"),
        },
        None => "another process".to_string(),
    }
}

pub fn kind(name: &str) -> &'static str {
    match name {
        "local-studio-serve" => "serve",
        "LocalStudio" | "local-studio" => "the desktop app",
        _ => "another Local Studio",
    }
}

pub fn kind_id(name: &str) -> &'static str {
    match name {
        "local-studio-serve" => "serve",
        "LocalStudio" | "local-studio" => "app",
        _ => "other",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listening {
    pub pid: u32,
    pub port: u16,
    pub tunnel_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_tunnel_port: Option<u16>,
}

pub fn listening_path(data_dir: &Path) -> PathBuf {
    data_dir.join("remote").join("listening.json")
}

pub fn record_listening(data_dir: &Path, listening: &Listening) -> anyhow::Result<()> {
    let text = serde_json::to_vec(listening)?;
    crate::devices::write_private(&listening_path(data_dir), &text)
}

fn read_listening(data_dir: &Path) -> Option<Listening> {
    serde_json::from_slice(&std::fs::read(listening_path(data_dir)).ok()?).ok()
}

pub fn listening(data_dir: &Path, owner: Option<u32>) -> Option<Listening> {
    read_listening(data_dir).filter(|listening| Some(listening.pid) == owner)
}

pub fn clear_listening(data_dir: &Path) {
    if read_listening(data_dir).is_some_and(|listening| listening.pid == std::process::id()) {
        std::fs::remove_file(listening_path(data_dir)).ok();
    }
}
