use super::hidden;

const NAME_LIMIT: usize = 100;
const PATH_LIMIT: usize = 1024;

pub(super) const APP_SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("appearance", "Appearance"),
    ("keybindings", "Keybindings"),
    ("snapshots", "SnapShots"),
    ("providers", "Providers"),
    ("local", "Local"),
    ("connections", "Connections"),
    ("plugins", "Plugins"),
    ("project", "Project"),
    ("source-control", "Source Control"),
    ("storage", "Storage"),
    ("archive", "Archive"),
];

pub(super) const EXTENSION_SECTIONS: &[(&str, &str)] = &[
    ("agents", "Agents"),
    ("skills", "Skills"),
    ("sites", "Sites"),
    ("dictation", "Dictation"),
    ("privacy", "Privacy"),
    ("costs", "Costs"),
    ("about", "About"),
];

const PAGES: &[(&str, &str)] = &[
    ("extensions", "Extensions"),
    ("shortcuts", "Shortcuts"),
    ("downloads", "Downloads"),
    ("history", "History"),
    ("passwords", "Passwords"),
    ("privacy", "Privacy"),
    ("appearance", "Appearance"),
    ("search", "Search"),
    ("languages", "Languages"),
    ("clear-data", "Clear Data"),
    ("site-settings", "Site Settings"),
    ("about", "About"),
];

pub(super) const PANES: &[(&str, &str)] = &[
    ("general", "General"),
    ("about", "About"),
    ("software-update", "Software Update"),
    ("storage", "Storage"),
    ("appearance", "Appearance"),
    ("accessibility", "Accessibility"),
    ("displays", "Displays"),
    ("sound", "Sound"),
    ("wifi", "Wi-Fi"),
    ("bluetooth", "Bluetooth"),
    ("network", "Network"),
    ("vpn", "VPN"),
    ("notifications", "Notifications"),
    ("focus", "Focus"),
    ("screen-time", "Screen Time"),
    ("lock-screen", "Lock Screen"),
    ("privacy", "Privacy"),
    ("screen-recording", "Screen Recording"),
    ("accessibility-access", "Accessibility Access"),
    ("microphone", "Microphone"),
    ("camera", "Camera"),
    ("full-disk-access", "Full Disk Access"),
    ("login-items", "Login Items"),
    ("users", "Users"),
    ("passwords", "Passwords"),
    ("keyboard", "Keyboard"),
    ("mouse", "Mouse"),
    ("trackpad", "Trackpad"),
    ("printers", "Printers"),
    ("battery", "Battery"),
    ("desktop-dock", "Desktop and Dock"),
    ("wallpaper", "Wallpaper"),
    ("date-time", "Date and Time"),
    ("language", "Language"),
    ("sharing", "Sharing"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum StudioLink {
    Settings { section: String },
    Thread { id: String },
    Browser { page: String },
    System { pane: String },
    App { name: String },
    Reveal { path: String },
    File { path: String },
    Shortcut { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LinkWords {
    pub title: String,
    pub detail: String,
    pub confirm: bool,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn studio_link(url: &str) -> Option<StudioLink> {
    let rest = url.strip_prefix("localstudio://")?;
    let (kind, tail) = rest.split_at(rest.find(['/', '?'])?);
    match kind {
        "reveal" => Some(StudioLink::Reveal { path: path(tail)? }),
        "file" => Some(StudioLink::File { path: path(tail)? }),
        "thread" => Some(StudioLink::Thread { id: thread(tail)? }),
        "app" => Some(StudioLink::App { name: name(tail)? }),
        "shortcut" => Some(StudioLink::Shortcut { name: name(tail)? }),
        "browser" => Some(StudioLink::Browser { page: one_of(PAGES, &name(tail)?)? }),
        "system" => Some(StudioLink::System { pane: one_of(PANES, &name(tail)?)? }),
        "settings" => {
            let section = name(tail)?;
            let section = one_of(APP_SECTIONS, &section).or_else(|| one_of(EXTENSION_SECTIONS, &section))?;
            Some(StudioLink::Settings { section })
        }
        _ => None,
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn outside_link(url: &str) -> Option<StudioLink> {
    studio_link(url).filter(|link| match link {
        StudioLink::Settings { section } => one_of(APP_SECTIONS, section).is_some(),
        StudioLink::Thread { .. } => true,
        _ => false,
    })
}

#[cfg_attr(feature = "ffi", uniffi::export)]
#[must_use]
pub fn link_words(link: StudioLink) -> LinkWords {
    let confirm = matches!(link, StudioLink::Shortcut { .. });
    let manager = if cfg!(target_os = "macos") { "Finder" } else { "Files" };
    let (title, detail) = match link {
        StudioLink::Settings { section } => (format!("Open {} settings", section_title(&section)), section),
        StudioLink::Thread { id } => ("Open the thread".to_string(), id),
        StudioLink::Browser { page } => (format!("Open the browser's {} page", title_in(PAGES, &page)), page),
        StudioLink::System { pane } => (format!("Open {} settings on this computer", title_in(PANES, &pane)), pane),
        StudioLink::App { name } => (format!("Open {name}"), name),
        StudioLink::Reveal { path } => (format!("Show {} in {manager}", file_name(&path)), path),
        StudioLink::File { path } => (format!("Open {}", file_name(&path)), path),
        StudioLink::Shortcut { name } => (format!("Run the shortcut \u{201C}{name}\u{201D}"), name),
    };
    LinkWords { title, detail, confirm }
}

pub(super) fn title_in<'a>(table: &[(&str, &'a str)], id: &'a str) -> &'a str {
    table.iter().find(|(known, _)| *known == id).map_or(id, |(_, title)| title)
}

pub(super) fn section_title(id: &str) -> &str {
    title_in(if one_of(APP_SECTIONS, id).is_some() { APP_SECTIONS } else { EXTENSION_SECTIONS }, id)
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').find(|segment| !segment.is_empty()).unwrap_or(path)
}

fn one_of(table: &[(&str, &str)], given: &str) -> Option<String> {
    table.iter().find(|(id, _)| id.eq_ignore_ascii_case(given)).map(|(id, _)| (*id).to_string())
}

fn thread(tail: &str) -> Option<String> {
    let id = decode(tail.strip_prefix('/')?, plain)?;
    let fits = (1..=NAME_LIMIT).contains(&id.len());
    (fits && id.chars().all(|char| char.is_ascii_alphanumeric() || matches!(char, '_' | '-'))).then_some(id)
}

fn name(tail: &str) -> Option<String> {
    let name = decode(tail.strip_prefix('/')?, plain)?;
    let fits = (1..=NAME_LIMIT).contains(&name.chars().count());
    let refused = name.contains(['/', '\\']) || name.starts_with(['-', '.']) || name.trim() != name;
    (fits && !refused && !name.contains(hidden)).then_some(name)
}

fn path(tail: &str) -> Option<String> {
    let in_path = |char| char == '/' || (plain(char) && !matches!(char, '&' | '='));
    let path = decode(tail.strip_prefix("?path=")?, in_path)?;
    let below = path.strip_prefix("~/").or_else(|| path.strip_prefix('/').filter(|below| !below.is_empty()))?;
    let segments = below.strip_suffix('/').unwrap_or(below);
    let refused = path.contains('\\') || path.contains(hidden);
    let bad = |segment| matches!(segment, "" | "." | "..");
    let sound = below.is_empty() || !segments.split('/').any(bad);
    (path.chars().count() <= PATH_LIMIT && sound && !refused).then_some(path)
}

fn plain(char: char) -> bool {
    char.is_alphabetic() || char.is_ascii_digit() || "-._~!$&'()*+,;=:@".contains(char)
}

fn decode(raw: &str, holds: impl Fn(char) -> bool) -> Option<String> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(char) = chars.next() {
        if char == '%' {
            let byte = chars.next()?.to_digit(16)? * 16 + chars.next()?.to_digit(16)?;
            bytes.push(u8::try_from(byte).ok()?);
        } else if holds(char) {
            bytes.extend_from_slice(char.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            return None;
        }
    }
    String::from_utf8(bytes).ok()
}
