use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{glib, pango};
use local_studio_core::{
    LocalAction, LocalCluster, LocalController, LocalEject, LocalEjectState, LocalGpu, LocalHotplug, LocalLendMode,
    LocalLendState, LocalPool, LocalPoolModel, LocalPoolPhase, LocalPowerHelper, LocalPowerLever, LocalThunderbolt,
    LocalThunderboltState,
};

use crate::app::App;
use crate::dashboard_parts::{self as parts, label};
use crate::icons;
use crate::numbers::gigabytes;

const MIB: u64 = 1_048_576;
const HELPER_COMMAND: &str = "local-studio fleet power-helper --yes";
const COMMIT_DELAY: Duration = Duration::from_millis(400);
const AWAY_LOW: f64 = 5.0;
// The controller refuses budgets above this (contracts.ts `Watts`, `AwayWatts`).
const MAX_WATTS: f64 = 2000.0;
const PRESENCE_UNKNOWN: &str = "This desktop doesn't report when you're at it, so the away limit applies.";

fn row(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn wrapped(text: &str, classes: &[&str]) -> gtk::Label {
    let label = label(text, classes);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label
}

fn title(text: &str, detail: Option<&str>) -> gtk::Box {
    let line = row(8);
    line.set_margin_bottom(4);
    let heading = label(text, &["ls-dash-label", "heading"]);
    line.append(&heading);
    if let Some(detail) = detail {
        line.append(&label(detail, &["dim-label", "ls-dash-label"]));
    }
    line
}

fn mark(ok: bool, warn: bool) -> gtk::Image {
    let (icon, class) = if ok {
        (icons::DONE, "success")
    } else if warn {
        (icons::WARNING, "warning")
    } else {
        (icons::ERROR, "error")
    };
    let image = icons::styled(icon, class);
    image.set_valign(gtk::Align::Start);
    icons::describe(&image, if ok { "Passes" } else { "Fails" });
    image
}

fn lend_dot(state: LocalLendState) -> gtk::Box {
    let dot = row(0);
    dot.add_css_class("ls-machine-dot");
    dot.add_css_class(match state {
        LocalLendState::Lent => "serving",
        LocalLendState::Reclaimed => "loading",
        LocalLendState::Off => "idle",
    });
    dot.set_valign(gtk::Align::Center);
    dot
}

pub fn lend_title(state: LocalLendState) -> &'static str {
    match state {
        LocalLendState::Lent => "Lending its GPUs",
        LocalLendState::Reclaimed => "Took its GPUs back",
        LocalLendState::Off => "Not lending",
    }
}

fn act(app: &Weak<App>, action: LocalAction, key: String) {
    if let Some(app) = app.upgrade() {
        app.local.act(&app, action, key);
    }
}

pub struct ClusterView {
    pub root: gtk::Box,
    app: Weak<App>,
    shown: RefCell<String>,
    dragging: Rc<Cell<bool>>,
}

struct PowerDraft {
    app: Weak<App>,
    target: String,
    top: f64,
    // Only a slider the user moved is sent; the other keeps the controller's value as stored.
    active: Cell<Option<f64>>,
    away: Cell<Option<f64>>,
    current: (u32, Option<u32>),
    timer: RefCell<Option<glib::SourceId>>,
    held: Cell<bool>,
    dragging: Rc<Cell<bool>>,
}

impl PowerDraft {
    // A pointer drag commits on release; keyboard steps commit after a short pause.
    fn schedule(self: &Rc<Self>) {
        self.dragging.set(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        if self.held.get() {
            return;
        }
        let draft = self.clone();
        let source = glib::timeout_add_local_once(COMMIT_DELAY, move || {
            draft.timer.borrow_mut().take();
            draft.commit();
        });
        self.timer.replace(Some(source));
    }

    fn press(&self) {
        self.held.set(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn release(self: &Rc<Self>) {
        self.held.set(false);
        if self.dragging.get() {
            self.commit();
        }
    }

    fn commit(&self) {
        self.dragging.set(false);
        let (active, away) = budget(self.active.get(), self.away.get(), self.top, self.current);
        if (active, away) == self.current {
            return;
        }
        act(
            &self.app,
            LocalAction::SetPowerBudget { controller: self.target.clone(), active_watts: active, away_watts: away },
            format!("power:{}", self.target),
        );
    }
}

fn whole(watts: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = watts.round().clamp(0.0, MAX_WATTS) as u32;
    whole
}

fn snap(watts: f64) -> f64 {
    (watts / 5.0).round() * 5.0
}

// The top stop: the GPUs' max limit (or a guess without one), raised to fit what is stored so a
// saved cap never reads as "No limit", and never past what the controller accepts.
fn power_top(controller: &LocalController, cluster: &LocalCluster) -> f64 {
    let power = &cluster.power;
    let has_gpu = controller.hardware.as_ref().is_some_and(|hardware| !hardware.gpus.is_empty());
    top_for(power.max_w, has_gpu, power.active_watts, power.away_watts)
}

fn top_for(max_w: Option<f64>, has_gpu: bool, active: u32, away: Option<u32>) -> f64 {
    let base = match max_w.filter(|max| *max > 0.0) {
        Some(max) => max,
        None if has_gpu => 150.0,
        None => 30.0,
    };
    let stored = f64::from(active).max(away.map_or(0.0, |watts| f64::from(watts) + 5.0));
    ((base.max(stored) / 5.0).ceil() * 5.0).min(MAX_WATTS)
}

// What a commit sends: moved sliders as drawn (the away slider's top stop is no limit), the rest as stored.
fn budget(active: Option<f64>, away: Option<f64>, top: f64, current: (u32, Option<u32>)) -> (u32, Option<u32>) {
    (active.map_or(current.0, whole), away.map_or(current.1, |watts| (watts < top).then(|| whole(watts))))
}

fn active_caption(watts: f64) -> String {
    if watts <= 0.0 {
        "Off".to_string()
    } else {
        format!("Up to {} W", whole(watts))
    }
}

fn away_caption(watts: f64, top: f64) -> String {
    if watts >= top {
        "No limit".to_string()
    } else {
        format!("Up to {} W", whole(watts))
    }
}

fn power_status(cluster: &LocalCluster) -> Option<String> {
    let power = &cluster.power;
    let mut parts: Vec<String> = Vec::new();
    match power.present {
        Some(true) => parts.push("You're using it".to_string()),
        Some(false) => parts.push("You're away".to_string()),
        None => {}
    }
    if let (LocalLendState::Lent, Some(cap)) = (cluster.state, power.cap_w) {
        parts.push(match power.measured_w {
            Some(measured) => format!("Drawing about {} W of {} W", whole(measured), whole(cap)),
            None => format!("Capped at {} W", whole(cap)),
        });
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn power_lever(cluster: &LocalCluster) -> Option<String> {
    match cluster.power.lever {
        LocalPowerLever::NvidiaLimit => Some("Uses the GPU's hardware power limit.".to_string()),
        LocalPowerLever::DutyCycle => Some("Pauses fleet work in short bursts to stay under the cap.".to_string()),
        LocalPowerLever::None => None,
    }
}

fn power_slider(
    title: &str,
    value: f64,
    (low, top): (f64, f64),
    caption: impl Fn(f64) -> String + 'static,
    busy: bool,
    draft: &Rc<PowerDraft>,
    changed: impl Fn(f64) + 'static,
) -> gtk::Box {
    let block = column(4);
    let head = row(8);
    let name = label(title, &["ls-dash-label"]);
    name.set_hexpand(true);
    head.append(&name);
    let shown = label(&caption(value), &["dim-label", "ls-dash-small", "numeric"]);
    head.append(&shown);
    block.append(&head);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, top, 5.0);
    scale.set_draw_value(false);
    scale.set_round_digits(0);
    scale.set_hexpand(true);
    scale.set_value(value);
    scale.set_sensitive(!busy);
    // Watch raw presses in the capture phase: the scale's own drag gesture claims the sequence,
    // which would cancel a click gesture before its release.
    let pointer = gtk::EventControllerLegacy::new();
    pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
    let grip = draft.clone();
    pointer.connect_event(move |_, event| {
        match event.event_type() {
            gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin => grip.press(),
            gtk::gdk::EventType::ButtonRelease | gtk::gdk::EventType::TouchEnd | gtk::gdk::EventType::TouchCancel => {
                grip.release();
            }
            _ => {}
        }
        glib::Propagation::Proceed
    });
    scale.add_controller(pointer);
    scale.update_property(&[
        gtk::accessible::Property::Label(title),
        gtk::accessible::Property::ValueText(&caption(value)),
    ]);
    scale.connect_value_changed(move |scale| {
        let snapped = snap(scale.value()).max(low);
        if (snapped - scale.value()).abs() > f64::EPSILON {
            scale.set_value(snapped);
            return;
        }
        let text = caption(snapped);
        shown.set_text(&text);
        scale.update_property(&[gtk::accessible::Property::ValueText(&text)]);
        changed(snapped);
    });
    block.append(&scale);
    block
}

impl ClusterView {
    pub fn build(app: &Weak<App>) -> Self {
        let root = column(36);
        Self { root, app: app.clone(), shown: RefCell::new(String::new()), dragging: Rc::new(Cell::new(false)) }
    }

    pub fn fill(&self, controller: &LocalController, pending: &dyn Fn(&str) -> bool) {
        if self.dragging.get() {
            return;
        }
        let target = controller.target.as_str();
        let keys: Vec<String> = [
            controller
                .hardware
                .iter()
                .flat_map(|hardware| &hardware.thunderbolt)
                .map(|device| format!("authorize:{target}:{}", device.uuid))
                .collect::<Vec<_>>(),
            controller
                .hardware
                .iter()
                .flat_map(|hardware| &hardware.gpus)
                .flat_map(|gpu| [false, true].map(|cancel| format!("eject:{target}:{}:{cancel}", gpu.index)))
                .collect(),
            vec![
                format!("lending:{target}"),
                format!("discovery:{target}"),
                format!("pool:{target}"),
                format!("power:{target}"),
            ],
            controller
                .cluster
                .iter()
                .flat_map(|cluster| &cluster.pools)
                .map(|pool| format!("stop-pool:{target}:{}", pool.id))
                .collect(),
        ]
        .concat();
        let busy: Vec<bool> = keys.iter().map(|key| pending(key)).collect();
        let key = format!("{target}{:?}{:?}{busy:?}{}", controller.hardware, controller.cluster, controller.local);
        if *self.shown.borrow() == key {
            return;
        }
        self.shown.replace(key);
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        self.root.set_visible(controller.hardware.is_some() || controller.cluster.is_some());
        if controller.hardware.is_some() {
            let top = row(44);
            top.set_homogeneous(controller.cluster.is_some());
            top.append(&self.gpus(controller, pending));
            if let Some(cluster) = &controller.cluster {
                top.append(&self.lending(controller, cluster, pending));
            }
            self.root.append(&top);
        }
        if let Some(cluster) = controller
            .cluster
            .as_ref()
            .filter(|cluster| !cluster.machines.is_empty() || !cluster.pools.is_empty() || !cluster.models.is_empty())
        {
            let bottom = row(44);
            bottom.set_homogeneous(true);
            bottom.append(&machines(cluster));
            bottom.append(&self.pools(controller, cluster, pending));
            self.root.append(&bottom);
        }
    }

    fn gpus(&self, controller: &LocalController, pending: &dyn Fn(&str) -> bool) -> gtk::Box {
        let list = column(10);
        list.set_hexpand(true);
        let gpus = controller.hardware.as_ref().map_or(&[][..], |hardware| hardware.gpus.as_slice());
        let external = gpus.iter().any(|gpu| gpu.external).then_some("one connected by cable");
        list.append(&title(&format!("GPUs on {}", controller.name), external));
        if let Some(note) = controller.hardware.as_ref().and_then(|hardware| hardware.egpu_note.as_deref()) {
            let line = row(6);
            line.append(&icons::styled(icons::INFO, "dim-label"));
            line.append(&wrapped(note, &["dim-label", "ls-dash-label"]));
            list.append(&line);
        }
        if gpus.is_empty() {
            list.append(&label("No NVIDIA GPU reported.", &["dim-label", "ls-dash-label"]));
        }
        for gpu in gpus {
            let line = gpu_row(gpu);
            list.append(&line);
            if let Some(eject) = gpu.eject.as_ref().filter(|_| controller.local) {
                self.eject(controller, gpu, eject, &line, &list, pending);
            }
        }
        let devices = controller.hardware.iter().flat_map(|hardware| &hardware.thunderbolt);
        for device in devices.filter(|device| device.state != LocalThunderboltState::Authorized) {
            list.append(&self.device_row(controller, device, pending));
        }
        let events = controller.hardware.iter().flat_map(|hardware| hardware.hotplug.iter().take(3));
        for event in events {
            list.append(&hotplug_row(event));
        }
        list
    }

    fn device_row(
        &self,
        controller: &LocalController,
        device: &LocalThunderbolt,
        pending: &dyn Fn(&str) -> bool,
    ) -> gtk::Box {
        let line = row(10);
        line.append(&mark(false, device.state != LocalThunderboltState::Failed));
        let text = column(2);
        text.set_hexpand(true);
        let name = [Some(device.name.as_str()), device.vendor.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        text.append(&label(&name, &["ls-dash-label"]));
        let status = match device.state {
            LocalThunderboltState::Pending if controller.local => {
                "Waiting for approval before its GPU can be used.".to_string()
            }
            LocalThunderboltState::Pending => {
                format!("Waiting for approval. Approve it on {} itself.", controller.name)
            }
            LocalThunderboltState::Authorizing => "Approving…".to_string(),
            LocalThunderboltState::Failed => "Approving this device failed.".to_string(),
            LocalThunderboltState::Authorized => "Approved".to_string(),
        };
        let message = wrapped(device.message.as_deref().unwrap_or(&status), &["ls-dash-small"]);
        message.add_css_class(if device.state == LocalThunderboltState::Failed { "error" } else { "dim-label" });
        text.append(&message);
        line.append(&text);
        if controller.local && device.state != LocalThunderboltState::Authorizing {
            let key = format!("authorize:{}:{}", controller.target, device.uuid);
            let busy = pending(&key);
            let caption = if busy {
                "Authorizing…"
            } else if device.state == LocalThunderboltState::Failed {
                "Try Again"
            } else {
                "Authorize"
            };
            let button = parts::button(&gtk::Label::new(Some(caption)), true);
            button.set_sensitive(!busy);
            button.set_tooltip_text(Some("Approve this Thunderbolt device so its GPU can be used"));
            let weak = self.app.clone();
            let (target, uuid) = (controller.target.clone(), device.uuid.clone());
            button.connect_clicked(move |_| {
                act(&weak, LocalAction::Authorize { controller: target.clone(), uuid: uuid.clone() }, key.clone());
            });
            line.append(&button);
        }
        line
    }

    fn eject(
        &self,
        controller: &LocalController,
        gpu: &LocalGpu,
        eject: &LocalEject,
        line: &gtk::Box,
        list: &gtk::Box,
        pending: &dyn Fn(&str) -> bool,
    ) {
        let key = |cancel: bool| format!("eject:{}:{}:{cancel}", controller.target, gpu.index);
        let (ejecting, cancelling) = (pending(&key(false)), pending(&key(true)));
        let busy = ejecting || cancelling;
        let button = |caption: &str, tip: &str, cancel: bool| {
            let caption = match (cancel, ejecting, cancelling) {
                (false, true, _) => "Ejecting…",
                (true, _, true) => "Cancelling…",
                _ => caption,
            };
            let button = parts::button(&gtk::Label::new(Some(caption)), true);
            button.set_valign(gtk::Align::Start);
            button.set_sensitive(!busy);
            button.set_tooltip_text(Some(tip));
            let weak = self.app.clone();
            let (target, index, key) = (controller.target.clone(), gpu.index, key(cancel));
            button.connect_clicked(move |_| {
                act(&weak, LocalAction::EjectGpu { controller: target.clone(), gpu: index, cancel }, key.clone());
            });
            button
        };
        if !eject.draining {
            if eject.state != LocalEjectState::Unsupported {
                line.append(&button(
                    "Eject",
                    "Unload its models and take it off the bus so it can be unplugged",
                    false,
                ));
            }
            return;
        }
        let block = column(4);
        block.set_margin_start(34);
        let text = match eject.state {
            LocalEjectState::Ready => "Ejecting…",
            _ => eject.message.as_deref().unwrap_or("Ejecting…"),
        };
        let message = wrapped(text, &["ls-dash-small", "warning"]);
        block.append(&message);
        for process in &eject.blockers {
            block.append(&label(&format!("pid {} · {}", process.pid, process.name), &["dim-label", "ls-dash-small"]));
        }
        if let Some(command) = &eject.command {
            let shown = label(command, &["ls-dash-small", "monospace"]);
            shown.set_selectable(true);
            shown.set_wrap(true);
            block.append(&shown);
        }
        let actions = row(8);
        if eject.state == LocalEjectState::Busy {
            actions.append(&button("Try Again", "Eject again once those programs have quit", false));
        }
        actions.append(&button("Cancel Eject", "Let this GPU load models again", true));
        block.append(&actions);
        list.append(&block);
    }

    fn lending(
        &self,
        controller: &LocalController,
        cluster: &LocalCluster,
        pending: &dyn Fn(&str) -> bool,
    ) -> gtk::Box {
        let block = column(10);
        block.set_hexpand(true);
        block.append(&title("Lending", None));
        let status = row(8);
        status.append(&lend_dot(cluster.state));
        status.append(&label(lend_title(cluster.state), &["ls-dash-label", "heading"]));
        if cluster.draining {
            status.append(&label("finishing requests", &["dim-label", "ls-dash-small"]));
        }
        block.append(&status);
        let reason = wrapped(&cluster.reason, &["ls-dash-label"]);
        reason.set_selectable(true);
        reason.add_css_class(if cluster.state == LocalLendState::Reclaimed { "warning" } else { "dim-label" });
        block.append(&reason);
        let modes = row(0);
        modes.add_css_class("linked");
        modes.set_halign(gtk::Align::Start);
        let lending = pending(&format!("lending:{}", controller.target));
        let mut leader: Option<gtk::ToggleButton> = None;
        for (mode, caption) in
            [(LocalLendMode::Never, "Never"), (LocalLendMode::Idle, "When idle"), (LocalLendMode::Always, "Always")]
        {
            let button = gtk::ToggleButton::with_label(caption);
            button.add_css_class("ls-small-toggle");
            button.set_active(cluster.lend == mode);
            button.set_sensitive(!lending);
            if let Some(leader) = &leader {
                button.set_group(Some(leader));
            } else {
                leader = Some(button.clone());
            }
            let weak = self.app.clone();
            let target = controller.target.clone();
            let current = cluster.lend;
            button.connect_toggled(move |button| {
                if button.is_active() && mode != current {
                    act(
                        &weak,
                        LocalAction::SetLending { controller: target.clone(), lend: mode },
                        format!("lending:{target}"),
                    );
                }
            });
            modes.append(&button);
        }
        let lend_line = row(10);
        lend_line.append(&label("Lend GPUs", &["ls-dash-label"]));
        lend_line.append(&modes);
        block.append(&lend_line);
        let discover = gtk::CheckButton::with_label("Find machines on the tailnet");
        discover.set_active(cluster.discover);
        discover.set_sensitive(!pending(&format!("discovery:{}", controller.target)));
        let weak = self.app.clone();
        let target = controller.target.clone();
        discover.connect_toggled(move |button| {
            let on = button.is_active();
            act(
                &weak,
                LocalAction::SetDiscovery { controller: target.clone(), discover: on },
                format!("discovery:{target}"),
            );
        });
        block.append(&discover);
        if cluster.lend != LocalLendMode::Never {
            for check in &cluster.checks {
                let line = row(6);
                line.append(&mark(check.ok, false));
                line.append(&label(&check.name, &["ls-dash-small"]));
                let detail = label(&check.detail, &["dim-label", "ls-dash-small"]);
                detail.set_ellipsize(pango::EllipsizeMode::End);
                line.append(&detail);
                block.append(&line);
            }
        }
        for server in &cluster.lent {
            let gpus = server.gpus.iter().map(|gpu| format!("G{gpu}")).collect::<Vec<_>>().join(", ");
            block.append(&label(&format!("Lent to {}, {gpus}", server.for_name), &["ls-dash-label"]));
        }
        if cluster.lend != LocalLendMode::Never {
            block.append(&self.power(controller, cluster, pending));
        }
        block
    }

    fn power(&self, controller: &LocalController, cluster: &LocalCluster, pending: &dyn Fn(&str) -> bool) -> gtk::Box {
        let power = &cluster.power;
        let block = column(10);
        block.set_margin_top(8);
        block.append(&title("Power for the fleet", None));
        let busy = pending(&format!("power:{}", controller.target));
        let top = power_top(controller, cluster);
        let active = f64::from(power.active_watts).min(top);
        // The controller takes 1 W and up while away; "stop lending" is the Lending control's job.
        let away = power.away_watts.map_or(top, |watts| f64::from(watts).clamp(AWAY_LOW, top));
        let draft = Rc::new(PowerDraft {
            app: self.app.clone(),
            target: controller.target.clone(),
            top,
            active: Cell::new(None),
            away: Cell::new(None),
            current: (power.active_watts, power.away_watts),
            timer: RefCell::new(None),
            held: Cell::new(false),
            dragging: self.dragging.clone(),
        });
        let using = draft.clone();
        block.append(&power_slider(
            "While you're using this computer",
            active,
            (0.0, top),
            active_caption,
            busy,
            &draft,
            move |watts| {
                using.active.set(Some(watts));
                using.schedule();
            },
        ));
        block.append(&wrapped(
            "Fleet work stops the moment you start a game or any GPU program.",
            &["dim-label", "ls-dash-small"],
        ));
        let leaving = draft.clone();
        block.append(&power_slider(
            "While you're away",
            away,
            (AWAY_LOW, top),
            move |watts| away_caption(watts, top),
            busy,
            &draft,
            move |watts| {
                leaving.away.set(Some(watts));
                leaving.schedule();
            },
        ));
        if power.present.is_none() {
            block.append(&wrapped(PRESENCE_UNKNOWN, &["dim-label", "ls-dash-small"]));
        }
        if let Some(status) = power_status(cluster) {
            let line = label(&status, &["dim-label", "ls-dash-small"]);
            line.set_ellipsize(pango::EllipsizeMode::End);
            block.append(&line);
        }
        if let Some(lever) = power_lever(cluster) {
            let line = wrapped(&lever, &["dim-label", "ls-dash-small"]);
            line.set_selectable(true);
            block.append(&line);
        }
        if power.helper == LocalPowerHelper::Missing {
            let line = row(10);
            let text = column(2);
            text.set_hexpand(true);
            text.append(&label("Use the hardware power limit", &["ls-dash-label"]));
            text.append(&wrapped("Needs a one-time administrator step.", &["dim-label", "ls-dash-small"]));
            line.append(&text);
            let copy = parts::button(&gtk::Label::new(Some("Copy")), true);
            copy.set_tooltip_text(Some(&format!("Copy {HELPER_COMMAND} to run in a terminal")));
            copy.connect_clicked(|button| button.clipboard().set_text(HELPER_COMMAND));
            line.append(&copy);
            block.append(&line);
        }
        block
    }

    fn pools(&self, controller: &LocalController, cluster: &LocalCluster, pending: &dyn Fn(&str) -> bool) -> gtk::Box {
        let block = column(10);
        block.set_hexpand(true);
        let own = if cluster.state == LocalLendState::Lent { cluster.free_mib } else { 0 };
        let pooled = cluster
            .machines
            .iter()
            .filter(|machine| machine.state == LocalLendState::Lent)
            .fold(own, |sum, machine| sum + machine.free_mib);
        block.append(&title("Pooled models", Some(&format!("{} GB lent across the fleet", gigabytes(pooled * MIB)))));
        for pool in &cluster.pools {
            block.append(&self.pool_row(controller, pool, pending));
        }
        if cluster.models.is_empty() {
            block.append(&wrapped(
                &format!(
                    "No GGUF models found on {}. Pooled models run with llama.cpp across every lending machine.",
                    controller.name
                ),
                &["dim-label", "ls-dash-label"],
            ));
        }
        let starting = pending(&format!("pool:{}", controller.target));
        for model in &cluster.models {
            block.append(&self.model_row(controller, model, starting));
        }
        block
    }

    fn pool_row(&self, controller: &LocalController, pool: &LocalPool, pending: &dyn Fn(&str) -> bool) -> gtk::Box {
        let line = row(10);
        line.append(&mark(pool.phase == LocalPoolPhase::Ready, pool.phase == LocalPoolPhase::Starting));
        let text = column(2);
        text.set_hexpand(true);
        let head = row(8);
        let alias = label(&pool.alias, &["ls-dash-label", "heading"]);
        alias.set_ellipsize(pango::EllipsizeMode::Middle);
        head.append(&alias);
        let phase = match pool.phase {
            LocalPoolPhase::Starting => "starting",
            LocalPoolPhase::Ready => "serving",
            LocalPoolPhase::Failed | LocalPoolPhase::Stopped => "stopped",
        };
        head.append(&label(phase, &["dim-label", "ls-dash-small"]));
        text.append(&head);
        let detail =
            pool.message.clone().unwrap_or_else(|| format!("On {} · port {}", pool.members.join(", "), pool.port));
        let detail = wrapped(&detail, &["ls-dash-small"]);
        detail.add_css_class(if pool.phase == LocalPoolPhase::Failed { "error" } else { "dim-label" });
        text.append(&detail);
        line.append(&text);
        if matches!(pool.phase, LocalPoolPhase::Starting | LocalPoolPhase::Ready) {
            let key = format!("stop-pool:{}:{}", controller.target, pool.id);
            let busy = pending(&key);
            let button = parts::button(&gtk::Label::new(Some(if busy { "Stopping…" } else { "Stop" })), false);
            button.set_sensitive(!busy);
            let weak = self.app.clone();
            let (target, id) = (controller.target.clone(), pool.id.clone());
            button.connect_clicked(move |_| {
                act(&weak, LocalAction::StopPool { controller: target.clone(), pool: id.clone() }, key.clone());
            });
            line.append(&button);
        }
        line
    }

    fn model_row(&self, controller: &LocalController, model: &LocalPoolModel, starting: bool) -> gtk::Box {
        let line = row(10);
        let text = column(2);
        text.set_hexpand(true);
        let name = label(&model.name, &["ls-dash-label"]);
        name.set_ellipsize(pango::EllipsizeMode::Middle);
        text.append(&name);
        let size = format!("{} GB", gigabytes(model.size_bytes));
        let fit = if model.fits_one {
            format!("{size}, fits on one machine")
        } else if model.fits_pool {
            format!("{size}, needs the pool")
        } else {
            format!("{size}, too large for the GPUs lent now")
        };
        text.append(&label(&fit, &["dim-label", "ls-dash-small"]));
        line.append(&text);
        let button =
            parts::button(&gtk::Label::new(Some(if starting { "Starting…" } else { "Run on the Pool" })), true);
        button.set_sensitive(model.fits_pool && !starting);
        button.set_tooltip_text(Some(if model.fits_pool {
            "Split this model across every lending machine"
        } else {
            "The lending machines do not have enough free GPU memory"
        }));
        let weak = self.app.clone();
        let (target, path) = (controller.target.clone(), model.path.clone());
        button.connect_clicked(move |_| {
            act(
                &weak,
                LocalAction::LaunchPool { controller: target.clone(), model: path.clone() },
                format!("pool:{target}"),
            );
        });
        line.append(&button);
        line
    }
}

fn gpu_row(gpu: &LocalGpu) -> gtk::Box {
    let line = row(10);
    let index = label(&format!("G{}", gpu.index), &["dim-label", "ls-dash-small", "ls-dash-num"]);
    index.set_width_chars(3);
    index.set_valign(gtk::Align::Start);
    line.append(&index);
    let text = column(3);
    text.set_hexpand(true);
    let head = row(8);
    let name = label(&gpu.name, &["ls-dash-label"]);
    name.set_ellipsize(pango::EllipsizeMode::End);
    head.append(&name);
    if gpu.external {
        let badge = label("eGPU", &["ls-egpu-badge"]);
        badge.set_valign(gtk::Align::Center);
        badge.set_tooltip_text(Some("Connected over Thunderbolt or USB4"));
        badge.update_property(&[gtk::accessible::Property::Label("External GPU")]);
        head.append(&badge);
    }
    text.append(&head);
    if gpu.memory_total_mib > 0 {
        let bar = gtk::ProgressBar::builder().margin_top(3).margin_bottom(3).build();
        #[allow(clippy::cast_precision_loss)]
        bar.set_fraction((gpu.memory_used_mib as f64 / gpu.memory_total_mib as f64).clamp(0.0, 1.0));
        text.append(&bar);
    }
    if let Some(link) = &gpu.link {
        text.append(&wrapped(link, &["dim-label", "ls-dash-small"]));
    }
    line.append(&text);
    let memory = label(
        &format!("{} / {} GB", gigabytes(gpu.memory_used_mib * MIB), gigabytes(gpu.memory_total_mib * MIB)),
        &["dim-label", "ls-dash-small", "ls-dash-num"],
    );
    memory.set_valign(gtk::Align::Start);
    line.append(&memory);
    line
}

fn hotplug_row(event: &LocalHotplug) -> gtk::Box {
    let block = column(2);
    let line = row(6);
    line.append(&icons::styled(
        if event.added { icons::ADD } else { icons::NONE },
        if event.added { "dim-label" } else { "warning" },
    ));
    let message = wrapped(&event.message, &["ls-dash-label"]);
    if !event.added {
        message.add_css_class("warning");
    }
    line.append(&message);
    block.append(&line);
    if !event.suggestions.is_empty() {
        let ideas =
            wrapped(&format!("Fits this card: {}", event.suggestions.join(", ")), &["dim-label", "ls-dash-small"]);
        ideas.set_margin_start(22);
        block.append(&ideas);
    }
    block
}

fn machines(cluster: &LocalCluster) -> gtk::Box {
    let block = column(10);
    block.set_hexpand(true);
    let lending = cluster.machines.iter().filter(|machine| machine.state == LocalLendState::Lent).count();
    block.append(&title("Machines in the fleet", Some(&format!("{lending} lending"))));
    if cluster.machines.is_empty() {
        block.append(&wrapped(
            "No other machine is linked yet. Turn on Find machines on the tailnet, or add one in Settings, Local.",
            &["dim-label", "ls-dash-label"],
        ));
    }
    for machine in &cluster.machines {
        let line = row(10);
        line.append(&lend_dot(machine.state));
        let text = column(2);
        text.set_hexpand(true);
        let head = row(8);
        head.append(&label(&machine.name, &["ls-dash-label", "heading"]));
        head.append(&label(lend_title(machine.state), &["dim-label", "ls-dash-small"]));
        text.append(&head);
        let reason = wrapped(&machine.reason, &["ls-dash-small"]);
        reason.add_css_class(if machine.state == LocalLendState::Reclaimed { "warning" } else { "dim-label" });
        text.append(&reason);
        line.append(&text);
        let gpus = if machine.gpus == 0 {
            "no GPU".to_string()
        } else {
            let plural = if machine.gpus == 1 { "" } else { "s" };
            format!("{} GPU{plural}, {} GB free", machine.gpus, gigabytes(machine.free_mib * MIB))
        };
        let figure = label(&gpus, &["dim-label", "ls-dash-small", "ls-dash-num"]);
        figure.set_valign(gtk::Align::Start);
        line.append(&figure);
        block.append(&line);
    }
    block
}

#[cfg(test)]
mod tests {
    use super::{budget, top_for};

    #[test]
    fn top_fits_a_stored_cap_above_the_guess() {
        // No maxW, one GPU: the guess is 150 W, but a stored 300 W away cap must not read as "No limit".
        let top = top_for(None, true, 60, Some(300));
        assert!((top - 305.0).abs() < f64::EPSILON, "top {top}");
    }

    #[test]
    fn top_never_passes_what_the_controller_accepts() {
        assert!((top_for(Some(2400.0), true, 0, None) - 2000.0).abs() < f64::EPSILON);
        assert!((top_for(Some(495.0), true, 60, None) - 495.0).abs() < f64::EPSILON);
    }

    #[test]
    fn moving_one_slider_keeps_the_other_as_stored() {
        // Stored: 60 W in use, 300 W away, on a 150 W top. Moving only the in-use slider keeps 300 W.
        assert_eq!(budget(Some(40.0), None, 150.0, (60, Some(300))), (40, Some(300)));
        // Moving only the away slider keeps the in-use watts even when they sit above the top.
        assert_eq!(budget(None, Some(100.0), 150.0, (200, None)), (200, Some(100)));
        // The away slider's top stop is no limit.
        assert_eq!(budget(None, Some(150.0), 150.0, (60, Some(100))), (60, None));
    }
}
