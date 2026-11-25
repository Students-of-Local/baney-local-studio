use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use local_studio_core::{ColorScheme, Settings, SkillDraft, SkillFile, Update};

use crate::app::{App, Dirty, Target};
use crate::icons;
use crate::state::State;

pub const SCENES: [&str; 4] = ["skill-draft", "skill-draft-light", "skill-draft-private", "skill-draft-review"];

const ASKS: &str = "asks.txt";
const PLACES: &str = "where.txt";
const SIGN_IN: &str = "Sign in with password: the one on the fridge";

struct Open {
    thread: String,
    dialog: adw::Dialog,
    save: gtk::Button,
    error: gtk::Label,
}

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
}

pub struct SkillCard {
    pub root: gtk::Box,
    name: gtk::Label,
    replaces: gtk::Label,
    about: gtk::Label,
    facts: gtk::Label,
    warning: gtk::Box,
    warned: gtk::Label,
    save: gtk::Button,
    review: gtk::Button,
    discard: gtk::Button,
}

fn label(class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.add_css_class(class);
    label
}

fn warning() -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&icons::styled(icons::WARNING, "ls-consent-finding"));
    let text = label("ls-consent-scan");
    row.append(&text);
    (row, text)
}

impl SkillCard {
    pub fn new(app: &Weak<App>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("ls-approval");
        root.add_css_class("ls-question");
        root.set_visible(false);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let kind = label("kind");
        kind.set_label("Skill from this thread");
        kind.set_hexpand(true);
        let replaces = label("ls-meta");
        replaces.set_label("Replaces yours");
        top.append(&icons::styled(icons::STAR_ADD, "ls-question-icon"));
        top.append(&kind);
        top.append(&replaces);
        let (name, about, facts) = (label("heading"), label("ls-consent-scan"), label("ls-meta"));
        let (warning, warned) = warning();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let save = gtk::Button::with_label("Save");
        let review = gtk::Button::with_label("Review");
        let discard = gtk::Button::with_label("Discard");
        discard.add_css_class("flat");
        for button in [&save, &review, &discard] {
            buttons.append(button);
        }
        root.append(&top);
        root.append(&name);
        root.append(&about);
        root.append(&facts);
        root.append(&warning);
        root.append(&buttons);
        let weak = app.clone();
        save.connect_clicked(move |_| {
            if let Some((app, thread, draft)) = shown(&weak) {
                keep(&app, thread, draft.id, draft.text);
            }
        });
        let weak = app.clone();
        review.connect_clicked(move |_| {
            if let Some((app, thread, draft)) = shown(&weak) {
                present(&app, thread, &draft);
            }
        });
        let weak = app.clone();
        discard.connect_clicked(move |_| {
            if let Some((app, thread, draft)) = shown(&weak) {
                app.core.run(move |studio| studio.discard_skill(thread, draft.id));
            }
        });
        Self { root, name, replaces, about, facts, warning, warned, save, review, discard }
    }

    pub fn render(&self, state: &State) {
        let thread = state.selected.as_ref();
        let draft = thread.and_then(|thread| state.skill_drafts.get(thread));
        self.root.set_visible(draft.is_some());
        let Some(draft) = draft else { return };
        let saving = thread.is_some_and(|thread| state.skill_saving.values().any(|saving| saving == thread));
        self.name.set_label(&draft.name);
        self.about.set_label(&draft.description);
        self.facts.set_label(&facts(draft));
        self.facts.set_visible(!self.facts.label().is_empty());
        self.replaces.set_visible(draft.replaces);
        self.warning.set_visible(!draft.marked.is_empty());
        self.warned.set_label(&private(draft.marked.len()));
        self.save.set_label(verb(draft));
        let (first, second) =
            if draft.marked.is_empty() { (&self.save, &self.review) } else { (&self.review, &self.save) };
        first.add_css_class("suggested-action");
        second.remove_css_class("suggested-action");
        for button in [&self.save, &self.review, &self.discard] {
            button.set_sensitive(!saving);
        }
    }
}

fn shown(app: &Weak<App>) -> Option<(Rc<App>, String, SkillDraft)> {
    let app = app.upgrade()?;
    let thread = app.state.borrow().selected.clone()?;
    let draft = app.state.borrow().skill_drafts.get(&thread).cloned()?;
    Some((app, thread, draft))
}

fn verb(draft: &SkillDraft) -> &'static str {
    if draft.replaces {
        "Replace"
    } else {
        "Save"
    }
}

fn private(lines: usize) -> String {
    let (lines, them) = if lines == 1 { ("1 line".to_string(), "it") } else { (format!("{lines} lines"), "them") };
    format!("{lines} may hold a password or a key. Read {them} before you save.")
}

fn lines<'a>(draft: &'a SkillDraft, path: &str) -> Vec<&'a str> {
    let file = draft.files.iter().find(|file| file.path == path);
    file.map(|file| file.text.lines().collect()).unwrap_or_default()
}

fn facts(draft: &SkillDraft) -> String {
    let places = lines(draft, PLACES).join(", ");
    let asks = lines(draft, ASKS).len();
    let files = draft.files.iter().filter(|file| file.path != ASKS && file.path != PLACES).count();
    let count = |number: usize, one: &str, many: &str| match number {
        0 => String::new(),
        1 => format!("1 {one}"),
        _ => format!("{number} {many}"),
    };
    let parts = [places, count(asks, "way to ask", "ways to ask"), count(files, "file", "files")];
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn keep(app: &Rc<App>, thread: String, draft: String, text: String) {
    let request = app.ask(Target::Skill);
    app.state.borrow_mut().skill_saving.insert(request.clone(), thread.clone());
    busy(&thread, true);
    app.render(Dirty { composer: true, ..Dirty::default() });
    app.core.run(move |studio| studio.save_skill(request, thread, draft, text));
}

fn busy(thread: &str, saving: bool) {
    OPEN.with(|slot| {
        if let Some(open) = slot.borrow().as_ref().filter(|open| open.thread == thread) {
            open.save.set_sensitive(!saving);
            open.error.set_visible(open.error.is_visible() && !saving);
        }
    });
}

fn close(thread: &str) {
    let open = OPEN.with(|slot| slot.borrow_mut().take_if(|open| open.thread == thread));
    if let Some(open) = open {
        open.dialog.force_close();
    }
}

pub fn set(app: &App, thread: String, draft: Option<SkillDraft>) {
    if draft.is_none() {
        close(&thread);
    }
    let mut state = app.state.borrow_mut();
    match draft {
        Some(draft) => state.skill_drafts.insert(thread, draft),
        None => state.skill_drafts.remove(&thread),
    };
}

pub fn done(app: &App, request: &str, message: &str) {
    let thread = app.state.borrow_mut().skill_saving.remove(request);
    if let Some(thread) = &thread {
        close(thread);
    }
    app.toast(message);
}

pub fn failed(app: &App, request: &str, message: &str) {
    let thread = app.state.borrow_mut().skill_saving.remove(request).unwrap_or_default();
    busy(&thread, false);
    let told = OPEN.with(|slot| {
        let slot = slot.borrow();
        let Some(open) = slot.as_ref().filter(|open| open.thread == thread) else { return false };
        open.error.set_label(message);
        open.error.set_visible(true);
        true
    });
    if !told {
        app.show_notice(message);
    }
}

fn text_view(text: &str, editable: bool) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(editable)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .build();
    view.buffer().set_text(text);
    view
}

fn present(app: &Rc<App>, thread: String, draft: &SkillDraft) {
    let dialog = adw::Dialog::builder().title("Review Skill").content_width(680).content_height(640).build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for set in [gtk::Box::set_margin_top, gtk::Box::set_margin_bottom] {
        set(&column, 12);
    }
    for set in [gtk::Box::set_margin_start, gtk::Box::set_margin_end] {
        set(&column, 16);
    }
    let error = label("error");
    error.set_visible(false);
    column.append(&error);
    if !draft.marked.is_empty() {
        let (row, text) = warning();
        text.set_label(&private(draft.marked.len()));
        column.append(&row);
        for line in &draft.marked {
            let marked = label("command");
            marked.add_css_class("ls-mono");
            marked.set_label(line);
            marked.set_selectable(true);
            column.append(&marked);
        }
    }
    if !draft.left.is_empty() {
        let left = label("ls-meta");
        left.set_label(&format!("Left out of the skill: code blocks labeled {}.", draft.left.join(", ")));
        column.append(&left);
    }
    let view = text_view(&draft.text, true);
    let frame = gtk::ScrolledWindow::builder().min_content_height(320).vexpand(true).child(&view).build();
    frame.add_css_class("ls-commit-box");
    column.append(&frame);
    for file in &draft.files {
        let expander = gtk::Expander::new(Some(&file.path));
        expander.set_child(Some(&text_view(&file.text, false)));
        column.append(&expander);
    }
    let save = gtk::Button::with_label(verb(draft));
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).build();
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));
    let (weak, target) = (Rc::downgrade(app), thread.clone());
    let id = draft.id.clone();
    save.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            let buffer = view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            keep(&app, target.clone(), id.clone(), text.to_string());
        }
    });
    let closed = thread.clone();
    dialog.connect_closed(move |_| {
        OPEN.with(|slot| drop(slot.borrow_mut().take_if(|open| open.thread == closed)));
    });
    OPEN.with(|slot| slot.replace(Some(Open { thread, dialog: dialog.clone(), save, error })));
    dialog.present(Some(&app.window));
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
    let file = |path: &str, text: &str| SkillFile { path: path.to_string(), text: text.to_string() };
    let private = !matches!(scene, "skill-draft" | "skill-draft-light");
    let draft = SkillDraft {
        id: scene.to_string(),
        name: "reserve-a-court".to_string(),
        description: "Reserve a tennis court at the parks site, for a date, an hour and a court you name".to_string(),
        folder: "reserve-a-court".to_string(),
        text: format!("---\nname: reserve-a-court\ndescription: Reserve a tennis court at the parks site, for a date, an hour and a court you name\n---\n\n{}## Find a free hour\n\n1. GET https://parks.example/api/courts?date=<yyyy-mm-dd>. Each row has `court`, `hour` and `free`.\n2. If the hour is not free, say which hours are and ask.\n\n## Reserve it\n\n1. Open https://parks.example/reserve and fill \"Court\", \"Date\" and \"Hour\".\n2. Press \"Reserve\". The page answers \"Reserved\" and a number. If it says \"Too soon\", wait 30 seconds and press it again.\n3. Ask before paying a fee.\n", if private { format!("{SIGN_IN}.\n\n") } else { String::new() }),
        files: vec![
            file(ASKS, "book a court\nreserve tennis for saturday\nis a court free\n"),
            file(PLACES, "parks.example\n"),
            file("scripts/free.py", "import json, sys\nprint([row for row in json.load(sys.stdin) if row[\"free\"]])\n"),
        ],
        marked: if private { vec![SIGN_IN.to_string()] } else { Vec::new() },
        left: if private { vec!["json".to_string()] } else { Vec::new() },
        replaces: private,
    };
    let thread = crate::fixtures::FOCUSED.to_string();
    app.apply(vec![Update::SkillDraft { thread: thread.clone(), draft: Some(draft.clone()) }]);
    if scene == "skill-draft-review" {
        present(app, thread, &draft);
    }
    true
}
