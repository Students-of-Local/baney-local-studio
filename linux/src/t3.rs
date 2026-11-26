use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{
    AccountLabel, ColorScheme, HarnessAccounts, Settings, T3Choices, T3Group, T3Item, T3ItemState, T3Kind, T3Report,
    Update,
};

use crate::app::App;
use crate::format;
use crate::icons;

pub const SCENES: [&str; 6] =
    ["t3-import", "t3-import-light", "t3-import-details", "t3-import-synced", "t3-settings", "t3-accounts"];

thread_local! {
    static SHOWN: Cell<bool> = const { Cell::new(false) };
}

pub fn count(group: &T3Group, state: T3ItemState) -> usize {
    group.items.iter().filter(|item| item.state == state).count()
}

pub fn pending(report: &T3Report) -> usize {
    report.groups.iter().map(|group| count(group, T3ItemState::Import)).sum()
}

pub fn wanted(report: &T3Report) -> bool {
    report.found && !report.offered && pending(report) > 0
}

pub fn offer(app: &Rc<App>) {
    let report = app.state.borrow().t3.clone();
    let Some(report) = report else { return };
    if !app.interactive.get() || !wanted(&report) || SHOWN.with(Cell::get) {
        return;
    }
    present(app, &report, &[]);
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class(class);
    label
}

pub fn summary(group: &T3Group) -> String {
    let importing: Vec<&T3Item> = group.items.iter().filter(|item| item.state == T3ItemState::Import).collect();
    let mut parts = Vec::new();
    if !importing.is_empty() {
        let names: Vec<&str> = importing.iter().take(3).map(|item| item.label.as_str()).collect();
        let more = if importing.len() > 3 { format!(" and {} more", importing.len() - 3) } else { String::new() };
        parts.push(format!("{}{more}", names.join(", ")));
    }
    let synced = count(group, T3ItemState::Synced);
    if synced > 0 {
        parts.push(format!("{synced} already in sync"));
    }
    let kept = count(group, T3ItemState::Kept);
    if kept > 0 {
        parts.push(format!("{kept} changed here, kept"));
    }
    let unsupported = count(group, T3ItemState::Unsupported);
    if unsupported > 0 {
        parts.push(format!("{unsupported} not brought over"));
    }
    parts.join(" · ")
}

fn chosen(choices: T3Choices, kind: T3Kind) -> bool {
    match kind {
        T3Kind::Accounts => choices.accounts,
        T3Kind::Projects => choices.projects,
        T3Kind::Threads => choices.threads,
        T3Kind::Models => choices.models,
        T3Kind::Keybindings => choices.keybindings,
        T3Kind::Appearance => choices.appearance,
        T3Kind::Preferences => choices.preferences,
    }
}

fn set(choices: &mut T3Choices, kind: T3Kind, value: bool) {
    match kind {
        T3Kind::Accounts => choices.accounts = value,
        T3Kind::Projects => choices.projects = value,
        T3Kind::Threads => choices.threads = value,
        T3Kind::Models => choices.models = value,
        T3Kind::Keybindings => choices.keybindings = value,
        T3Kind::Appearance => choices.appearance = value,
        T3Kind::Preferences => choices.preferences = value,
    }
}

fn state_icon(state: T3ItemState) -> (&'static str, &'static str) {
    match state {
        T3ItemState::Import => (icons::ADD, "ls-t3-import"),
        T3ItemState::Synced => (icons::DONE, "ls-t3-synced"),
        T3ItemState::Kept => (icons::AGENT, "ls-t3-kept"),
        T3ItemState::Unsupported => (icons::NONE, "ls-t3-unsupported"),
    }
}

fn item_row(item: &T3Item) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(format::escape(&item.label)).build();
    if !item.detail.is_empty() {
        row.set_subtitle(&format::escape(&item.detail));
    }
    let (name, class) = state_icon(item.state);
    let mark = icons::image(name);
    mark.add_css_class(class);
    row.add_prefix(&mark);
    if item.state == T3ItemState::Unsupported {
        row.add_css_class("dim-label");
    }
    row
}

fn confirm(count: usize) -> String {
    if count == 1 {
        "Bring Over 1 Item".to_string()
    } else {
        format!("Bring Over {count} Items")
    }
}

fn header() -> gtk::Box {
    let tile = icons::image(icons::ARCHIVE);
    tile.set_pixel_size(24);
    tile.set_vexpand(true);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.set_size_request(40, 40);
    frame.set_valign(gtk::Align::Start);
    frame.add_css_class("ls-consent-tile");
    frame.append(&tile);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text.set_hexpand(true);
    text.append(&label("Bring over your T3 Code setup", "ls-consent-title"));
    text.append(&label(
        "Local Studio found T3 Code on this computer. Pick what to bring over. It keeps syncing from T3 Code until you turn that off in Settings, and anything you change here stays yours.",
        "ls-consent-secondary",
    ));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&frame);
    row.append(&text);
    row
}

#[allow(clippy::too_many_lines)]
pub fn present(app: &Rc<App>, report: &T3Report, open: &[T3Kind]) {
    let dialog = adw::Dialog::builder().title("Bring over your T3 Code setup").content_width(560).build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let top = gtk::Box::new(gtk::Orientation::Vertical, 14);
    top.set_margin_top(18);
    top.set_margin_bottom(18);
    top.set_margin_start(18);
    top.set_margin_end(18);
    top.append(&header());
    column.append(&top);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let choices = Rc::new(RefCell::new(report.choices));
    let groups: Vec<T3Group> = report.groups.iter().filter(|group| !group.items.is_empty()).cloned().collect();
    let total = {
        let groups = groups.clone();
        move |choices: &T3Choices| -> usize {
            groups
                .iter()
                .filter(|group| chosen(*choices, group.kind))
                .map(|group| count(group, T3ItemState::Import))
                .sum()
        }
    };
    let install = gtk::Button::with_label(&confirm(total(&choices.borrow())));
    install.add_css_class("suggested-action");
    install.set_sensitive(total(&choices.borrow()) > 0);
    let total = Rc::new(total);
    for group in &groups {
        let row = adw::ExpanderRow::builder()
            .title(format::escape(&group.title))
            .subtitle(format::escape(&summary(group)))
            .expanded(open.contains(&group.kind))
            .build();
        let switch = gtk::Switch::builder()
            .active(chosen(*choices.borrow(), group.kind))
            .valign(gtk::Align::Center)
            .sensitive(count(group, T3ItemState::Import) > 0)
            .build();
        switch.update_property(&[gtk::accessible::Property::Label(&group.title)]);
        let (choices, install, total, kind) = (Rc::clone(&choices), install.clone(), Rc::clone(&total), group.kind);
        switch.connect_active_notify(move |switch| {
            set(&mut choices.borrow_mut(), kind, switch.is_active());
            let count = total(&choices.borrow());
            install.set_label(&confirm(count));
            install.set_sensitive(count > 0);
        });
        row.add_suffix(&switch);
        for item in &group.items {
            row.add_row(&item_row(item));
        }
        list.append(&row);
    }
    body.append(&list);
    column.append(&body);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    footer.add_css_class("ls-consent-footer");
    footer.append(&label(
        "Sign-ins stay where they are. Local Studio uses the same Claude Code and Codex folders as T3 Code, so both apps share one login. Nothing is written back to T3 Code.",
        "ls-consent-secondary",
    ));
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let later = gtk::Button::with_label("Not Now");
    buttons.append(&later);
    buttons.append(&install);
    footer.append(&buttons);
    column.append(&footer);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(760)
        .child(&column)
        .build();
    dialog.set_child(Some(&scroller));
    let answered = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(app);
    let (closing, done, app_ref) = (dialog.clone(), Rc::clone(&answered), weak.clone());
    later.connect_clicked(move |_| {
        done.set(true);
        if let Some(app) = app_ref.upgrade() {
            app.core.run(local_studio_core::Studio::dismiss_t3_import);
        }
        closing.close();
    });
    let (closing, done, app_ref) = (dialog.clone(), Rc::clone(&answered), weak.clone());
    install.connect_clicked(move |_| {
        done.set(true);
        let picked = *choices.borrow();
        if let Some(app) = app_ref.upgrade() {
            app.core.run(move |studio| studio.import_t3(picked));
        }
        closing.close();
    });
    dialog.connect_closed(move |_| {
        SHOWN.with(|shown| shown.set(false));
        if !answered.replace(true) {
            if let Some(app) = weak.upgrade() {
                app.core.run(local_studio_core::Studio::dismiss_t3_import);
            }
        }
    });
    SHOWN.with(|shown| shown.set(true));
    dialog.present(Some(&app.window));
}

fn abbreviate(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn status(report: &T3Report) -> String {
    if !report.enabled {
        return match pending(report) {
            0 => "Off. Nothing new in T3 Code.".to_string(),
            count => format!("Off. {count} new in T3 Code."),
        };
    }
    let synced: usize = report.groups.iter().map(|group| count(group, T3ItemState::Synced)).sum();
    let kept: usize = report.groups.iter().map(|group| count(group, T3ItemState::Kept)).sum();
    let kept = if kept == 0 { String::new() } else { format!(", {kept} changed here and kept") };
    format!("Following {}: {synced} in sync{kept}.", abbreviate(&report.source))
}

fn last(report: &T3Report) -> String {
    report.last_synced_at.map_or_else(|| "Never".to_string(), |at| format::relative(at, format::now_ms()))
}

pub fn rows(app: &Weak<App>, report: Option<&T3Report>) -> Vec<gtk::Widget> {
    let Some(report) = report.filter(|report| report.found) else {
        let row = adw::ActionRow::builder()
            .title("T3 Code sync")
            .subtitle(if report.is_none() { "Looking for T3 Code…" } else { "T3 Code is not on this computer." })
            .build();
        return vec![row.upcast()];
    };
    let switch = adw::SwitchRow::builder()
        .title("T3 Code sync")
        .subtitle(format::escape(&status(report)))
        .active(report.enabled)
        .build();
    let weak = app.clone();
    switch.connect_active_notify(move |switch| {
        let enabled = switch.is_active();
        if let Some(app) = weak.upgrade() {
            app.core.run(move |studio| studio.set_t3_sync(enabled));
        }
    });
    let synced = adw::ActionRow::builder().title("Last synced").subtitle(format::escape(&last(report))).build();
    let now = gtk::Button::builder().label("Import Now").valign(gtk::Align::Center).build();
    now.add_css_class("flat");
    let (weak, choices) = (app.clone(), report.choices);
    now.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.core.run(move |studio| studio.import_t3(choices));
        }
    });
    synced.add_suffix(&now);
    let mut rows: Vec<gtk::Widget> = vec![switch.upcast(), synced.upcast()];
    for problem in &report.problems {
        let row = adw::ActionRow::builder().title(format::escape(problem)).build();
        row.add_prefix(&icons::image(icons::WARNING));
        rows.push(row.upcast());
    }
    rows
}

pub fn accounts_group(accounts: &HarnessAccounts) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Claude Code and Codex Accounts")
        .description("Each account is a config folder with its own sign-in. When one hits its usage limit, a thread can move to another.")
        .build();
    let label = |dir: &str| accounts.labels.iter().find(|label| label.dir == dir);
    let entries =
        accounts.claude.iter().map(|dir| ("Claude Code", dir)).chain(accounts.codex.iter().map(|dir| ("Codex", dir)));
    let mut any = false;
    for (harness, dir) in entries {
        any = true;
        let found = label(dir);
        let name = found
            .map(|label| label.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| crate::state::short_path(dir));
        let row = adw::ActionRow::builder()
            .title(format::escape(&name))
            .subtitle(format::escape(&format!("{harness} · {}", abbreviate(dir))))
            .build();
        row.add_prefix(&icons::image(if harness == "Codex" { icons::TERMINAL } else { icons::AGENT }));
        if let Some(source) = found.and_then(|label| label.source.as_ref()) {
            let tag = gtk::Label::new(Some(&format!("From {source}")));
            tag.add_css_class("ls-pill");
            tag.set_valign(gtk::Align::Center);
            row.add_suffix(&tag);
        }
        group.add(&row);
    }
    if !any {
        group.add(
            &adw::ActionRow::builder()
                .title("Local Studio finds signed-in ~/.claude and ~/.codex folders on its own.")
                .build(),
        );
    }
    group
}

fn item(key: &str, label: &str, detail: &str, state: T3ItemState) -> T3Item {
    T3Item { key: key.to_string(), label: label.to_string(), detail: detail.to_string(), state }
}

fn fixture_groups(state: T3ItemState) -> Vec<T3Group> {
    let kept = if state == T3ItemState::Synced { T3ItemState::Kept } else { state };
    let group = |kind, title: &str, items| T3Group { kind, title: title.to_string(), chosen: true, items };
    vec![
        group(
            T3Kind::Accounts,
            "Accounts",
            vec![
                item("account:claudeAgent", "personal", "Claude Code account at ~/.claude", state),
                item("account:claudeAgent_work", "work", "Claude Code account at ~/.claude-work", state),
                item("account:codex", "Codex", "Codex account at ~/.codex", state),
                item("account:cursor", "Cursor", "Not supported yet: Local Studio has no Cursor harness. Claude Code and Codex are supported. API key set in T3 Code.", T3ItemState::Unsupported),
                item("account:opencode", "OpenCode", "Not supported yet: Local Studio has no OpenCode harness. Claude Code and Codex are supported. Server password set in T3 Code.", T3ItemState::Unsupported),
            ],
        ),
        group(
            T3Kind::Projects,
            "Projects",
            vec![
                item("project:/home/demo/Projects/local-studio", "local-studio", "~/Projects/local-studio", state),
                item("project:/home/demo/Projects/field-notes", "field-notes", "~/Projects/field-notes", state),
                item("project:/home/demo/Projects/site", "site", "~/Projects/site", kept),
                item("project:p-old", "old-prototype", "Its folder is gone: ~/Projects/old-prototype", T3ItemState::Unsupported),
            ],
        ),
        group(
            T3Kind::Threads,
            "Threads",
            vec![
                item("thread:demo-1", "Fix the sidebar order", "Claude Code, ~/Projects/local-studio", state),
                item("thread:demo-2", "Draft the release notes", "Codex, ~/Projects/site", state),
                item("thread:demo-3", "Port the importer", "Running in T3 Code. It comes over when it stops.", T3ItemState::Unsupported),
            ],
        ),
        group(
            T3Kind::Models,
            "Models",
            vec![
                item("model.default", "Default model", "Claude Opus 5.5, thinking high", state),
                item("favorite:claude-code/claude-opus-5-5", "Claude Opus 5.5", "Favorite", state),
                item("favorite:codex/gpt-6.1-sol", "gpt-6.1-sol", "Favorite (Codex)", state),
                item("favorite:cursor/kimi-k3p1-pro", "kimi-k3p1-pro", "Not supported yet: a Cursor model.", T3ItemState::Unsupported),
            ],
        ),
        group(
            T3Kind::Keybindings,
            "Keybindings",
            vec![
                item("keybinding:chat.new", "New Thread", "Ctrl+Shift+O", state),
                item("keybinding:unknown", "2 shortcuts", "Not supported yet: Local Studio has no composer.dictation or terminal.split command.", T3ItemState::Unsupported),
            ],
        ),
        group(
            T3Kind::Appearance,
            "Appearance",
            vec![
                item("setting.theme", "Theme", "Martyr Brand", state),
                item("setting.chatWidth", "Chat width", "Wide", kept),
            ],
        ),
        group(
            T3Kind::Preferences,
            "Preferences",
            vec![
                item("setting.timeFormat", "Time format", "24-hour", state),
                item("setting.sendShortcut", "Send shortcut", "Ctrl+Enter", state),
            ],
        ),
    ]
}

fn fixture(state: T3ItemState) -> T3Report {
    let synced = state == T3ItemState::Synced;
    T3Report {
        found: true,
        source: "/home/demo/.t3/userdata".to_string(),
        enabled: synced,
        offered: synced,
        applied: synced,
        last_synced_at: synced.then(|| format::now_ms() - 4 * 60_000),
        choices: T3Choices::default(),
        groups: fixture_groups(state),
        problems: Vec::new(),
    }
}

fn fixture_accounts() -> HarnessAccounts {
    let label = |dir: &str, name: &str, accent: Option<&str>| AccountLabel {
        dir: dir.to_string(),
        name: name.to_string(),
        accent: accent.map(str::to_string),
        source: Some("T3 Code".to_string()),
    };
    HarnessAccounts {
        claude: vec!["/home/demo/.claude".to_string(), "/home/demo/.claude-work".to_string()],
        codex: vec!["/home/demo/.codex".to_string()],
        labels: vec![
            label("/home/demo/.claude", "personal", None),
            label("/home/demo/.claude-work", "work", Some("#d4af5d")),
            label("/home/demo/.codex", "Codex", Some("#16a34a")),
        ],
    }
}

fn scroll_later(app: &Rc<App>) {
    let weak = Rc::downgrade(app);
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(dialog) = weak.upgrade().and_then(|app| app.window.visible_dialog()) {
            crate::scenes_machines::scroll_to_end(dialog.upcast_ref());
        }
    });
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if !SCENES.contains(&scene) {
        return false;
    }
    crate::fixtures::conversation(app, false);
    let mut settings = Settings { harness_accounts: fixture_accounts(), ..Settings::default() };
    if scene.ends_with("-light") {
        settings.color_scheme = ColorScheme::Light;
    }
    app.apply(vec![Update::Settings { settings }]);
    match scene.trim_end_matches("-light") {
        "t3-import-details" => present(app, &fixture(T3ItemState::Import), &[T3Kind::Accounts, T3Kind::Models]),
        "t3-import-synced" => present(app, &fixture(T3ItemState::Synced), &[T3Kind::Projects]),
        "t3-settings" => {
            app.apply(vec![Update::T3Sync { report: fixture(T3ItemState::Synced) }]);
            app.open_settings(Some("general"));
            scroll_later(app);
        }
        "t3-accounts" => {
            app.open_settings(Some("providers"));
            scroll_later(app);
        }
        _ => present(app, &fixture(T3ItemState::Import), &[]),
    }
    true
}
