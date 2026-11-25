use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{AppUpdateState, ChannelInstall, ColorScheme, Settings, Update};

use crate::app::App;
use crate::fixtures::conversation;

pub const SCENES: [&str; 11] = [
    "updates-up-to-date",
    "updates-available",
    "updates-downloading",
    "updates-ready",
    "updates-ready-light",
    "updates-failed",
    "updates-unavailable",
    "updates-channel-menu",
    "updates-header-ready",
    "updates-header-ready-light",
    "updates-header-applying",
];

const VERSION: &str = "0.4.0-nightly.20261121";
const NEXT: &str = "0.4.1-nightly.20261122";
const NOTES: &str = "## Highlights\n\n- Signed in-app updates on every channel, checked on launch and every 6 hours\n- Settings shows the update state, the release notes and a Restart to Update button\n- Switching channel opens the other app, or its download page when it is not installed\n\n## Fixes\n\n- The sidebar keeps its width after a restart\n- Pasting into a terminal no longer drops the last line\n- Startup is faster when many projects are open\n- Streaming replies redraw less often";

fn channels() -> Vec<ChannelInstall> {
    [("stable", "Local Studio", true), ("nightly", "Local Studio Nightly", true), ("dev", "Local Studio Dev", false)]
        .into_iter()
        .map(|(channel, name, installed)| ChannelInstall {
            channel: channel.to_string(),
            name: name.to_string(),
            installed,
            current: channel == "nightly",
        })
        .collect()
}

fn state(scene: &str) -> AppUpdateState {
    let notes = NOTES.to_string();
    match scene {
        "updates-up-to-date" => AppUpdateState::UpToDate { latest: VERSION.to_string() },
        "updates-available" => AppUpdateState::Available { version: NEXT.to_string(), notes },
        "updates-downloading" => AppUpdateState::Downloading {
            version: NEXT.to_string(),
            notes,
            received: 12_400_000,
            total: Some(31_000_000),
        },
        "updates-failed" => AppUpdateState::Failed {
            reason: "The download's signature was refused: the signature does not match the nightly channel key."
                .to_string(),
            version: Some(NEXT.to_string()),
            notes,
        },
        "updates-unavailable" => AppUpdateState::Unavailable {
            reason: "This is a local build. Local builds never update themselves.".to_string(),
        },
        "updates-header-applying" => AppUpdateState::Applying { version: NEXT.to_string() },
        _ => AppUpdateState::Ready { version: NEXT.to_string(), notes },
    }
}

fn reveal(app: &Rc<App>, open_menu: bool) {
    let weak = Rc::downgrade(app);
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(dialog) = weak.upgrade().and_then(|app| app.window.visible_dialog()) else { return };
        let root: gtk::Widget = dialog.upcast();
        let Some(group) = crate::picker::find::<adw::PreferencesGroup>(&root, |group| {
            group.title() == "Updates" && group.is_mapped()
        }) else {
            return;
        };
        let Some(scroller) = group.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>()
        else {
            return;
        };
        let Some(content) = scroller.child() else { return };
        if let Some(point) = group.compute_point(&content, &gtk::graphene::Point::new(0.0, 0.0)) {
            let adjustment = scroller.vadjustment();
            adjustment.set_value((f64::from(point.y()) - 24.0).clamp(0.0, adjustment.upper() - adjustment.page_size()));
        }
        if open_menu {
            if let Some(button) = crate::picker::find::<gtk::MenuButton>(group.upcast_ref(), WidgetExt::is_mapped) {
                button.popup();
            }
        }
    });
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if !SCENES.contains(&scene) {
        return false;
    }
    conversation(app, false);
    let base = scene.trim_end_matches("-light");
    let automatic = !matches!(base, "updates-available" | "updates-unavailable");
    let color_scheme = if scene.ends_with("-light") { ColorScheme::Light } else { ColorScheme::System };
    let settings = Settings { automatic_updates: automatic, color_scheme, ..Settings::default() };
    {
        let mut state = app.state.borrow_mut();
        state.channels = channels();
        state.build = Some(if base == "updates-unavailable" {
            ("dev".to_string(), "0.4.0-dev.0+local.78494ed".to_string())
        } else {
            ("nightly".to_string(), VERSION.to_string())
        });
    }
    app.apply(vec![Update::Settings { settings }, Update::AppUpdate { state: state(base) }]);
    if !base.starts_with("updates-header") {
        app.open_settings(Some("general"));
        reveal(app, base == "updates-channel-menu");
    }
    true
}
