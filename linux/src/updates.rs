use std::cell::{Cell, RefCell};
use std::rc::Weak;

use adw::prelude::*;
use local_studio_core::{update::channel_name, AppUpdateState};

use crate::app::App;
use crate::format;
use crate::icons;
use crate::picker::{Picker, Row, Spec};
use crate::state::State;

const HELP: &str =
    "Each channel is its own app. Switching opens that app, or its download page when it is not installed.";

pub struct UpdatesGroup {
    pub group: adw::PreferencesGroup,
    build: adw::ActionRow,
    status: adw::ActionRow,
    mark: gtk::Stack,
    marked: Cell<bool>,
    progress: gtk::ProgressBar,
    check: gtk::Button,
    download: gtk::Button,
    restart: gtk::Button,
    automatic: adw::SwitchRow,
    notes_row: gtk::ListBoxRow,
    notes_title: gtk::Label,
    notes: gtk::ScrolledWindow,
    notes_shown: RefCell<String>,
    channels: Picker,
}

struct Status {
    title: String,
    subtitle: String,
    busy: bool,
    failed: bool,
    fraction: Option<f64>,
    notes: Option<(String, String)>,
}

impl UpdatesGroup {
    pub fn new(app: &Weak<App>) -> Self {
        let group = adw::PreferencesGroup::builder().title("Updates").build();
        let build = adw::ActionRow::builder().subtitle_selectable(true).build();
        group.add(&build);
        let status = adw::ActionRow::builder().title_lines(2).subtitle_lines(4).build();
        let spinner = adw::Spinner::new();
        spinner.set_size_request(icons::SIZE, icons::SIZE);
        let mark = gtk::Stack::builder().valign(gtk::Align::Center).build();
        mark.add_named(&spinner, Some("busy"));
        mark.add_named(&icons::styled(icons::ERROR, "error"), Some("failed"));
        let progress = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(140).build();
        status.add_suffix(&progress);
        let download = button("Download", "win.download-update");
        status.add_suffix(&download);
        let restart = button("Restart to Update", "win.apply-update");
        restart.add_css_class("suggested-action");
        restart.remove_css_class("flat");
        status.add_suffix(&restart);
        let check = button("Check Now", "win.check-update");
        status.add_suffix(&check);
        group.add(&status);
        let current = app.upgrade().is_some_and(|app| app.state.borrow().settings.automatic_updates);
        let automatic = adw::SwitchRow::builder()
            .title("Automatic updates")
            .subtitle("Download new versions in the background, then restart when you are ready")
            .active(current)
            .build();
        let weak = app.clone();
        automatic.connect_active_notify(move |row| {
            let Some(app) = weak.upgrade() else { return };
            let active = row.is_active();
            if app.syncing.get() || app.state.borrow().settings.automatic_updates == active {
                return;
            }
            crate::settings::change(&app, |settings| settings.automatic_updates = active);
            let dialog = app.settings.borrow().clone();
            if let Some(dialog) = dialog {
                dialog.refresh_updates(&app.state.borrow());
            }
        });
        group.add(&automatic);
        let (notes_row, notes_title, notes) = notes_row();
        group.add(&notes_row);
        let weak = app.clone();
        let spec = Spec { tooltip: "Other Channels", title: Some("Other channels"), width: 300, ..Spec::default() };
        let channels = Picker::new(&spec, move |channel| {
            if let Some(app) = weak.upgrade() {
                let channel = channel.to_string();
                app.core.run(|studio| studio.open_channel(channel));
            }
        });
        channels.set_label("Switch Channel");
        channels.button.set_valign(gtk::Align::Center);
        let channel = adw::ActionRow::builder().title("Channel").subtitle(HELP).subtitle_lines(3).build();
        channel.add_suffix(&channels.button);
        group.add(&channel);
        Self {
            group,
            build,
            status,
            mark,
            marked: Cell::new(false),
            progress,
            check,
            download,
            restart,
            automatic,
            notes_row,
            notes_title,
            notes,
            notes_shown: RefCell::new(String::new()),
            channels,
        }
    }

    pub fn refresh(&self, state: &State) {
        let (channel, version) = state
            .build
            .clone()
            .unwrap_or_else(|| (local_studio_core::CHANNEL.to_string(), local_studio_core::VERSION.to_string()));
        self.build.set_title(&format::escape(channel_name(&channel)));
        self.build.set_subtitle(&format::escape(&format!("Version {version}")));
        let automatic = state.settings.automatic_updates;
        if self.automatic.is_active() != automatic {
            self.automatic.set_active(automatic);
        }
        let status = status(state.update.as_ref(), automatic);
        self.status.set_title(&format::escape(&status.title));
        self.status.set_subtitle(&format::escape(&status.subtitle));
        let marked = status.busy || status.failed;
        if marked != self.marked.replace(marked) {
            if marked {
                self.status.add_prefix(&self.mark);
            } else {
                self.status.remove(&self.mark);
            }
        }
        self.mark.set_visible_child_name(if status.busy { "busy" } else { "failed" });
        self.progress.set_visible(status.fraction.is_some());
        self.progress.set_fraction(status.fraction.unwrap_or_default());
        let update = state.update.as_ref();
        self.check.set_sensitive(!matches!(
            update,
            Some(
                AppUpdateState::Checking
                    | AppUpdateState::Downloading { .. }
                    | AppUpdateState::Applying { .. }
                    | AppUpdateState::Restarting { .. }
            )
        ));
        self.download.set_visible(!automatic && matches!(update, Some(AppUpdateState::Available { .. })));
        self.restart.set_visible(matches!(update, Some(AppUpdateState::Ready { .. })));
        self.render_notes(status.notes);
        let rows: Vec<Row> = state
            .channels
            .iter()
            .filter(|install| !install.current && install.channel != channel)
            .map(|install| {
                let (verb, detail) = if install.installed {
                    ("Open", "Installed on this computer")
                } else {
                    ("Get", "Opens its download page")
                };
                Row::new(install.channel.clone(), format!("{verb} {}", install.name)).subtitle(detail)
            })
            .collect();
        self.channels.button.set_sensitive(!rows.is_empty());
        self.channels.set_rows(rows);
    }

    fn render_notes(&self, notes: Option<(String, String)>) {
        let Some((version, text)) = notes.filter(|(_, text)| !text.trim().is_empty()) else {
            self.notes_row.set_visible(false);
            return;
        };
        self.notes_row.set_visible(true);
        self.notes_title.set_label(&format!("What's new in {version}"));
        if *self.notes_shown.borrow() != text {
            let view = crate::markdown::view(text.trim());
            view.set_top_margin(8);
            view.set_bottom_margin(8);
            view.set_left_margin(10);
            view.set_right_margin(10);
            self.notes.set_child(Some(&view));
            self.notes_shown.replace(text);
        }
    }
}

fn button(label: &str, action: &str) -> gtk::Button {
    let button = gtk::Button::builder().label(label).action_name(action).valign(gtk::Align::Center).build();
    button.add_css_class("flat");
    button
}

fn notes_row() -> (gtk::ListBoxRow, gtk::Label, gtk::ScrolledWindow) {
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.add_css_class("heading");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(180)
        .max_content_height(240)
        .build();
    scroller.add_css_class("ls-update-notes");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.add_css_class("ls-update-notes-row");
    column.append(&title);
    column.append(&scroller);
    let row = gtk::ListBoxRow::builder().activatable(false).selectable(false).child(&column).visible(false).build();
    (row, title, scroller)
}

fn bytes(count: u64) -> String {
    gtk::glib::format_size(count).to_string()
}

fn status(update: Option<&AppUpdateState>, automatic: bool) -> Status {
    let plain = |title: &str, subtitle: &str| Status {
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        busy: false,
        failed: false,
        fraction: None,
        notes: None,
    };
    match update {
        None | Some(AppUpdateState::Idle) => {
            plain("Not checked yet", "Local Studio checks shortly after it starts and every 6 hours.")
        }
        Some(AppUpdateState::Checking) => Status { busy: true, ..plain("Checking for updates…", "") },
        Some(AppUpdateState::UpToDate { latest }) => {
            plain("Local Studio is up to date", &format!("The newest version on this channel is {latest}."))
        }
        Some(AppUpdateState::Available { version, notes }) => Status {
            notes: Some((version.clone(), notes.clone())),
            ..plain(
                &format!("Version {version} is available"),
                if automatic {
                    "It downloads in the background."
                } else {
                    "Download it now, or turn on automatic updates."
                },
            )
        },
        Some(AppUpdateState::Downloading { version, notes, received, total }) => {
            #[allow(clippy::cast_precision_loss)]
            let fraction =
                total.filter(|total| *total > 0).map(|total| (*received as f64 / total as f64).clamp(0.0, 1.0));
            let progress = match total {
                Some(total) => format!("{} of {}", bytes(*received), bytes(*total)),
                None => format!("{} so far", bytes(*received)),
            };
            let subtitle = format!("{version} · {progress}");
            Status {
                fraction,
                notes: Some((version.clone(), notes.clone())),
                ..plain("Downloading the update…", &subtitle)
            }
        }
        Some(AppUpdateState::Ready { version, notes }) => Status {
            notes: Some((version.clone(), notes.clone())),
            ..plain(&format!("Version {version} is ready"), "Restart Local Studio to finish the update.")
        },
        Some(AppUpdateState::Applying { version }) => Status {
            busy: true,
            ..plain(&format!("Installing version {version}…"), "Local Studio restarts when the install finishes.")
        },
        Some(AppUpdateState::Restarting { version }) => {
            Status { busy: true, ..plain(&format!("Restarting into version {version}…"), "") }
        }
        Some(AppUpdateState::Failed { reason, version, notes }) => Status {
            failed: true,
            notes: version.clone().map(|version| (version, notes.clone())),
            ..plain(
                &version.as_ref().map_or_else(
                    || "The update check failed".to_string(),
                    |version| format!("Could not update to version {version}"),
                ),
                reason,
            )
        },
        Some(AppUpdateState::Unavailable { reason }) => plain("Updates are off for this build", reason),
    }
}
