use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use local_studio_core::{ColorScheme, ConsentAnswer, ConsentNeed, ConsentRequest, ConsentRow, Settings, Update};

use crate::app::App;
use crate::format;
use crate::icons;

pub const SCENES: [&str; 4] = ["consent-install", "consent-install-light", "consent-findings", "consent-update-light"];

thread_local! {
    static SHOWN: RefCell<Option<(String, adw::Dialog)>> = const { RefCell::new(None) };
}

pub fn show(app: &Rc<App>, consents: &[ConsentRequest]) {
    let open = SHOWN.with(|slot| slot.borrow().clone());
    if let Some((request, dialog)) = open {
        if consents.iter().any(|consent| consent.request == request) {
            return;
        }
        SHOWN.with(|slot| slot.replace(None));
        dialog.force_close();
    }
    if let Some(consent) = consents.first() {
        present(app, consent, false);
    }
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class(class);
    label
}

fn subtitle(consent: &ConsentRequest) -> String {
    let description = consent.description.trim_end_matches(['.', ' ']);
    let lead = if description.is_empty() { String::new() } else { format!("{}. ", format::escape(description)) };
    let replacing = consent.update_from.as_ref().map(|from| format!(", replacing {}", format::escape(from)));
    let version = format!(
        "{lead}Version {}{} from {}",
        format::escape(&consent.version),
        replacing.map(|text| format!("{text},")).unwrap_or_default(),
        format::escape(&consent.source)
    );
    match &consent.pin {
        Some(pin) => format!("{version}, pinned to <tt>{}</tt>.", format::escape(pin)),
        None => format!("{version}."),
    }
}

fn scan_line(consent: &ConsentRequest) -> String {
    let files = if consent.files == 1 { "1 file".to_string() } else { format!("{} files", consent.files) };
    match consent.findings.len() {
        0 => format!("Install scan found nothing: {files}, no binaries outside the manifest."),
        1 => format!("Install scan: {files}, 1 finding to read before you install."),
        count => format!("Install scan: {files}, {count} findings to read before you install."),
    }
}

fn title(consent: &ConsentRequest) -> String {
    match (consent.install, &consent.update_from) {
        (false, _) => format!("Allow {} to run?", consent.name),
        (true, None) => format!("Install {}?", consent.name),
        (true, Some(_)) => format!("Update {}?", consent.name),
    }
}

fn confirm(consent: &ConsentRequest, count: usize) -> String {
    let noun = if count == 1 { "permission" } else { "permissions" };
    let off = count < consent.rows.len();
    if !consent.install {
        return if off { format!("Allow {count} {noun}, tools off") } else { format!("Allow {count} {noun}") };
    }
    let verb = if consent.update_from.is_some() { "Update" } else { "Install" };
    if off {
        format!("{verb} with tools off")
    } else if count == 0 {
        format!("{verb} with no new permissions")
    } else {
        format!("{verb} with {count} {noun}")
    }
}

fn row_subtitle(row: &ConsentRow) -> String {
    format!("{}  <tt>{}</tt>", format::escape(&row.scope), format::escape(&row.id))
}

fn stays_off(consent: &ConsentRequest) -> Option<String> {
    consent.rows.iter().any(|row| row.need == ConsentNeed::Sensitive).then(|| {
        format!(
            "The plugin's tools stay off until every permission it asks for is on. To grant one later, run local-studio plugin grant {} <capability>.",
            consent.plugin
        )
    })
}

fn header(consent: &ConsentRequest) -> gtk::Box {
    let tile = consent.icon.as_ref().map_or_else(|| gtk::Image::from_icon_name(icons::PLUGIN), gtk::Image::from_file);
    tile.set_pixel_size(24);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.set_size_request(40, 40);
    frame.set_valign(gtk::Align::Start);
    frame.add_css_class("ls-consent-tile");
    tile.set_vexpand(true);
    frame.append(&tile);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text.set_hexpand(true);
    text.append(&label(&title(consent), "ls-consent-title"));
    let sub = label("", "ls-consent-secondary");
    sub.set_markup(&subtitle(consent));
    text.append(&sub);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&frame);
    row.append(&text);
    row
}

#[allow(clippy::too_many_lines)]
fn present(app: &Rc<App>, consent: &ConsentRequest, findings_open: bool) {
    let dialog = adw::Dialog::builder().title(title(consent)).content_width(540).build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let top = gtk::Box::new(gtk::Orientation::Vertical, 14);
    top.set_margin_top(18);
    top.set_margin_bottom(18);
    top.set_margin_start(18);
    top.set_margin_end(18);
    top.append(&header(consent));
    let scan = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let clean = consent.findings.is_empty();
    let mark = gtk::Image::from_icon_name(if clean { icons::DONE } else { icons::WARNING });
    mark.add_css_class(if clean { "ls-consent-clean" } else { "ls-consent-finding" });
    scan.append(&mark);
    scan.append(&label(&scan_line(consent), "ls-consent-scan"));
    top.append(&scan);
    let findings = gtk::Box::new(gtk::Orientation::Vertical, 2);
    findings.set_margin_start(22);
    for finding in &consent.findings {
        let line = label(finding, "ls-consent-secondary");
        line.add_css_class("monospace");
        line.set_selectable(true);
        findings.append(&line);
    }
    let revealer = gtk::Revealer::builder().child(&findings).reveal_child(findings_open).build();
    revealer.set_transition_type(gtk::RevealerTransitionType::None);
    let toggle = gtk::Button::with_label(if findings_open { "Hide findings" } else { "Show findings" });
    toggle.add_css_class("flat");
    toggle.add_css_class("ls-consent-link");
    if !clean {
        scan.append(&toggle);
        top.append(&revealer);
    }
    column.append(&top);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let asks = gtk::Box::new(gtk::Orientation::Vertical, 6);
    asks.set_margin_top(14);
    asks.set_margin_bottom(14);
    asks.set_margin_start(18);
    asks.set_margin_end(18);
    asks.append(&label("It asks to", "ls-consent-heading"));
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let switches: Rc<RefCell<Vec<(String, adw::SwitchRow)>>> = Rc::default();
    let needed = consent.rows.iter().filter(|row| row.need == ConsentNeed::Needed).count();
    for row in &consent.rows {
        let subtitle = row_subtitle(row);
        if row.need == ConsentNeed::Sensitive {
            let switch = adw::SwitchRow::builder().title(format::escape(&row.sentence)).subtitle(subtitle).build();
            let tag = gtk::Label::new(Some("Sensitive"));
            tag.add_css_class("ls-consent-tag");
            tag.set_valign(gtk::Align::Center);
            switch.add_suffix(&tag);
            list.append(&switch);
            switches.borrow_mut().push((row.id.clone(), switch));
        } else {
            let action = adw::ActionRow::builder().title(format::escape(&row.sentence)).subtitle(subtitle).build();
            let tag = gtk::Label::new(Some("Needed"));
            tag.add_css_class("ls-consent-secondary");
            action.add_suffix(&tag);
            list.append(&action);
        }
    }
    asks.append(&list);
    column.append(&asks);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    footer.add_css_class("ls-consent-footer");
    if let Some(text) = stays_off(consent) {
        footer.append(&label(&text, "ls-consent-secondary"));
    }
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let install = gtk::Button::with_label(&confirm(consent, needed));
    install.add_css_class("suggested-action");
    install.set_sensitive(clean || findings_open);
    buttons.append(&cancel);
    buttons.append(&install);
    footer.append(&buttons);
    column.append(&footer);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(720)
        .child(&column)
        .build();
    dialog.set_child(Some(&scroller));

    for (_, switch) in switches.borrow().iter() {
        let (switches, install, consent) = (Rc::clone(&switches), install.clone(), consent.clone());
        switch.connect_active_notify(move |_| {
            let on = switches.borrow().iter().filter(|(_, switch)| switch.is_active()).count();
            install.set_label(&confirm(&consent, needed + on));
        });
    }
    let opened = Rc::new(Cell::new(findings_open));
    let (install_ref, opened_ref) = (install.clone(), Rc::clone(&opened));
    toggle.connect_clicked(move |toggle| {
        let open = !revealer.reveals_child();
        revealer.set_reveal_child(open);
        toggle.set_label(if open { "Hide findings" } else { "Show findings" });
        opened_ref.set(opened_ref.get() || open);
        install_ref.set_sensitive(opened_ref.get());
    });
    let answered = Rc::new(Cell::new(false));
    let request = consent.request.clone();
    let send = {
        let (weak, request, answered) = (Rc::downgrade(app), request.clone(), Rc::clone(&answered));
        Rc::new(move |answer: ConsentAnswer| {
            if answered.replace(true) {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            let request = request.clone();
            app.core.run(move |studio| studio.answer_consent(request, answer));
        })
    };
    let (closing, cancel_send) = (dialog.clone(), Rc::clone(&send));
    cancel.connect_clicked(move |_| {
        cancel_send(ConsentAnswer::Cancel);
        closing.close();
    });
    let (closing, install_send, chosen) = (dialog.clone(), Rc::clone(&send), Rc::clone(&switches));
    install.connect_clicked(move |_| {
        let sensitive =
            chosen.borrow().iter().filter(|(_, switch)| switch.is_active()).map(|(id, _)| id.clone()).collect();
        install_send(ConsentAnswer::Install { sensitive });
        closing.close();
    });
    let shown = request.clone();
    dialog.connect_closed(move |_| {
        send(ConsentAnswer::Cancel);
        SHOWN.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(request, _)| *request == shown) {
                *slot = None;
            }
        });
    });
    SHOWN.with(|slot| slot.replace(Some((request, dialog.clone()))));
    dialog.present(Some(&app.window));
}

fn row(id: &str, need: ConsentNeed, sentence: &str, scope: &str, gates: Option<&str>) -> ConsentRow {
    ConsentRow {
        id: id.to_string(),
        need,
        sentence: sentence.to_string(),
        scope: scope.to_string(),
        gates: gates.map(str::to_string),
    }
}

fn fixture(rows: Vec<ConsentRow>, findings: Vec<String>, update_from: Option<&str>) -> ConsentRequest {
    ConsentRequest {
        request: "snapshot".to_string(),
        plugin: "field-notes".to_string(),
        name: "Field Notes".to_string(),
        icon: None,
        description: "Reads pages in its own tabs and keeps notes for a thread".to_string(),
        version: if update_from.is_some() { "1.5.0" } else { "1.4.0" }.to_string(),
        install: true,
        update_from: update_from.map(str::to_string),
        source: "https://github.com/example/field-notes".to_string(),
        pin: Some("3f9a2c1".to_string()),
        files: 14,
        findings,
        rows,
    }
}

fn fixture_rows() -> Vec<ConsentRow> {
    vec![
        row(
            "mcp-server",
            ConsentNeed::Needed,
            "Give agents tools",
            "Starts field-notes-mcp --stdio when a thread uses it.",
            None,
        ),
        row(
            "ask-user",
            ConsentNeed::Needed,
            "Ask you questions in a thread",
            "Shows a question card; it never answers for you.",
            None,
        ),
        row(
            "screen-capture",
            ConsentNeed::Sensitive,
            "See the windows or tabs it opened",
            "Screenshots of its own tabs or windows, never your other windows.",
            Some("page screenshots for the agent"),
        ),
        row(
            "site-secrets",
            ConsentNeed::Sensitive,
            "Fill sign-ins you approve",
            "Reads one saved login per site, after you allow that site.",
            None,
        ),
    ]
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if !SCENES.contains(&scene) {
        return false;
    }
    crate::fixtures::conversation(app, false);
    if scene.ends_with("-light") {
        let settings = Settings { color_scheme: ColorScheme::Light, ..Settings::default() };
        app.apply(vec![Update::Settings { settings }]);
    }
    match scene.trim_end_matches("-light") {
        "consent-findings" => present(
            app,
            &fixture(
                fixture_rows().into_iter().take(3).collect(),
                vec!["executable not named in the manifest: bin/helper".to_string()],
                None,
            ),
            true,
        ),
        "consent-update" => present(
            app,
            &fixture(
                vec![row(
                    "synthetic-input",
                    ConsentNeed::Sensitive,
                    "Click and type in those tabs",
                    "Input goes only to what the plugin opened.",
                    Some("clicking and typing in its own tabs"),
                )],
                Vec::new(),
                Some("1.4.0"),
            ),
            false,
        ),
        _ => present(app, &fixture(fixture_rows(), Vec::new(), None), false),
    }
    true
}
