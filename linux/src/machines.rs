use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use local_studio_core::{Machine, MachineState, PairedDevice, Studio, TimeFormat};

use crate::app::{App, Target};
use crate::format;
use crate::icons;
use crate::state::State;

const PAIRED_FOOTER: &str = "A paired phone or computer sees every thread on this computer, reads them live, sends messages and answers approvals. Revoke a device to close its connection and make it pair again.";
const MACHINES_FOOTER: &str = "Threads on linked machines appear in the sidebar beside this computer's, marked with the machine's name. Pair them the way a phone pairs: open Pair a Device on that machine and paste its link here.";
const COMMAND: &str = "<tt>local-studio-serve install --tailscale</tt>";

thread_local! {
    static PAIRING: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub link: String,
    pub url: String,
    pub serving: bool,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMachine {
    pub name: String,
    pub icon: &'static str,
    pub reachable: bool,
    pub status: String,
}

impl RowMachine {
    pub fn new(state: &State, id: &str) -> Self {
        let machine = state.machines.iter().find(|machine| machine.id == id);
        Self {
            name: machine.map_or_else(|| id.to_string(), |machine| machine.name.clone()),
            icon: icon(machine.map_or("", |machine| machine.os.as_str())),
            reachable: machine.is_some_and(|machine| machine.state == MachineState::Connected),
            status: match machine {
                Some(machine) if machine.state == MachineState::Connected => String::new(),
                Some(machine) => status(machine, state.settings.time_format),
                None => "not linked".to_string(),
            },
        }
    }

    pub fn tooltip(&self) -> String {
        if self.reachable {
            format!("On {}", self.name)
        } else {
            format!("On {}, {}", self.name, self.status)
        }
    }
}

pub fn icon(platform: &str) -> &'static str {
    match platform.to_lowercase().as_str() {
        "ios" | "iphone" | "iphoneos" | "ipados" | "ipad" => icons::DEVICE,
        _ => icons::COMPUTER,
    }
}

fn platform_name(platform: &str) -> Option<String> {
    let name = match platform.to_lowercase().as_str() {
        "ios" | "iphone" | "iphoneos" => "iPhone",
        "ipados" | "ipad" => "iPad",
        "macos" | "mac" | "darwin" => "Mac",
        "linux" => "Linux",
        "windows" => "Windows",
        "" => return None,
        _ => return Some(platform.to_string()),
    };
    Some(name.to_string())
}

pub fn status(machine: &Machine, time: TimeFormat) -> String {
    match &machine.state {
        MachineState::Connecting => "Connecting…".to_string(),
        MachineState::Connected => {
            let latency = machine.latency_ms.map(|ms| format!(" · {ms} ms")).unwrap_or_default();
            format!("Connected{latency}")
        }
        MachineState::Offline { since } => format!("Offline since {}, retrying", clock(*since, time)),
        MachineState::PairAgain => format!("Pair again: {} no longer accepts this computer", machine.name),
        MachineState::Incompatible { protocol, min_protocol } => {
            let spoken = if min_protocol == protocol {
                format!("protocol {protocol}")
            } else {
                format!("protocols {min_protocol} to {protocol}")
            };
            format!("Update Local Studio: {} speaks {spoken}", machine.name)
        }
        MachineState::Refused { until } => {
            format!("Refused after failed attempts, retrying at {}", clock(*until, time))
        }
    }
}

fn dot_class(state: &MachineState) -> &'static str {
    match state {
        MachineState::Connected => "serving",
        MachineState::Connecting | MachineState::Offline { .. } => "loading",
        MachineState::PairAgain | MachineState::Incompatible { .. } | MachineState::Refused { .. } => "failed",
    }
}

fn clock(millis: i64, time: TimeFormat) -> String {
    let pattern = if time == TimeFormat::TwelveHour { "%-l:%M %p" } else { "%H:%M" };
    glib::DateTime::from_unix_local(millis.div_euclid(1000))
        .ok()
        .and_then(|at| at.format(pattern).ok())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

fn ago(at: i64, now: i64) -> String {
    let seconds = (now - at).max(0) / 1000;
    let (count, unit) = match seconds {
        0..60 => return "now".to_string(),
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        86_400..172_800 => return "yesterday".to_string(),
        172_800..604_800 => (seconds / 86_400, "day"),
        604_800..2_592_000 => (seconds / 604_800, "week"),
        2_592_000..31_536_000 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

fn device_subtitle(device: &PairedDevice, now: i64) -> String {
    let parts: Vec<String> = [
        platform_name(&device.platform),
        (!device.app.is_empty()).then(|| device.app.clone()),
        Some(format!("last seen {}", ago(device.last_seen, now))),
    ]
    .into_iter()
    .flatten()
    .collect();
    parts.join(" · ")
}

fn reachable(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    !host.is_empty() && !matches!(host.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

fn expiry(expires_at: i64, now: i64) -> String {
    let left = (expires_at - now).div_euclid(1000);
    if left <= 0 {
        return "This code has expired. Close and open Pair a Device for a new one.".to_string();
    }
    format!("Works once, for the next {}:{:02}.", left / 60, left % 60)
}

fn suffix_button(label: &str) -> gtk::Button {
    gtk::Button::builder().label(label).valign(gtk::Align::Center).build()
}

fn footer(group: &adw::PreferencesGroup, text: &str) {
    let label = gtk::Label::builder().label(text).wrap(true).xalign(0.0).margin_top(10).build();
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    group.add(&label);
}

fn glyph(platform: &str) -> gtk::Image {
    let image = icons::styled(icon(platform), "dim-label");
    image.set_valign(gtk::Align::Center);
    image
}

pub fn groups(app: &Weak<App>, state: &State) -> (Vec<adw::PreferencesGroup>, Vec<(String, adw::ActionRow)>) {
    let mut rows = Vec::new();
    let groups = vec![devices_group(app, state), machines_group(app, state, &mut rows)];
    (groups, rows)
}

fn devices_group(app: &Weak<App>, state: &State) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Paired Devices").build();
    let pair = adw::ActionRow::builder()
        .title("Pair a device")
        .subtitle("Shows a code to scan with Local Studio on iPhone, or a link to paste on another computer.")
        .build();
    let button = suffix_button("Pair a Device…");
    let weak = app.clone();
    button.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            open_pairing(&app);
        }
    });
    pair.add_suffix(&button);
    group.add(&pair);
    if state.devices.is_empty() {
        let text = if state.devices_loaded { "No devices are paired with this computer." } else { "Loading…" };
        group.add(&adw::ActionRow::builder().title(text).build());
    }
    let now = format::now_ms();
    for device in &state.devices {
        let row = adw::ActionRow::builder()
            .title(format::escape(&device.name))
            .subtitle(format::escape(&device_subtitle(device, now)))
            .build();
        row.add_prefix(&glyph(&device.platform));
        let revoke = suffix_button("Revoke…");
        let weak = app.clone();
        let (id, name) = (device.id.clone(), device.name.clone());
        revoke.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let id = id.clone();
            crate::actions::confirm_action(
                &app,
                &format!("Revoke {name}?"),
                "Its connection closes now, and it has to pair again to reach this computer.",
                "Revoke",
                true,
                move |app| {
                    let id = id.clone();
                    app.core.run(|studio| studio.revoke_paired_device(id));
                },
            );
        });
        row.add_suffix(&revoke);
        group.add(&row);
    }
    footer(&group, PAIRED_FOOTER);
    group
}

fn machines_group(app: &Weak<App>, state: &State, rows: &mut Vec<(String, adw::ActionRow)>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Other Machines").build();
    if state.machines.is_empty() {
        group.add(&adw::ActionRow::builder().title("No other machines are linked.").build());
    }
    for machine in &state.machines {
        let row = adw::ActionRow::builder()
            .title(format::escape(&machine.name))
            .subtitle(format::escape(&status(machine, state.settings.time_format)))
            .subtitle_selectable(true)
            .tooltip_text(&machine.url)
            .build();
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("ls-machine-dot");
        dot.add_css_class(dot_class(&machine.state));
        dot.set_valign(gtk::Align::Center);
        row.add_prefix(&dot);
        row.add_prefix(&glyph(&machine.os));
        let forget = suffix_button("Forget…");
        let weak = app.clone();
        let (id, name) = (machine.id.clone(), machine.name.clone());
        forget.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let id = id.clone();
            crate::actions::confirm_action(
                &app,
                &format!("Forget {name}?"),
                "Its threads leave the sidebar. Nothing changes on that machine; pair again to bring them back.",
                "Forget",
                true,
                move |app| {
                    let id = id.clone();
                    app.core.run(|studio| studio.forget_machine(id));
                },
            );
        });
        row.add_suffix(&forget);
        group.add(&row);
        rows.push((machine.id.clone(), row));
    }
    let add = adw::ActionRow::builder().title("Add a machine").build();
    let button = suffix_button("Add a Machine…");
    let weak = app.clone();
    button.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            add_machine(&app);
        }
    });
    add.add_suffix(&button);
    group.add(&add);
    footer(&group, MACHINES_FOOTER);
    group
}

pub fn open_pairing(app: &Rc<App>) {
    let request = app.ask(Target::Pairing);
    {
        let mut state = app.state.borrow_mut();
        state.pairing = Some(request.clone());
        state.offer = None;
        state.offer_error = None;
    }
    let dialog = adw::AlertDialog::new(Some("Pair a Device"), None);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_width_request(380);
    dialog.set_extra_child(Some(&content));
    dialog.add_responses(&[("done", "Done")]);
    dialog.set_default_response(Some("done"));
    dialog.set_close_response("done");
    PAIRING.with(|slot| slot.replace(Some(content)));
    let weak = Rc::downgrade(app);
    dialog.connect_closed(move |_| {
        PAIRING.with(|slot| slot.replace(None));
        let Some(app) = weak.upgrade() else { return };
        let closed = {
            let mut state = app.state.borrow_mut();
            state.offer = None;
            state.offer_error = None;
            state.pairing.take()
        };
        if let Some(request) = closed {
            app.core.run(|studio| studio.close_pairing(request));
        }
        app.core.run(Studio::list_paired_devices);
    });
    render_pairing(app);
    dialog.present(Some(&app.window));
    app.core.run(|studio| studio.open_pairing(request));
}

pub fn pairing_failed(app: &App, request: &str, message: &str) {
    {
        let mut state = app.state.borrow_mut();
        if state.pairing.as_deref() != Some(request) {
            return;
        }
        state.offer_error = Some(message.to_string());
    }
    render_pairing(app);
}

pub fn render_pairing(app: &App) {
    let Some(content) = PAIRING.with(|slot| slot.borrow().clone()) else { return };
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
    let state = app.state.borrow();
    if let Some(offer) = &state.offer {
        offer_view(app, &content, offer);
    } else if let Some(error) = &state.offer_error {
        let label = gtk::Label::builder().label(error).wrap(true).selectable(true).xalign(0.0).build();
        label.add_css_class("error");
        content.append(&label);
    } else {
        let spinner = adw::Spinner::builder().height_request(220).build();
        content.append(&spinner);
    }
}

fn offer_view(app: &App, content: &gtk::Box, offer: &Offer) {
    if let Some(code) = qr(&offer.link) {
        content.append(&code);
    }
    let explain = gtk::Label::builder()
        .label("Scan the code with Local Studio on iPhone, or copy the link into Add a Machine on another computer.")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    explain.add_css_class("dim-label");
    content.append(&explain);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let shown = gtk::Label::builder()
        .label(&offer.link)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .selectable(true)
        .hexpand(true)
        .xalign(0.0)
        .build();
    shown.add_css_class("ls-mono");
    line.append(&shown);
    let copy = suffix_button("Copy Link");
    let (window, text) = (app.window.clone(), offer.link.clone());
    copy.connect_clicked(move |button| {
        window.clipboard().set_text(&text);
        button.set_label("Copied");
    });
    line.append(&copy);
    content.append(&line);
    if let Some(dialog) = content.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
        dialog.set_focus(Some(&copy));
    }
    let countdown = gtk::Label::new(Some(&expiry(offer.expires_at, format::now_ms())));
    countdown.add_css_class("dim-label");
    countdown.add_css_class("caption");
    let (weak, expires_at) = (countdown.downgrade(), offer.expires_at);
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(label) = weak.upgrade() else { return glib::ControlFlow::Break };
        label.set_label(&expiry(expires_at, format::now_ms()));
        glib::ControlFlow::Continue
    });
    content.append(&countdown);
    if !offer.serving {
        content.append(&note(&format!(
            "This computer is not serving remote devices yet, so the code will not connect. Run {COMMAND} in a terminal, then open this again."
        )));
    } else if !reachable(&offer.url) {
        content.append(&note(&format!(
            "The link points at {}, which only this computer can reach. Turn on Tailscale and run {COMMAND} so other devices can reach it.",
            format::escape(&offer.url)
        )));
    }
}

fn note(markup: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let image = icons::styled(icons::WARNING, "warning");
    image.set_valign(gtk::Align::Start);
    image.set_margin_top(2);
    row.append(&image);
    let label = gtk::Label::builder().wrap(true).xalign(0.0).hexpand(true).build();
    label.set_markup(markup);
    row.append(&label);
    row
}

#[allow(clippy::cast_precision_loss)]
fn qr(link: &str) -> Option<gtk::DrawingArea> {
    let code = qrcode::QrCode::with_error_correction_level(link.as_bytes(), qrcode::EcLevel::M).ok()?;
    let width = code.width();
    let dark: Vec<bool> = code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect();
    let area = gtk::DrawingArea::builder()
        .content_width(220)
        .content_height(220)
        .halign(gtk::Align::Center)
        .accessible_role(gtk::AccessibleRole::Img)
        .build();
    area.update_property(&[gtk::accessible::Property::Label("Pairing code")]);
    area.set_draw_func(move |_, cr, w, h| {
        let size = f64::from(w.min(h));
        let radius = 8.0;
        cr.new_sub_path();
        cr.arc(size - radius, radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
        cr.arc(size - radius, size - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
        cr.arc(radius, size - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
        cr.arc(radius, radius, radius, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
        cr.close_path();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().ok();
        let cell = ((size - 20.0) / width as f64).floor().max(1.0);
        let origin = ((size - cell * width as f64) / 2.0).round();
        cr.set_antialias(gtk::cairo::Antialias::None);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        for (index, _) in dark.iter().enumerate().filter(|(_, dark)| **dark) {
            let (x, y) = ((index % width) as f64, (index / width) as f64);
            cr.rectangle(origin + x * cell, origin + y * cell, cell, cell);
        }
        cr.fill().ok();
    });
    Some(area)
}

pub fn add_machine(app: &Rc<App>) -> gtk::TextBuffer {
    let dialog = adw::AlertDialog::new(Some("Add a Machine"), None);
    dialog.set_body_use_markup(true);
    dialog.set_body(
        "On the other machine, open Settings › Connections › Pair a Device (or run <tt>local-studio-serve pair</tt>), copy the link and paste it here.",
    );
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::Char)
        .accepts_tab(false)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .build();
    view.add_css_class("ls-mono");
    view.add_css_class("ls-link-entry");
    view.update_property(&[gtk::accessible::Property::Label("Pairing link")]);
    let placeholder = gtk::Label::builder()
        .label("Pairing link")
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(8)
        .margin_start(10)
        .can_target(false)
        .build();
    placeholder.add_css_class("dim-label");
    placeholder.add_css_class("ls-mono");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(64)
        .max_content_height(110)
        .propagate_natural_height(true)
        .width_request(400)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroller));
    overlay.add_overlay(&placeholder);
    overlay.add_css_class("card");
    overlay.set_overflow(gtk::Overflow::Hidden);
    dialog.set_extra_child(Some(&overlay));
    dialog.add_responses(&[("cancel", "Cancel"), ("pair", "Pair")]);
    dialog.set_response_appearance("pair", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("pair"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("pair", false);
    let buffer = view.buffer();
    let (enabling, hint) = (dialog.clone(), placeholder);
    buffer.connect_changed(move |buffer| {
        let empty = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().is_empty();
        enabling.set_response_enabled("pair", !empty);
        hint.set_visible(buffer.char_count() == 0);
    });
    let weak = Rc::downgrade(app);
    let typed = buffer.clone();
    dialog.connect_response(Some("pair"), move |_, _| {
        let Some(app) = weak.upgrade() else { return };
        let link = typed.text(&typed.start_iter(), &typed.end_iter(), false).trim().to_string();
        if !link.is_empty() {
            app.core.run(|studio| studio.pair_machine(link));
        }
    });
    let pasted = buffer.clone();
    app.window.clipboard().read_text_async(None::<&gio::Cancellable>, move |result| {
        if let Ok(Some(text)) = result {
            if text.contains("://pair?") && pasted.char_count() == 0 {
                pasted.set_text(text.trim());
            }
        }
    });
    dialog.present(Some(&app.window));
    view.grab_focus();
    buffer
}
