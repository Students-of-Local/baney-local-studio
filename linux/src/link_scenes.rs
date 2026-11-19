use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{Block, ItemKind, LinkTarget, Settings, TranscriptOp, Update};
use webkit::prelude::*;

use crate::app::App;
use crate::fixtures::{conversation, item, FOCUSED};

const SCENES: [&str; 2] = ["link-app", "link-system"];
const LABEL: &str = "Open the studio";

thread_local! {
    static TITLE: RefCell<String> = const { RefCell::new(String::new()) };
}

fn url() -> Option<String> {
    std::env::var("LOCAL_STUDIO_SNAPSHOT_LINK").ok().filter(|url| !url.is_empty())
}

fn target(scene: &str) -> Option<LinkTarget> {
    match scene {
        "link-app" => Some(LinkTarget::App),
        "link-system" => Some(LinkTarget::System),
        _ => None,
    }
}

pub fn scenes() -> Vec<&'static str> {
    if url().is_some() {
        SCENES.to_vec()
    } else {
        Vec::new()
    }
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    let (Some(target), Some(url)) = (target(scene), url()) else { return false };
    conversation(app, false);
    let settings = Settings { open_links_in: target, ..app.state.borrow().settings.clone() };
    let reply = item(
        91,
        3,
        1,
        ItemKind::Assistant {
            blocks: vec![Block::Text { text: format!("The studio is running. [{LABEL}]({url}) to see the model.") }],
            streaming: false,
            error: None,
            changed_files: Vec::new(),
        },
    );
    app.apply(vec![
        Update::Settings { settings },
        Update::Transcript { thread: FOCUSED.to_string(), op: TranscriptOp::Insert { index: u64::MAX, item: reply } },
    ]);
    true
}

pub fn click(app: &Rc<App>, scene: &str) -> Result<(), String> {
    if target(scene).is_none() {
        return Ok(());
    }
    let view = find_view(app.window.upcast_ref()).ok_or("no transcript view shows the link")?;
    let buffer = view.buffer();
    let tag = buffer.tag_table().lookup("link").ok_or("the transcript view has no link tag")?;
    let mut iter = buffer.start_iter();
    if !iter.starts_tag(Some(&tag)) && !iter.forward_to_tag_toggle(Some(&tag)) {
        return Err("the transcript view has no link".to_string());
    }
    let rect = view.iter_location(&iter);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x() + 2, rect.y() + rect.height() / 2);
    let gesture = view
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<gtk::GestureClick>().ok())
        .ok_or("the transcript view has no click gesture")?;
    let title = app.panel.browser.view.title().map(|title| title.to_string()).unwrap_or_default();
    TITLE.with(|before| before.replace(title));
    gesture.emit_by_name::<()>("released", &[&1i32, &f64::from(x), &f64::from(y)]);
    println!("{scene}: clicked {LABEL} at {x},{y}");
    Ok(())
}

pub fn ready(app: &Rc<App>, scene: &str) -> bool {
    if target(scene) != Some(LinkTarget::App) {
        return true;
    }
    let view = &app.panel.browser.view;
    let fresh =
        view.title().is_some_and(|title| !title.is_empty() && TITLE.with(|before| *before.borrow() != title.as_str()));
    !view.is_loading() && fresh
}

pub fn check(app: &Rc<App>, scene: &str) -> Result<(), String> {
    let (Some(target), Some(url)) = (target(scene), url()) else { return Ok(()) };
    let showing = app.panel.showing("browser");
    match target {
        LinkTarget::App => {
            let view = &app.panel.browser.view;
            let uri = view.uri().map(|uri| uri.to_string()).unwrap_or_default();
            let title = view.title().map(|title| title.to_string()).unwrap_or_default();
            println!("{scene}: browser panel showing={showing} uri={uri} title={title}");
            if !showing {
                return Err("the Browser panel did not open".to_string());
            }
            if uri != url {
                return Err(format!("the Browser panel loaded {uri}, not {url}"));
            }
        }
        LinkTarget::System => {
            let visible = app.panel.root.is_visible();
            println!("{scene}: panel visible={visible}");
            if visible {
                return Err("the panel opened with links set to the system browser".to_string());
            }
        }
    }
    Ok(())
}

fn find_view(root: &gtk::Widget) -> Option<gtk::TextView> {
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
            let buffer = view.buffer();
            if view.has_css_class("ls-markdown")
                && buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).contains(LABEL)
            {
                return Some(view.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            stack.push(current);
        }
    }
    None
}
