use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use local_studio_core::{link_words, outside_link, studio_link, system_link, Connection, LinkWords, StudioLink};

use crate::app::App;

const LOOK: Duration = Duration::from_millis(250);
const LOOKS: u32 = 20;
const LIMIT: u32 = 240;
const MISSING: &str = "That thread is not in Local Studio on this computer.";

const PAGES: [&str; 10] = [
    "general",
    "appearance",
    "keybindings",
    "providers",
    "plugins",
    "source-control",
    "storage",
    "local",
    "connections",
    "archive",
];

pub fn ours(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("localstudio"))
}

pub fn words(url: &str) -> Option<LinkWords> {
    studio_link(url).map(link_words)
}

pub fn install(app: &Weak<App>, transcript: &impl IsA<gtk::Widget>) {
    let action = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    let weak = app.clone();
    action.connect_activate(move |_, url| {
        let link = url.and_then(glib::Variant::get::<String>).and_then(|url| studio_link(&url).map(|link| (url, link)));
        if let (Some(app), Some((url, link))) = (weak.upgrade(), link) {
            open(&app, &url, link);
        }
    });
    let group = gio::SimpleActionGroup::new();
    group.add_action(&action);
    transcript.insert_action_group("links", Some(&group));
}

fn open(app: &Rc<App>, url: &str, link: StudioLink) {
    match link {
        StudioLink::Settings { section } if outside_link(url).is_some() => settings(app, &section),
        StudioLink::Thread { id } => {
            if app.state.borrow().row(&id).is_some() {
                app.select_thread(Some(id));
            } else {
                app.show_notice(MISSING);
            }
        }
        link => app.core.run(|studio| studio.open_studio_link(link)),
    }
}

pub fn from_system(app: &Rc<App>, url: &str) {
    app.window.present();
    match system_link(url) {
        Some(StudioLink::Settings { section }) => settings(app, &section),
        Some(StudioLink::Thread { id }) => find_thread(app, id),
        _ => app.show_notice("Only settings and thread links open Local Studio from outside it."),
    }
}

fn find_thread(app: &Rc<App>, id: String) {
    if app.state.borrow().row(&id).is_some() {
        app.select_thread(Some(id));
        return;
    }
    let weak = Rc::downgrade(app);
    let looked = Cell::new((0, 0));
    glib::timeout_add_local(LOOK, move || {
        let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
        if app.state.borrow().row(&id).is_some() {
            app.select_thread(Some(id.clone()));
            return glib::ControlFlow::Break;
        }
        let (mut ready, total) = looked.get();
        ready += u32::from(matches!(app.state.borrow().connection, Some(Connection::Ready)));
        looked.set((ready, total + 1));
        if ready < LOOKS && total < LIMIT {
            return glib::ControlFlow::Continue;
        }
        app.show_notice(MISSING);
        glib::ControlFlow::Break
    });
}

fn settings(app: &Rc<App>, section: &str) {
    if section == "project" {
        let project = app.state.borrow().current_project();
        match project {
            Some(project) => crate::project::present(app, &project),
            None => app.show_notice("There is no project to show settings for."),
        }
    } else if PAGES.contains(&section) {
        app.open_settings(Some(section));
    } else {
        app.show_notice("Those settings are only in the Mac app.");
    }
}
