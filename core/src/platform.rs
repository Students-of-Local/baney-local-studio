use std::ffi::OsString;
use std::fs::{DirBuilder, Metadata, OpenOptions};
use std::path::{Path, PathBuf};

pub(crate) trait Mode {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl Mode for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
}

#[cfg(unix)]
impl Mode for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::DirBuilderExt::mode(self, mode)
    }
}

#[cfg(windows)]
impl Mode for OpenOptions {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

#[cfg(windows)]
impl Mode for DirBuilder {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub(crate) fn set_mode(path: &Path, _mode: u32) -> std::io::Result<()> {
    std::fs::metadata(path).map(|_| ())
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn link_count(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.nlink())
}

#[cfg(windows)]
pub(crate) fn link_count(_metadata: &Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
pub(crate) fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
pub(crate) fn is_executable(_metadata: &Metadata) -> bool {
    false
}

#[cfg(unix)]
pub(crate) fn symlink(link: &Path, at: &Path, _to_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(windows)]
pub(crate) fn symlink(link: &Path, at: &Path, to_dir: bool) -> std::io::Result<()> {
    if to_dir {
        std::os::windows::fs::symlink_dir(link, at)
    } else {
        std::os::windows::fs::symlink_file(link, at)
    }
}

#[cfg(unix)]
pub(crate) const ROOT: &str = "/";
#[cfg(windows)]
pub(crate) const ROOT: &str = "\\";

#[cfg(unix)]
pub(crate) const HOME_VAR: &str = "HOME";
#[cfg(windows)]
pub(crate) const HOME_VAR: &str = "USERPROFILE";

pub(crate) fn home() -> Option<OsString> {
    std::env::var_os(HOME_VAR)
}

pub(crate) fn home_or_root() -> String {
    std::env::var(HOME_VAR).unwrap_or_else(|_| ROOT.to_string())
}

#[cfg(windows)]
pub(crate) fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()).map(PathBuf::from)
}

#[cfg(unix)]
pub(crate) fn extra_search_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let user = std::env::var("USER").unwrap_or_default();
    let mut extra = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
        PathBuf::from(format!("/etc/profiles/per-user/{user}/bin")),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    if let Some(home) = home {
        extra.push(home.join(".nix-profile/bin"));
        extra.push(home.join(".bun/bin"));
        extra.push(home.join(".local/bin"));
        extra.push(home.join(".local/share/mise/shims"));
        extra.push(home.join(".local/share/mise/installs/bun/latest/bin"));
    }
    extra
}

#[cfg(windows)]
pub(crate) fn extra_search_paths() -> Vec<PathBuf> {
    let mut extra = Vec::new();
    if let Some(home) = home().filter(|value| !value.is_empty()).map(PathBuf::from) {
        extra.push(home.join(".bun").join("bin"));
        extra.push(home.join(".local").join("bin"));
    }
    if let Some(local) = local_app_data() {
        extra.push(local.join("mise").join("shims"));
    }
    extra
}

#[cfg(unix)]
pub(crate) fn executable(candidate: PathBuf) -> Option<PathBuf> {
    candidate.is_file().then_some(candidate)
}

#[cfg(windows)]
fn path_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|extension| extension.starts_with('.') && extension.len() > 1)
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(windows)]
pub(crate) fn executable(candidate: PathBuf) -> Option<PathBuf> {
    let extensions = path_extensions();
    let listed = candidate
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.iter().any(|known| known[1..].eq_ignore_ascii_case(extension)));
    if listed && candidate.is_file() {
        return Some(candidate);
    }
    extensions.iter().find_map(|extension| {
        let mut name = candidate.clone().into_os_string();
        name.push(extension);
        let path = PathBuf::from(name);
        path.is_file().then_some(path)
    })
}
