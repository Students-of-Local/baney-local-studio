use gtk::gdk;
use gtk::prelude::*;

pub const SIZE: i32 = 16;
pub const HERO: i32 = 48;

macro_rules! icons {
    ($($name:ident = $file:literal;)*) => {
        $(pub const $name: &str = concat!("ls-", $file, "-symbolic");)*
        const FILES: &[(&str, &[u8])] = &[$((concat!("ls-", $file, "-symbolic.svg"), include_bytes!(concat!("../assets/icons/", $file, ".svg")))),*];
    };
}

icons! {
    ADD = "plus";
    CLOSE = "x";
    STOP = "square";
    RUN = "play";
    UP = "chevron-up";
    DOWN = "chevron-down";
    BACK = "chevron-left";
    FORWARD = "chevron-right";
    EXPANDED = "chevron-down";
    COLLAPSED = "chevron-right";
    REFRESH = "refresh-cw";
    COPY = "copy";
    EDIT = "pencil";
    RESET = "undo-2";
    DELETE = "trash-2";
    ARCHIVE = "archive";
    SETTINGS = "settings";
    FOLDER = "folder";
    FOLDER_OPEN = "folder-open";
    FOLDER_NEW = "folder-plus";
    FILE = "file-text";
    TERMINAL = "square-terminal";
    SIDEBAR = "panel-left";
    PANEL = "panel-right";
    SPLIT = "columns-2";
    MAXIMIZE = "maximize-2";
    MORE = "ellipsis";
    ATTACH = "paperclip";
    UNREAD = "mail";
    PIN = "pin";
    DONE = "check";
    WORKING = "loader-circle";
    APPROVAL = "circle-question-mark";
    ERROR = "circle-x";
    WARNING = "triangle-alert";
    INFO = "info";
    TIME = "clock";
    WATCH = "eye";
    SEARCH = "search";
    WEB = "globe";
    AGENT = "bot";
    TOOL = "wrench";
    LOCKED = "lock";
    UNLOCKED = "lock-open";
    GOAL = "target";
    PULL_REQUESTS = "git-pull-request";
    DEVICE = "smartphone";
    USAGE = "gauge";
    SERVER = "server";
    OFFLINE = "cloud-off";
    PLUGIN = "puzzle";
    UNREACHABLE = "unplug";
    KEYBOARD = "keyboard";
    PROVIDERS = "puzzle";
    KEYS = "key-round";
    APPEARANCE = "palette";
    STORAGE = "hard-drive";
    STARRED = "star";
    STAR_ADD = "star-plus";
    NONE = "minus";
    BRANCH = "git-branch";
    SAVE = "save";
    SWITCH = "arrow-left-right";
    COMPACTED = "minimize-2";
    ALL = "layout-grid";
    COMPUTER = "monitor";
    CONNECTIONS = "network";
    UPDATE = "download";
}

pub const STACKED: &str = "ls-icon-stacked";

const CSS: &str = r"
.ls-icon-button image, .ls-icon-button > button image { opacity: 1; }
.ls-icon-stacked image { -gtk-icon-transform: rotate(90deg); }
.ls-footer button { min-width: 30px; min-height: 30px; }
.ls-bulk button { min-width: 28px; min-height: 28px; }
.ls-panel-bar button { min-width: 28px; min-height: 28px; }
.ls-drawer-bar button { min-width: 28px; min-height: 28px; padding: 0; }
.ls-row-actions button { min-width: 28px; min-height: 28px; padding: 0 4px; }
.ls-user-row .ls-row-actions { opacity: 0; }
.ls-user-row:hover .ls-row-actions, .ls-user-row:focus-within .ls-row-actions { opacity: 1; }
.ls-chip-remove { min-width: 24px; min-height: 24px; padding: 0; margin: 0; }
.ls-tab-close { min-width: 24px; min-height: 24px; padding: 0; }
.ls-picker-remove { min-width: 24px; min-height: 24px; padding: 0; }
.ls-minimap-button { min-width: 24px; min-height: 24px; padding: 0; }
.ls-minimap drawingarea { opacity: 0.55; color: var(--accent-color); }
.ls-minimap:hover drawingarea { opacity: 1; }
.ls-picker-icon { min-width: 16px; }
.ls-shelf image { color: var(--sidebar-fg-color); }
.ls-summary image { color: var(--window-fg-color); }
.ls-settings preferencesgroup box.header label.heading { font-size: 1.15em; }
.ls-settings-trail { margin-start: 6px; }
floating-sheet > dimming { background-color: RGB(from var(--shade-color) r g b / calc(alpha * 2.25)); }
.ls-settings row .subtitle { font-size: 12px; }
";

const HIGH_CONTRAST: &str = r"
image, image.dim-label, .dim-label image, .ls-user-row .ls-row-actions, .ls-minimap drawingarea, .ls-thread.receded { opacity: 1; }
image.dim-label, image.ls-meta { color: inherit; }
.ls-footer button.active, .ls-tab.active, button:checked { box-shadow: inset 0 0 0 2px currentColor; }
";

fn write_icons() -> std::io::Result<std::path::PathBuf> {
    let dir = gtk::glib::user_cache_dir().join("local-studio").join("icons");
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in FILES {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
            std::fs::write(&path, bytes)?;
        }
    }
    Ok(dir)
}

pub fn install(display: &gdk::Display) {
    match write_icons() {
        Ok(dir) => gtk::IconTheme::for_display(display).add_search_path(dir),
        Err(error) => eprintln!("local-studio: could not write the app's icons: {error}"),
    }
    let base = gtk::CssProvider::new();
    base.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(display, &base, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 3);
    let contrast = gtk::CssProvider::new();
    contrast.load_from_string(HIGH_CONTRAST);
    let display = display.clone();
    let apply = move |manager: &adw::StyleManager| {
        if manager.is_high_contrast() {
            gtk::style_context_add_provider_for_display(
                &display,
                &contrast,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 4,
            );
        } else {
            gtk::style_context_remove_provider_for_display(&display, &contrast);
        }
    };
    let manager = adw::StyleManager::default();
    apply(&manager);
    manager.connect_high_contrast_notify(apply);
}

pub fn image(name: &str) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(SIZE);
    image
}

pub fn styled(name: &str, class: &str) -> gtk::Image {
    let image = image(name);
    image.add_css_class(class);
    image
}

pub fn button(name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(name);
    describe(&button, label);
    button
}

pub fn flat(name: &str, label: &str) -> gtk::Button {
    let button = button(name, label);
    button.add_css_class("flat");
    button
}

pub fn describe(widget: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>), label: &str) {
    widget.set_tooltip_text(Some(label));
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
    widget.add_css_class("ls-icon-button");
}
