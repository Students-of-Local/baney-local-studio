use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gio, glib};
use local_studio_core::{link_words, outside_link, studio_link, LinkWords, StudioLink};

use crate::app::App;

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
                app.show_notice("That thread is not in Local Studio on this computer.");
            }
        }
        link => app.core.run(|studio| studio.open_studio_link(link)),
    }
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
