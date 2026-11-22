use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{ColorScheme, Settings, Shelf, ThreadStatus, Update};

use crate::app::App;
use crate::fixtures::{
    conversation, machines, paired_devices, projects, remote_projects, remote_threads, threads, FOCUSED,
};
use crate::format;

pub const SCENES: [&str; 15] = [
    "60-sidebar-machines",
    "60-sidebar-machines-light",
    "61-sidebar-machines-slim",
    "62-settings-connections",
    "62-settings-connections-light",
    "62-settings-connections-end",
    "62-settings-connections-end-light",
    "63-settings-connections-empty",
    "64-pairing",
    "64-pairing-light",
    "65-pairing-loopback",
    "66-pairing-not-serving",
    "67-add-machine",
    "68-add-machine-pasted",
    "68-add-machine-pasted-light",
];

const LINK_HOST: &str = "wss://studio-mac.tail0000.ts.net/v1/ws";
const LOOPBACK: &str = "ws://127.0.0.1:18093/v1/ws";

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| if byte.is_ascii_alphanumeric() { char::from(byte).to_string() } else { format!("%{byte:02X}") })
        .collect()
}

fn link(url: &str) -> String {
    format!(
        "localstudio-dev://pair?url={}&name=studio-mac&id=3f9d2c7a1b8e4d60#q2Vx0mJm1bN3c4Xr8tLk2wQe9yUo7pAs5dFg6hJk3lM",
        encode(url)
    )
}

pub fn scroll_to_end(root: &gtk::Widget) {
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            if scroller.is_mapped() {
                let adjustment = scroller.vadjustment();
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            stack.push(current);
        }
    }
}

fn sidebar(slim: bool) -> Vec<Update> {
    let mut remote = remote_threads();
    let mut local = threads();
    if slim {
        for row in &mut remote {
            row.shelf = Shelf::Settled;
            row.status = ThreadStatus::Idle;
        }
        local.retain(|row| row.id == FOCUSED || row.shelf == Shelf::Settled);
    }
    remote.extend(local);
    vec![
        Update::Projects { projects: projects().into_iter().chain(remote_projects()).collect() },
        Update::Threads { threads: remote },
        Update::Machines { machines: machines() },
    ]
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if !SCENES.contains(&scene) {
        return false;
    }
    conversation(app, false);
    if scene.ends_with("-light") {
        let settings = Settings { color_scheme: ColorScheme::Light, ..Settings::default() };
        app.apply(vec![Update::Settings { settings }]);
    }
    match scene.trim_end_matches("-light") {
        "60-sidebar-machines" => app.apply(sidebar(false)),
        "61-sidebar-machines-slim" => app.apply(sidebar(true)),
        "62-settings-connections" | "62-settings-connections-end" => {
            app.apply(vec![
                Update::Machines { machines: machines() },
                Update::PairedDevices { devices: paired_devices() },
            ]);
            app.open_settings(Some("connections"));
            if scene.contains("-end") {
                let weak = Rc::downgrade(app);
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    if let Some(dialog) = weak.upgrade().and_then(|app| app.window.visible_dialog()) {
                        scroll_to_end(dialog.upcast_ref());
                    }
                });
            }
        }
        "63-settings-connections-empty" => {
            app.apply(vec![Update::Machines { machines: Vec::new() }, Update::PairedDevices { devices: Vec::new() }]);
            app.open_settings(Some("connections"));
        }
        "64-pairing" | "65-pairing-loopback" | "66-pairing-not-serving" => {
            let base = scene.trim_end_matches("-light");
            let url = if base == "65-pairing-loopback" { LOOPBACK } else { LINK_HOST };
            crate::machines::open_pairing(app);
            let request = app.state.borrow().pairing.clone().unwrap_or_default();
            app.apply(vec![Update::Pairing {
                request,
                link: link(url),
                url: url.to_string(),
                serving: base != "66-pairing-not-serving",
                expires_at: format::now_ms() + 540_000,
            }]);
        }
        "67-add-machine" => {
            crate::machines::add_machine(app);
        }
        "68-add-machine-pasted" => {
            crate::machines::add_machine(app).set_text(&link(LINK_HOST));
        }
        _ => {}
    }
    true
}
