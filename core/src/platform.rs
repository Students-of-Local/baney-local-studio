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

#[cfg(all(unix, feature = "link"))]
pub(crate) fn mode(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(all(windows, feature = "link"))]
pub(crate) fn mode(_metadata: &Metadata) -> u32 {
    0o600
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub(crate) fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if mode.trailing_zeros() >= 6 {
        owner_only(path, metadata.is_dir())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn owner_only(path: &Path, dir: bool) -> std::io::Result<()> {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|user| !user.is_empty())
        .ok_or_else(|| std::io::Error::other("USERNAME is not set, so the owner-only ACL cannot be set"))?;
    let account = match std::env::var("USERDOMAIN").ok().filter(|domain| !domain.is_empty()) {
        Some(domain) => format!("{domain}\\{user}"),
        None => user,
    };
    let grant = if dir { format!("{account}:(OI)(CI)F") } else { format!("{account}:F") };
    let status = std::process::Command::new(system32("icacls.exe"))
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &grant, "/Q"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("icacls could not make {} private to {account} ({status})", path.display())))
    }
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

#[cfg(windows)]
fn system32(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(name), |root| PathBuf::from(root).join("System32").join(name))
}

#[cfg(windows)]
pub(crate) fn taskkill(pid: u32, tree: bool) {
    let mut command = std::process::Command::new(system32("taskkill.exe"));
    command.args(["/PID", &pid.to_string(), "/F"]);
    if tree {
        command.arg("/T");
    }
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(windows)]
pub(crate) fn command_line(pid: u32) -> Option<String> {
    let query = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine");
    let output = std::process::Command::new(system32("WindowsPowerShell\\v1.0\\powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", &query])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !line.is_empty()).then_some(line)
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[cfg(unix)]
pub(crate) fn own_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
pub(crate) fn own_group(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
pub(crate) fn own_group_async(command: &mut tokio::process::Command) {
    command.process_group(0);
}

#[cfg(windows)]
pub(crate) fn own_group_async(command: &mut tokio::process::Command) {
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(windows)]
pub(crate) fn command_interpreter() -> PathBuf {
    std::env::var_os("COMSPEC")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| system32("cmd.exe"))
}

#[cfg(windows)]
pub(crate) fn cmd_script(script: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(command_interpreter());
    let lines: Vec<&str> = script.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    command.args(["/D", "/S", "/C"]).raw_arg(format!("\"{}\"", lines.join(" & ")));
    command
}

#[cfg(unix)]
pub(crate) fn fill_random(bytes: &mut [u8]) -> bool {
    std::fs::File::open("/dev/urandom").and_then(|mut file| std::io::Read::read_exact(&mut file, bytes)).is_ok()
}

#[cfg(windows)]
pub(crate) fn fill_random(bytes: &mut [u8]) -> bool {
    getrandom::getrandom(bytes).is_ok()
}
