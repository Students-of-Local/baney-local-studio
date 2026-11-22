use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::gio;
use local_studio_core::{CatalogPlugin, ConsentNeed, PluginGrant, PluginRow, PluginState};

use crate::app::{App, Dirty, Target};
use crate::format;
use crate::icons;
use crate::state::State;

pub const INSTALLING: &str = "install";
pub const CHECKING: &str = "check";

thread_local! {
    static REQUESTS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static EXPANDED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

pub fn expand(ids: &[&str]) {
    EXPANDED.with(|expanded| expanded.borrow_mut().extend(ids.iter().map(|id| (*id).to_string())));
}

fn act(app: &Weak<App>, key: &str, call: impl FnOnce(&local_studio_core::Studio, String)) {
    let Some(app) = app.upgrade() else { return };
    let request = app.ask(Target::Plugin);
    REQUESTS.with(|requests| requests.borrow_mut().insert(request.clone(), key.to_string()));
    {
        let mut state = app.state.borrow_mut();
        state.plugin_pending.insert(key.to_string());
        state.plugin_error = None;
    }
    app.render(Dirty { settings: true, ..Dirty::default() });
    app.core.run(|studio| call(studio, request));
}

fn settle(app: &App, request: &str) {
    if let Some(key) = REQUESTS.with(|requests| requests.borrow_mut().remove(request)) {
        app.state.borrow_mut().plugin_pending.remove(&key);
    }
}

pub fn done(app: &Rc<App>, request: &str, message: &str) {
    settle(app, request);
    if !message.is_empty() {
        app.toast(message);
    }
    app.render(Dirty { settings: true, ..Dirty::default() });
}

pub fn failed(app: &Rc<App>, request: &str, message: &str) {
    settle(app, request);
    app.state.borrow_mut().plugin_error = Some(message.to_string());
    app.render(Dirty { settings: true, ..Dirty::default() });
}

pub fn install(app: &Weak<App>, source: &str, key: &str) {
    let source = source.trim().to_string();
    if source.is_empty() {
        return;
    }
    act(app, key, move |studio, request| studio.install_plugin(request, source));
}

fn state_label(state: PluginState) -> &'static str {
    match state {
        PluginState::Running => "Running",
        PluginState::Waiting => "Waiting for you",
        PluginState::Off => "Off",
        PluginState::Failed => "Failed",
    }
}

fn state_icon(state: PluginState) -> gtk::Image {
    let image = gtk::Image::from_icon_name(match state {
        PluginState::Running => icons::DONE,
        PluginState::Waiting | PluginState::Failed => icons::WARNING,
        PluginState::Off => icons::PLUGIN,
    });
    match state {
        PluginState::Waiting => image.add_css_class("ls-provider-warning"),
        PluginState::Failed => image.add_css_class("ls-provider-error"),
        PluginState::Running | PluginState::Off => image.add_css_class("dim-label"),
    }
    image.set_tooltip_text(Some(state_label(state)));
    image
}

fn suffix_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_valign(gtk::Align::Center);
    label
}

fn flat_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.set_valign(gtk::Align::Center);
    button
}

fn source_line(plugin: &PluginRow) -> String {
    let mut parts =
        vec![if plugin.source.is_empty() { "Copied in by hand".to_string() } else { plugin.source.clone() }];
    if let Some(revision) = &plugin.revision {
        parts.push(format!("at {revision}"));
    }
    if plugin.pinned {
        parts.push("pinned".to_string());
    } else if let Some(channel) = &plugin.channel {
        parts.push(format!("follows {channel}"));
    }
    parts.join(" · ")
}

fn grant_caption(grant: &PluginGrant, told: bool) -> String {
    if grant.need != ConsentNeed::Sensitive {
        return grant.id.clone();
    }
    match (&grant.gates, told) {
        (Some(gates), true) => {
            format!("Off means no {gates}. The plugin is told; Local Studio cannot block one that ignores it.")
        }
        (None, true) => {
            "Off turns this off for the plugin. The plugin is told; Local Studio cannot block one that ignores it."
                .to_string()
        }
        _ => "The plugin's tools stay off until this is on.".to_string(),
    }
}

fn grant_row(app: &Weak<App>, plugin: &PluginRow, grant: &PluginGrant, pending: bool) -> gtk::Widget {
    let subtitle = format::escape(&grant_caption(grant, plugin.told));
    if grant.need == ConsentNeed::Sensitive {
        let row = adw::SwitchRow::builder()
            .title(format::escape(&grant.sentence))
            .subtitle(subtitle)
            .active(grant.granted)
            .sensitive(!pending)
            .build();
        let tag = gtk::Label::new(Some("Sensitive"));
        tag.add_css_class("ls-consent-tag");
        tag.set_valign(gtk::Align::Center);
        row.add_suffix(&tag);
        let (weak, id, capability) = (app.clone(), plugin.id.clone(), grant.id.clone());
        row.connect_active_notify(move |row| {
            let (id, capability, on) = (id.clone(), capability.clone(), row.is_active());
            let key = id.clone();
            act(&weak, &key, move |studio, request| {
                if on {
                    studio.grant_plugin(request, id, vec![capability]);
                } else {
                    studio.revoke_plugin(request, id, capability);
                }
            });
        });
        row.upcast()
    } else {
        let row = adw::ActionRow::builder().title(format::escape(&grant.sentence)).subtitle(subtitle).build();
        let label = suffix_label(if grant.granted { "Allowed" } else { "Not allowed" });
        if !grant.granted {
            label.add_css_class("ls-provider-warning");
        }
        row.add_suffix(&label);
        row.upcast()
    }
}

fn confirm_remove(app: &Weak<App>, plugin: &PluginRow) {
    let Some(strong) = app.upgrade() else { return };
    let (weak, id) = (app.clone(), plugin.id.clone());
    crate::actions::confirm_action(
        &strong,
        &format!("Remove {}?", plugin.name),
        "Its folder and its permissions are deleted. Its commands and tools leave every thread. You can install it again later.",
        "Remove",
        true,
        move |_| {
            let id = id.clone();
            act(&weak, &id.clone(), move |studio, request| studio.remove_plugin(request, id));
        },
    );
}

fn row_action(app: &Weak<App>, plugin: &PluginRow, pending: bool) -> gtk::Widget {
    if pending {
        let spinner = gtk::Spinner::new();
        spinner.start();
        spinner.set_valign(gtk::Align::Center);
        return spinner.upcast();
    }
    let (weak, id) = (app.clone(), plugin.id.clone());
    if let Some(next) = &plugin.update {
        let button = flat_button(&format!("Update to {}", next.version));
        button.add_css_class("suggested-action");
        if !next.asks.is_empty() {
            button.set_tooltip_text(Some(&format!("Asks for {}; you choose before it installs", next.asks.join(", "))));
        }
        button.connect_clicked(move |_| {
            let id = id.clone();
            act(&weak, &id.clone(), move |studio, request| studio.update_plugin(request, id));
        });
        return button.upcast();
    }
    let button = flat_button("Allow…");
    button.set_visible(plugin.state == PluginState::Waiting);
    button.connect_clicked(move |_| {
        let id = id.clone();
        act(&weak, &id.clone(), move |studio, request| studio.grant_plugin(request, id, Vec::new()));
    });
    button.upcast()
}

fn plugin_row(app: &Weak<App>, state: &State, plugin: &PluginRow) -> adw::ExpanderRow {
    let pending = state.plugin_pending.contains(&plugin.id);
    let title = if plugin.version.is_empty() {
        format::escape(&plugin.name)
    } else {
        format!("{} <tt>{}</tt>", format::escape(&plugin.name), format::escape(&plugin.version))
    };
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(format::escape(&format!("{} · {}", state_label(plugin.state), plugin.detail)))
        .expanded(EXPANDED.with(|expanded| expanded.borrow().contains(&plugin.id)))
        .build();
    let id = plugin.id.clone();
    row.connect_expanded_notify(move |row| {
        EXPANDED.with(|expanded| {
            let mut expanded = expanded.borrow_mut();
            if row.is_expanded() {
                expanded.insert(id.clone());
            } else {
                expanded.remove(&id);
            }
        });
    });
    row.add_prefix(&state_icon(plugin.state));
    row.add_suffix(&row_action(app, plugin, pending));
    if !plugin.description.is_empty() {
        let about = adw::ActionRow::builder().title(format::escape(&plugin.description)).build();
        about.set_title_lines(0);
        row.add_row(&about);
    }
    for grant in &plugin.grants {
        row.add_row(&grant_row(app, plugin, grant, pending));
    }
    for command in &plugin.commands {
        let line = adw::ActionRow::builder()
            .title(format!("<tt>/{}</tt>", format::escape(&command.name)))
            .subtitle(format::escape(&command.description))
            .build();
        line.add_suffix(&suffix_label("Command"));
        row.add_row(&line);
    }
    for skill in &plugin.skills {
        let line = adw::ActionRow::builder()
            .title(format!("<tt>{}</tt>", format::escape(&skill.name)))
            .subtitle(format::escape(&skill.description))
            .build();
        line.add_suffix(&suffix_label("Skill"));
        row.add_row(&line);
    }
    let source = adw::ActionRow::builder()
        .title("Source")
        .subtitle(format::escape(&source_line(plugin)))
        .subtitle_selectable(true)
        .build();
    if !plugin.pinned && plugin.channel.is_some() {
        let warning = gtk::Label::new(Some("Not pinned by the catalog"));
        warning.add_css_class("ls-provider-warning");
        warning.set_valign(gtk::Align::Center);
        source.add_suffix(&warning);
    }
    row.add_row(&source);
    let actions = adw::ActionRow::new();
    let open = flat_button("Open Folder");
    let folder = plugin.folder.clone();
    let weak = app.clone();
    open.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&folder)));
        launcher.launch(Some(&app.window), None::<&gio::Cancellable>, |_| {});
    });
    actions.add_prefix(&open);
    let remove = flat_button("Remove…");
    remove.add_css_class("destructive-action");
    remove.set_sensitive(!pending);
    let (weak, shown) = (app.clone(), plugin.clone());
    remove.connect_clicked(move |_| confirm_remove(&weak, &shown));
    actions.add_suffix(&remove);
    row.add_row(&actions);
    row
}

fn catalog_row(app: &Weak<App>, state: &State, entry: &CatalogPlugin) -> adw::ActionRow {
    let local = if entry.local_only { " Local models only." } else { "" };
    let asks = if entry.grants.is_empty() {
        format!("Asks for no permissions. Pinned to {}.{local}", entry.revision)
    } else {
        let sentences: Vec<String> = entry.grants.iter().map(|grant| grant.sentence.to_lowercase()).collect();
        format!("Asks to {}. Pinned to {}.{local}", sentences.join(", "), entry.revision)
    };
    let row = adw::ActionRow::builder()
        .title(format::escape(&entry.name))
        .subtitle(format::escape(&format!("{}\n{asks}", entry.description)))
        .build();
    row.set_subtitle_lines(0);
    let icon = gtk::Image::from_icon_name(icons::PLUGIN);
    icon.add_css_class("dim-label");
    row.add_prefix(&icon);
    if state.plugin_pending.contains(&entry.id) {
        let spinner = gtk::Spinner::new();
        spinner.start();
        spinner.set_valign(gtk::Align::Center);
        row.add_suffix(&spinner);
    } else if let Some(version) = &entry.installed {
        row.add_suffix(&suffix_label(&format!("Installed {version}")));
    } else {
        let button = flat_button("Install");
        let (weak, id) = (app.clone(), entry.id.clone());
        button.connect_clicked(move |_| install(&weak, &id, &id));
        row.add_suffix(&button);
    }
    row
}

fn channel(state: &State) -> String {
    if state.plugin_channel.is_empty() {
        "app's".to_string()
    } else {
        state.plugin_channel.clone()
    }
}

fn installed_group(app: &Weak<App>, state: &State) -> adw::PreferencesGroup {
    let checked = state.plugins_checked.map(|checked| format::relative(checked, format::now_ms())).map_or_else(
        String::new,
        |ago| if ago == "now" { " Last checked just now.".to_string() } else { format!(" Last checked {ago} ago.") },
    );
    let description = format!(
        "Plugins installed from git follow the {} channel, like Local Studio. An update that asks for new permissions shows them first.{checked}",
        channel(state)
    );
    let group = adw::PreferencesGroup::builder().title("Installed").description(format::escape(&description)).build();
    let check = flat_button("Check for Updates");
    check.add_css_class("flat");
    check.set_sensitive(!state.plugin_pending.contains(CHECKING));
    let weak = app.clone();
    check.connect_clicked(move |_| act(&weak, CHECKING, local_studio_core::Studio::check_plugin_updates));
    group.set_header_suffix(Some(&check));
    if let Some(error) = &state.plugin_error {
        let row = adw::ActionRow::builder().title(format::escape(error)).build();
        row.set_title_lines(0);
        row.add_css_class("ls-provider-error");
        row.add_prefix(&gtk::Image::from_icon_name(icons::WARNING));
        group.add(&row);
    }
    if state.plugins.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title(if state.plugins_loaded {
                    "No plugins yet. Install one from the catalog below."
                } else {
                    "Loading plugins…"
                })
                .build(),
        );
    }
    for plugin in &state.plugins {
        group.add(&plugin_row(app, state, plugin));
    }
    group
}

fn source_group(app: &Weak<App>, state: &State) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Install from a Git URL or Folder")
        .description(format::escape(&format!(
            "A git URL follows the {} channel. Code from a URL or folder is not pinned by the catalog: you still see everything it asks for before it runs, but read it first if you do not know who wrote it.",
            channel(state)
        )))
        .build();
    let busy = state.plugin_pending.contains(INSTALLING);
    let entry = adw::EntryRow::builder().title("Git URL").show_apply_button(true).sensitive(!busy).build();
    let weak = app.clone();
    entry.connect_apply(move |entry| {
        install(&weak, &entry.text(), INSTALLING);
        entry.set_text("");
    });
    group.add(&entry);
    let folder = adw::ActionRow::builder().title("Plugin folder").subtitle("A folder with a plugin.toml").build();
    let warning = gtk::Label::new(Some("Not pinned by the catalog"));
    warning.add_css_class("ls-provider-warning");
    warning.set_valign(gtk::Align::Center);
    folder.add_suffix(&warning);
    let choose = flat_button("Choose Folder…");
    choose.set_sensitive(!busy);
    let weak = app.clone();
    choose.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder().title("Install Plugin").modal(true).build();
        let weak = weak.clone();
        dialog.select_folder(Some(&app.window), None::<&gio::Cancellable>, move |result| {
            if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                install(&weak, &path.to_string_lossy(), INSTALLING);
            }
        });
    });
    folder.add_suffix(&choose);
    group.add(&folder);
    group
}

pub fn groups(app: &Weak<App>, state: &State) -> Vec<adw::PreferencesGroup> {
    let catalog = adw::PreferencesGroup::builder()
        .title("Catalog")
        .description(
            "Catalog plugins are pinned to a commit and a content hash that Local Studio checks before anything runs.",
        )
        .build();
    for entry in &state.catalog {
        catalog.add(&catalog_row(app, state, entry));
    }
    vec![installed_group(app, state), catalog, source_group(app, state)]
}
