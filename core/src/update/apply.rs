use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{channel_name, channel_page, Build, Refusal, Staged};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub target: PathBuf,
    pub previous: PathBuf,
    pub version: String,
}

pub fn apply(build: &Build, staged: &Staged, target: &Path) -> Result<Applied, Refusal> {
    if let Some(refusal) = build.refusal() {
        return Err(refusal);
    }
    if !super::newer(build, &staged.version)? {
        return Err(Refusal::NotNewer { feed: staged.version.clone(), build: build.version.clone() });
    }
    let ready = std::fs::read(staged.dir.join(super::READY_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<super::ReadyFile>(&bytes).ok())
        .ok_or_else(|| Refusal::Apply("the staged download has no ready.json".to_string()))?;
    if ready.version != staged.version || ready.channel != build.channel {
        return Err(Refusal::Apply("the staged download does not match its ready.json".to_string()));
    }
    super::verify_file(build, &staged.artifact, &ready.sha256, &ready.signature, &ready.asset)?;
    let unpacked = staged.dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked)
        .map_err(|error| Refusal::Apply(format!("create {}: {error}", unpacked.display())))?;
    let result = install(build, staged, target, &unpacked);
    let _ = std::fs::remove_dir_all(&unpacked);
    result
}

fn install(build: &Build, staged: &Staged, target: &Path, unpacked: &Path) -> Result<Applied, Refusal> {
    if !target.exists() {
        return Err(Refusal::Apply(format!(
            "nothing is installed at {}; install it with scripts/install.sh",
            target.display()
        )));
    }
    run(Command::new("tar").arg("-xzf").arg(&staged.artifact).arg("-C").arg(unpacked))
        .map_err(|error| Refusal::BadBundle(format!("could not unpack it: {error}")))?;
    let parent = target.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Refusal::Apply(format!("{} names no install", target.display())))?;
    let incoming = parent.join(format!(".{name}.incoming"));
    let previous = parent.join(format!(".{name}.previous"));
    let _ = std::fs::remove_dir_all(&incoming);
    let outcome = platform_install(build, staged, target, unpacked, &incoming, &previous);
    let _ = std::fs::remove_dir_all(&incoming);
    outcome.map(|()| Applied { target: target.to_path_buf(), previous, version: staged.version.clone() })
}

#[cfg(target_os = "macos")]
fn platform_install(
    build: &Build,
    staged: &Staged,
    target: &Path,
    unpacked: &Path,
    incoming: &Path,
    previous: &Path,
) -> Result<(), Refusal> {
    let requirement = designated_requirement(target)?;
    let identifier = plist(target, "CFBundleIdentifier").map_err(Refusal::Apply)?;
    let app = only_entry(unpacked, |path| path.extension().is_some_and(|ext| ext == "app"))?;
    check_bundle(&app, build, &staged.version, &identifier)?;
    codesign_verify(&app, &requirement)?;
    run(Command::new("/usr/bin/ditto").arg(&app).arg(incoming)).map_err(Refusal::Apply)?;
    let _ = Command::new("/usr/bin/xattr").arg("-dr").arg("com.apple.quarantine").arg(incoming).output();
    codesign_verify(incoming, &requirement)?;
    swap(target, incoming, previous)?;
    let health = codesign_verify(target, &requirement)
        .map_err(|refusal| refusal.to_string())
        .and_then(|()| reports_version(&executable(target)?, build, &staged.version));
    if let Err(reason) = health {
        return Err(rollback(target, previous, reason));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_install(
    build: &Build,
    staged: &Staged,
    target: &Path,
    unpacked: &Path,
    incoming: &Path,
    previous: &Path,
) -> Result<(), Refusal> {
    if !target.join("local-studio").is_file() {
        return Err(Refusal::Apply(format!("{} holds no local-studio binary", target.display())));
    }
    let bundle = only_entry(unpacked, Path::is_dir)?;
    reports_version(&bundle.join("local-studio"), build, &staged.version).map_err(Refusal::BadBundle)?;
    run(Command::new("cp").arg("-a").arg(&bundle).arg(incoming)).map_err(Refusal::Apply)?;
    swap(target, incoming, previous)?;
    if let Err(reason) = reports_version(&target.join("local-studio"), build, &staged.version) {
        return Err(rollback(target, previous, reason));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_install(
    _build: &Build,
    _staged: &Staged,
    _target: &Path,
    _unpacked: &Path,
    _incoming: &Path,
    _previous: &Path,
) -> Result<(), Refusal> {
    Err(Refusal::Unsupported("this platform installs updates by hand".to_string()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn only_entry(dir: &Path, wanted: impl Fn(&Path) -> bool) -> Result<PathBuf, Refusal> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| Refusal::BadBundle(format!("read {}: {error}", dir.display())))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("._")))
        .collect();
    match entries.as_slice() {
        [only] if wanted(only) => Ok(only.clone()),
        _ => {
            Err(Refusal::BadBundle(format!("the archive should hold one app, and it holds {} entries", entries.len())))
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn swap(target: &Path, incoming: &Path, previous: &Path) -> Result<(), Refusal> {
    if previous.exists() {
        std::fs::remove_dir_all(previous)
            .map_err(|error| Refusal::Apply(format!("remove {}: {error}", previous.display())))?;
    }
    std::fs::rename(target, previous)
        .map_err(|error| Refusal::Apply(format!("move {} aside: {error}", target.display())))?;
    if let Err(error) = std::fs::rename(incoming, target) {
        return match std::fs::rename(previous, target) {
            Ok(()) => Err(Refusal::Apply(format!("move the new build into place: {error}; the previous build is back"))),
            Err(back) => Err(Refusal::Apply(format!(
                "move the new build into place: {error}; restoring the previous build also failed ({back}), it is at {}",
                previous.display()
            ))),
        };
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rollback(target: &Path, previous: &Path, reason: String) -> Refusal {
    let failed = previous.with_extension("failed");
    let _ = std::fs::remove_dir_all(&failed);
    if let Err(error) = std::fs::rename(target, &failed) {
        return Refusal::Apply(format!("{reason}; moving the failed build aside also failed: {error}"));
    }
    match std::fs::rename(previous, target) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&failed);
            Refusal::RolledBack(reason)
        }
        Err(error) => Refusal::Apply(format!(
            "{reason}; restoring the previous build from {} failed: {error}",
            previous.display()
        )),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reports_version(program: &Path, build: &Build, version: &str) -> Result<(), String> {
    let want = format!("local-studio {version} ({})", build.channel);
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", program.display()))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < HEALTH_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} --version did not answer within {} s",
                    program.display(),
                    HEALTH_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(format!("{} --version: {error}", program.display())),
        }
    };
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut stdout, &mut output);
    }
    let first = output.lines().next().unwrap_or_default().trim();
    if !status.success() {
        return Err(format!("{} --version exited with {status}", program.display()));
    }
    if first != want {
        return Err(format!("{} reports \"{first}\", expected \"{want}\"", program.display()));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_bundle(app: &Path, build: &Build, version: &str, identifier: &str) -> Result<(), Refusal> {
    let got = plist(app, "CFBundleIdentifier").map_err(Refusal::BadBundle)?;
    if got != identifier {
        return Err(Refusal::BadBundle(format!("it is {got}, and the installed app is {identifier}")));
    }
    let channel = plist(app, "LocalStudioChannel").map_err(Refusal::BadBundle)?;
    if channel != build.channel {
        return Err(Refusal::OtherChannel { feed: channel, build: build.channel.clone() });
    }
    let stamped = plist(app, "LocalStudioVersion").map_err(Refusal::BadBundle)?;
    if stamped != version {
        return Err(Refusal::BadBundle(format!("it carries version {stamped}, and the feed said {version}")));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn plist(app: &Path, key: &str) -> Result<String, String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(app.join("Contents").join("Info.plist"))
        .output()
        .map_err(|error| format!("plutil: {error}"))?;
    if !output.status.success() {
        return Err(format!("{} has no {key} in its Info.plist", app.display()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "macos")]
fn executable(app: &Path) -> Result<PathBuf, String> {
    let name = plist(app, "CFBundleExecutable")?;
    Ok(app.join("Contents").join("MacOS").join(name))
}

#[cfg(target_os = "macos")]
fn designated_requirement(app: &Path) -> Result<String, Refusal> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(app)
        .output()
        .map_err(|error| Refusal::Apply(format!("codesign: {error}")))?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let requirement = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("designated => "))
        .map(str::to_string)
        .filter(|_| output.status.success());
    match requirement {
        Some(requirement) if requirement.contains("certificate") && !requirement.starts_with("cdhash") => {
            Ok(requirement)
        }
        Some(_) => Err(Refusal::Apply(format!(
            "{} is signed ad hoc, so there is no signing identity a new build could match",
            app.display()
        ))),
        None => Err(Refusal::Apply(format!("{} is not code signed", app.display()))),
    }
}

#[cfg(target_os = "macos")]
fn codesign_verify(app: &Path, requirement: &str) -> Result<(), Refusal> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "-R"])
        .arg(format!("={requirement}"))
        .arg(app)
        .output()
        .map_err(|error| Refusal::Apply(format!("codesign: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or_default().trim().to_string();
    Err(Refusal::BadBundle(format!(
        "its code signature does not hold or is not the installed app's identity ({detail})"
    )))
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command.stdin(Stdio::null()).output().map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() { format!("exited with {}", output.status) } else { detail })
}

pub fn current_install() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("could not find this app: {error}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if cfg!(target_os = "macos") {
        return exe
            .ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| "this build is not running from an app bundle".to_string());
    }
    let dir = exe.parent().ok_or_else(|| "this build has no install folder".to_string())?;
    if dir.join("eukhe").is_file() && dir.join("local-studio").is_file() {
        return Ok(dir.to_path_buf());
    }
    Err(format!("this build is not running from an installed bundle ({})", dir.display()))
}

#[must_use]
pub fn install_path(channel: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from("/Applications").join(format!("{}.app", channel_name(channel))));
    }
    if cfg!(target_os = "linux") {
        let slug = match channel {
            "stable" => "local-studio",
            "nightly" => "local-studio-nightly",
            _ => "local-studio-dev",
        };
        return crate::store::home_dir().map(|home| home.join(".local").join("lib").join(slug));
    }
    None
}

pub fn relaunch(target: &Path) -> Result<(), String> {
    let script = if cfg!(target_os = "macos") {
        r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#
    } else if cfg!(target_os = "linux") {
        r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2/local-studio""#
    } else {
        return Err("this platform restarts by hand".to_string());
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("local-studio-relaunch")
        .arg(std::process::id().to_string())
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::own_group(&mut command);
    command.spawn().map(drop).map_err(|error| format!("could not schedule the restart: {error}"))
}

pub fn open_channel(channel: &str) -> Result<String, String> {
    if !super::CHANNELS.contains(&channel) {
        return Err(format!("{channel} is not a release channel"));
    }
    let installed = install_path(channel).filter(|path| path.exists());
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/open");
        match &installed {
            Some(path) => command.arg(path),
            None => command.arg(channel_page(channel)),
        };
        command
    } else if let Some(path) = &installed {
        Command::new(path.join("local-studio"))
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(channel_page(channel));
        command
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::own_group(&mut command);
    command.spawn().map_err(|error| format!("could not open {}: {error}", channel_name(channel)))?;
    Ok(match installed {
        Some(path) => format!("Opened {}", path.display()),
        None => format!("Opened {}", channel_page(channel)),
    })
}
