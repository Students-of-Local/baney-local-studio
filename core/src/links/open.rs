use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use super::studio::{file_name, link_words, section_title, title_in, StudioLink, EXTENSION_SECTIONS, PANES};
use crate::studio::Studio;
use crate::{platform, store};

const EXIT_WAIT: Duration = Duration::from_secs(10);
const ENTRY_LIMIT: u64 = 1024 * 1024;
const OPEN: &str = "/usr/bin/open";
const EXPLORER: &str = r"C:\Windows\explorer.exe";
const DOCUMENTS: &str = "pdf txt md csv tsv log rtf doc docx xls xlsx ppt pptx odt ods odp pages numbers key epub \
    png jpg jpeg gif webp heic mp3 m4a wav flac mp4 mov mkv webm ics vcf json yaml toml";
const ZIPPED: &str = "docx xlsx pptx odt ods odp pages numbers key epub";
const RUNS: &[&[u8]] = &[
    b"#!",
    b"MZ",
    b"\x7fELF",
    b"\xfe\xed\xfa",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"book\0\0\0\0mark",
    b"L\0\0\0\x01\x14\x02",
];
const OLD_OFFICE: &str = "doc xls ppt";
const OLE: &[u8] = b"\xd0\xcf\x11\xe0";
const INSTALLER: &[u8] = b"\x10\x0c\0\0\0\0\0\xc0\0\0\0\0\0\0\x46";

const PANE_IDS: &[(&str, &str, &str, &str, &str)] = &[
    ("general", "systempreferences.GeneralSettings", "system", "", ""),
    ("about", "SystemProfiler.AboutExtension", "system about", "kcm_about-distro", "about"),
    ("software-update", "Software-Update-Settings.extension", "", "kcm_updates", "windowsupdate"),
    ("storage", "settings.Storage", "", "", "storagesense"),
    ("appearance", "Appearance-Settings.extension", "background", "kcm_lookandfeel", "personalization"),
    ("accessibility", "Accessibility-Settings.extension", "universal-access", "kcm_access", "easeofaccess"),
    ("displays", "Displays-Settings.extension", "display", "kcm_kscreen", "display"),
    ("sound", "Sound-Settings.extension", "sound", "kcm_pulseaudio", "sound"),
    ("wifi", "wifi-settings-extension", "wifi", "kcm_networkmanagement", "network-wifi"),
    ("bluetooth", "BluetoothSettings", "bluetooth", "kcm_bluetooth", "bluetooth"),
    ("network", "Network-Settings.extension", "network", "kcm_networkmanagement", "network-status"),
    ("vpn", "NetworkExtensionSettingsUI.NESettingsUIExtension", "network", "kcm_networkmanagement", "network-vpn"),
    ("notifications", "Notifications-Settings.extension", "notifications", "kcm_notifications", "notifications"),
    ("focus", "Focus-Settings.extension", "", "", "quiethours"),
    ("screen-time", "Screen-Time-Settings.extension", "wellbeing", "", ""),
    ("lock-screen", "Lock-Screen-Settings.extension", "privacy", "kcm_screenlocker", "lockscreen"),
    ("privacy", "settings.PrivacySecurity.extension", "privacy", "", "privacy"),
    ("screen-recording", "preference.security?Privacy_ScreenCapture", "", "", "privacy-graphicscaptureprogrammatic"),
    ("accessibility-access", "preference.security?Privacy_Accessibility", "", "", ""),
    ("microphone", "preference.security?Privacy_Microphone", "privacy", "", "privacy-microphone"),
    ("camera", "preference.security?Privacy_Camera", "privacy", "", "privacy-webcam"),
    ("full-disk-access", "preference.security?Privacy_AllFiles", "", "", "privacy-broadfilesystemaccess"),
    ("login-items", "LoginItems-Settings.extension", "", "kcm_autostart", "startupapps"),
    ("users", "Users-Groups-Settings.extension", "system users", "kcm_users", "otherusers"),
    ("passwords", "Passwords-Settings.extension", "", "", ""),
    ("keyboard", "Keyboard-Settings.extension", "keyboard", "kcm_keyboard", "typing"),
    ("mouse", "Mouse-Settings.extension", "mouse", "kcm_mouse", "mousetouchpad"),
    ("trackpad", "Trackpad-Settings.extension", "mouse", "kcm_touchpad", "devices-touchpad"),
    ("printers", "Print-Scan-Settings.extension", "printers", "kcm_printer_manager", "printers"),
    ("battery", "Battery-Settings.extension", "power", "kcm_powerdevilprofilesconfig", "powersleep"),
    ("desktop-dock", "Desktop-Settings.extension", "", "", "taskbar"),
    ("wallpaper", "Wallpaper-Settings.extension", "background", "kcm_wallpaper", "personalization-background"),
    ("date-time", "Date-Time-Settings.extension", "system datetime", "kcm_clock", "dateandtime"),
    ("language", "Localization-Settings.extension", "system region", "kcm_regionandlang", "regionlanguage"),
    ("sharing", "Sharing-Settings.extension", "sharing", "", ""),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Linux,
    Windows,
}

impl Platform {
    #[must_use]
    pub fn here() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Mac,
            "windows" => Self::Windows,
            _ => Self::Linux,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LinkEnv {
    pub path: OsString,
    pub home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub desktop: String,
}

impl LinkEnv {
    #[must_use]
    pub fn here() -> Self {
        let set = |name| std::env::var_os(name).filter(|value| !value.is_empty());
        let home = store::home_dir().unwrap_or_else(|| PathBuf::from(platform::ROOT));
        let own = set("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from);
        let shared = set("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let dirs = std::iter::once(own).chain(std::env::split_paths(&shared)).filter(|dir| dir.is_absolute());
        let desktop = set("XDG_CURRENT_DESKTOP").unwrap_or_default().to_string_lossy().into_owned();
        Self { path: set("PATH").unwrap_or_default(), home, data_dirs: dirs.collect(), desktop }
    }
}

#[doc(hidden)]
pub fn link_command(link: &StudioLink, on: Platform, env: &LinkEnv) -> Result<Vec<OsString>, String> {
    match link {
        StudioLink::Settings { section } if EXTENSION_SECTIONS.iter().any(|(id, _)| id == section) => {
            let title = section_title(section);
            Err(format!("{title} is in the extension's settings: open the side panel, then Settings."))
        }
        StudioLink::Settings { .. } | StudioLink::Thread { .. } => Err("Local Studio opens this itself.".to_string()),
        StudioLink::Browser { .. } => Err("Browser pages open from a link in the side panel.".to_string()),
        StudioLink::System { pane } => system(pane, on, env),
        StudioLink::App { name } => app(name, on, env),
        StudioLink::Reveal { path } => reveal(path, on, env),
        StudioLink::File { path } => file(path, on, env),
        StudioLink::Shortcut { name } if on == Platform::Mac => {
            Ok(vec!["/usr/bin/shortcuts".into(), "run".into(), name.into()])
        }
        StudioLink::Shortcut { .. } => Err("Shortcuts run on a Mac.".to_string()),
    }
}

fn system(pane: &str, on: Platform, env: &LinkEnv) -> Result<Vec<OsString>, String> {
    let (_, mac, gnome, kde, windows) =
        PANE_IDS.iter().find(|(id, ..)| *id == pane).copied().unwrap_or((pane, "", "", "", ""));
    let (program, panel, scheme): (OsString, _, _) = match on {
        Platform::Mac => (OPEN.into(), mac, "x-apple.systempreferences:com.apple."),
        Platform::Windows => (EXPLORER.into(), windows, "ms-settings:"),
        Platform::Linux => {
            let on_gnome = env.desktop.is_empty()
                || env.desktop.split(':').any(|name| ["GNOME", "Unity"].iter().any(|g| name.eq_ignore_ascii_case(g)));
            let found = [("gnome-control-center", gnome), ("systemsettings", kde)];
            let mut found = found.into_iter().skip(usize::from(!on_gnome));
            found
                .find_map(|(name, panel)| Some((program(name, env).ok()?, panel, "")))
                .ok_or_else(|| "This computer has no settings program Local Studio knows.".to_string())?
        }
    };
    if panel.is_empty() {
        return Err(format!("{} has no settings page on this computer.", title_in(PANES, pane)));
    }
    let args = panel.split(' ').map(|part| format!("{scheme}{part}").into());
    Ok(std::iter::once(program).chain(args).collect())
}

fn file(given: &str, on: Platform, env: &LinkEnv) -> Result<Vec<OsString>, String> {
    let path = expand(given, env).canonicalize().map_err(|_| missing(given))?;
    if on == Platform::Windows {
        return Err("Opening files from a link is not there yet on Windows.".to_string());
    }
    if !document(&path, on) {
        return Err("That kind of file doesn't open from a link. Show it instead.".to_string());
    }
    let opener = if on == Platform::Mac { OPEN.into() } else { program("xdg-open", env)? };
    Ok(vec![opener, path.into()])
}

fn document(path: &Path, on: Platform) -> bool {
    let Ok(metadata) = fs::metadata(path) else { return false };
    if metadata.is_dir() {
        let plain = path.file_name().is_some_and(|name| !name.to_string_lossy().contains('.'));
        let bundle = || ["Info.plist", "Contents"].iter().any(|part| fs::symlink_metadata(path.join(part)).is_ok());
        return on != Platform::Mac || (plain && !bundle());
    }
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or_default();
    let listed = |list: &str| list.split(' ').any(|known| known.eq_ignore_ascii_case(extension));
    metadata.is_file() && listed(DOCUMENTS) && starts_as_document(path, listed(ZIPPED), listed(OLD_OFFICE))
}

fn starts_as_document(path: &Path, zipped: bool, old_office: bool) -> bool {
    let mut start = Vec::new();
    let Ok(mut file) = fs::File::open(path) else { return false };
    if file.by_ref().take(64).read_to_end(&mut start).is_err() || RUNS.iter().any(|runs| start.starts_with(runs)) {
        return false;
    }
    if start.starts_with(b"PK\x03\x04") {
        return zipped && !start.get(30..).unwrap_or_default().starts_with(b"META-INF");
    }
    !start.starts_with(OLE) || (old_office && !installer(&mut file, &start).unwrap_or(true))
}

fn installer(file: &mut fs::File, start: &[u8]) -> Option<bool> {
    let shift = start.get(30).copied().filter(|shift| [9, 12].contains(shift))?;
    let root = u64::from(u32::from_le_bytes(start.get(48..52)?.try_into().ok()?));
    let mut kind = [0; 16];
    file.seek(SeekFrom::Start(((root + 1) << shift) + 80)).ok()?;
    file.read_exact(&mut kind).ok()?;
    Some(&kind[1..] == INSTALLER)
}

fn reveal(given: &str, on: Platform, env: &LinkEnv) -> Result<Vec<OsString>, String> {
    let path = expand(given, env);
    fs::symlink_metadata(&path).map_err(|_| missing(given))?;
    match on {
        Platform::Mac => Ok(vec![OPEN.into(), "-R".into(), path.into()]),
        Platform::Windows => Err("Showing files from a link is not there yet on Windows.".to_string()),
        Platform::Linux => {
            let Some(gdbus) = program("gdbus", env).ok().filter(|_| has_file_manager(env)) else {
                return Ok(vec![program("xdg-open", env)?, path.parent().unwrap_or(&path).into()]);
            };
            let call =
                "call --session --dest org.freedesktop.FileManager1 --object-path /org/freedesktop/FileManager1 \
                --method org.freedesktop.FileManager1.ShowItems";
            let items = [format!("['file://{}']", encoded(&path)).into(), OsString::new()];
            Ok(std::iter::once(gdbus).chain(call.split(' ').map(OsString::from)).chain(items).collect())
        }
    }
}

fn has_file_manager(env: &LinkEnv) -> bool {
    let files = env.data_dirs.iter().flat_map(|dir| fs::read_dir(dir.join("dbus-1/services")).into_iter().flatten());
    files.flatten().map(|entry| entry.path()).any(|file| {
        let named = file.file_name().is_some_and(|name| name.to_string_lossy().contains("FileManager1"));
        let offers = |text: String| text.lines().any(|line| line.trim() == "Name=org.freedesktop.FileManager1");
        named && fs::read_to_string(&file).is_ok_and(offers)
    })
}

fn encoded(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes().iter();
    let kept = |byte: u8| byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte);
    bytes.map(|&byte| if kept(byte) { char::from(byte).to_string() } else { format!("%{byte:02X}") }).collect()
}

fn app(name: &str, on: Platform, env: &LinkEnv) -> Result<Vec<OsString>, String> {
    match on {
        Platform::Mac => Ok(vec![OPEN.into(), "-a".into(), name.into()]),
        Platform::Windows => Err("Opening apps from a link is not there yet on Windows.".to_string()),
        Platform::Linux => {
            let stem = app_stem(name, env).ok_or_else(|| format!("There is no app named {name} here."))?;
            Ok(vec![program("gtk-launch", env)?, stem.into()])
        }
    }
}

fn app_stem(name: &str, env: &LinkEnv) -> Option<String> {
    let mut seen = HashSet::new();
    let mut named = None;
    for dir in &env.data_dirs {
        let dir = dir.join("applications");
        let mut files: Vec<PathBuf> =
            fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path()).collect();
        files.sort();
        for file in files {
            let stem = file.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_suffix(".desktop"));
            let Some(stem) = stem.filter(|stem| !stem.is_empty() && !stem.starts_with('-')) else { continue };
            let same = |text: &str| text.to_lowercase() == name.to_lowercase();
            let Some(shown) = seen.insert(stem.to_string()).then(|| app_name(&file)).flatten() else { continue };
            if same(stem) {
                return Some(stem.to_string());
            }
            if named.is_none() && same(&shown) {
                named = Some(stem.to_string());
            }
        }
    }
    named
}

fn app_name(file: &Path) -> Option<String> {
    fs::metadata(file).ok().filter(|metadata| metadata.is_file() && metadata.len() <= ENTRY_LIMIT)?;
    let text = fs::read_to_string(file).ok()?;
    let entry = text.lines().map(str::trim).skip_while(|line| *line != "[Desktop Entry]").skip(1);
    let pairs = entry.take_while(|line| !line.starts_with('[')).filter_map(|line| line.split_once('='));
    let value = |key| pairs.clone().find(|(known, _)| known.trim() == key).map(|(_, value)| value.trim());
    let runs =
        value("Type") == Some("Application") && ["Hidden", "NoDisplay"].iter().all(|key| value(key) != Some("true"));
    runs.then(|| value("Name").unwrap_or_default().to_string())
}

fn expand(given: &str, env: &LinkEnv) -> PathBuf {
    let path = given.trim_end_matches('/');
    let below = path.strip_prefix('~').filter(|below| below.is_empty() || below.starts_with('/'));
    below.map_or_else(|| PathBuf::from(path), |below| env.home.join(below.trim_start_matches('/')))
}

fn missing(given: &str) -> String {
    format!("{} is not there.", file_name(given))
}

fn program(name: &str, env: &LinkEnv) -> Result<OsString, String> {
    std::env::split_paths(&env.path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|file| fs::metadata(file).is_ok_and(|found| found.is_file() && platform::is_executable(&found)))
        .map(PathBuf::into_os_string)
        .ok_or_else(|| format!("This computer has no {name}, which Local Studio needs for that."))
}

fn failed(link: &StudioLink) -> String {
    let title = link_words(link.clone()).title;
    let (first, rest) = title.split_at(1);
    format!("Local Studio could not {}{rest}.", first.to_ascii_lowercase())
}

fn start(link: &StudioLink) -> Result<Child, String> {
    let plan = link_command(link, Platform::here(), &LinkEnv::here())?;
    let (program, args) = plan.split_first().ok_or_else(|| failed(link))?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    platform::own_group_async(&mut command);
    command.spawn().map_err(|_| failed(link))
}

async fn run(link: StudioLink) -> Result<(), String> {
    let failed = failed(&link);
    let started = tokio::task::spawn_blocking(move || start(&link)).await;
    let mut child = started.unwrap_or_else(|_| Err(failed.clone()))?;
    match tokio::time::timeout(EXIT_WAIT, child.wait()).await {
        Ok(Ok(status)) if status.success() || cfg!(windows) => Ok(()),
        Ok(_) => Err(failed),
        Err(_) => child.wait().await.map(|_| ()).map_err(|_| failed),
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Studio {
    pub fn open_studio_link(&self, link: StudioLink) {
        self.workspace.warn_if(run(link));
    }
}
