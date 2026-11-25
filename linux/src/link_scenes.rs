use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{Block, ItemKind, LinkTarget, Settings, TranscriptOp, Update};
use webkit::prelude::*;

use crate::app::App;
use crate::fixtures::{conversation, item, FOCUSED};
use crate::state::Page;

const SCENES: [&str; 3] = ["link-app", "link-system", "link-app-off-thread"];
const BUTTONS: &str = "link-buttons";
const PRESSED: &str = "link-buttons-pressed";
const LABEL: &str = "Open the studio";
const OFFER: &str = "Done. [Open the **studio** harmless](localstudio://app/the%20studio) or \
    [harmless](localstudio://settings/Providers), then [harmless](localstudio://reveal?path=~/Documents/report.pdf) \
    and [not a link](localstudio://app/../x).";

thread_local! {
    static TITLE: RefCell<String> = const { RefCell::new(String::new()) };
    static CLICKED: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn url() -> Option<String> {
    std::env::var("LOCAL_STUDIO_SNAPSHOT_LINK").ok().filter(|url| !url.is_empty())
}

/// Links set to open in the app, clicked while another page shows: the panel would open out of
/// sight, so the link goes to the system handler instead.
fn off_thread(scene: &str) -> bool {
    scene == "link-app-off-thread"
}

fn target(scene: &str) -> Option<LinkTarget> {
    match scene {
        "link-app" | "link-app-off-thread" => Some(LinkTarget::App),
        "link-system" => Some(LinkTarget::System),
        _ => None,
    }
}

pub fn scenes() -> Vec<&'static str> {
    let web: &[&str] = if url().is_some() { &SCENES } else { &[] };
    web.iter().copied().chain([BUTTONS, PRESSED]).collect()
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if [BUTTONS, PRESSED].contains(&scene) {
        conversation(app, false);
        reply(app, OFFER);
        return true;
    }
    let (Some(target), Some(url)) = (target(scene), url()) else { return false };
    conversation(app, false);
    let settings = Settings { open_links_in: target, ..app.state.borrow().settings.clone() };
    app.apply(vec![Update::Settings { settings }]);
    reply(app, &format!("The studio is running. [{LABEL}]({url}) to see the model."));
    if off_thread(scene) {
        app.open_page(Page::Usage);
    }
    true
}

fn reply(app: &Rc<App>, text: &str) {
    let item = item(
        91,
        3,
        1,
        ItemKind::Assistant {
            blocks: vec![Block::Text { text: text.to_string() }],
            streaming: false,
            error: None,
            changed_files: Vec::new(),
        },
    );
    let op = TranscriptOp::Insert { index: u64::MAX, item };
    app.apply(vec![Update::Transcript { thread: FOCUSED.to_string(), op }]);
}

fn buttons(app: &Rc<App>) -> Result<(), String> {
    let view = find_view(app.window.upcast_ref()).ok_or("no transcript view shows the buttons")?;
    let buffer = view.buffer();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).replace('\u{A0}', " ");
    println!("{BUTTONS}: {text}");
    for shown in [LABEL, "Open Providers settings", "Show report.pdf in Files", "not a link"] {
        if text.matches(shown).count() != 1 {
            return Err(format!("{shown} is not there once"));
        }
    }
    if text.contains("harmless") {
        return Err("a button shows the model's own words".to_string());
    }
    Ok(())
}

fn press(app: &Rc<App>) -> Result<(), String> {
    let view = find_view(app.window.upcast_ref()).ok_or("no transcript view shows the buttons")?;
    let opened = |url: &str| view.activate_action("links.open", Some(&url.to_variant())).is_ok();
    if !opened("localstudio://thread/nowhere") || app.settings.borrow().is_some() {
        return Err("a thread link opened settings".to_string());
    }
    if app.window.lookup_action("open").is_some() {
        return Err("the window has the action that runs a link".to_string());
    }
    click_text(app, &view, "Open Providers settings")
}

fn click_text(app: &Rc<App>, view: &gtk::TextView, shown: &str) -> Result<(), String> {
    let (from, _) = view
        .buffer()
        .start_iter()
        .forward_search(shown, gtk::TextSearchFlags::TEXT_ONLY, None)
        .ok_or_else(|| format!("the transcript does not show {shown}"))?;
    release(view, &from)?;
    if app.settings.borrow().is_none() {
        return Err(format!("{shown} did not open settings"));
    }
    Ok(())
}

fn release(view: &gtk::TextView, at: &gtk::TextIter) -> Result<(i32, i32), String> {
    let rect = view.iter_location(at);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x() + 2, rect.y() + rect.height() / 2);
    let gesture = view
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<gtk::GestureClick>().ok())
        .ok_or("the transcript view has no click gesture")?;
    gesture.emit_by_name::<()>("released", &[&1i32, &f64::from(x), &f64::from(y)]);
    Ok((x, y))
}

pub fn click(app: &Rc<App>, scene: &str) -> Result<(), String> {
    if scene == PRESSED {
        return press(app);
    }
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
    let title = app.panel.browser.view.title().map(|title| title.to_string()).unwrap_or_default();
    TITLE.with(|before| before.replace(title));
    CLICKED.with(|clicked| clicked.set(Some(Instant::now())));
    let (x, y) = release(&view, &iter)?;
    println!("{scene}: clicked {LABEL} at {x},{y}");
    Ok(())
}

pub fn ready(app: &Rc<App>, scene: &str) -> bool {
    match target(scene) {
        None => return true,
        Some(_) if off_thread(scene) => {
            return CLICKED.with(Cell::get).is_none_or(|clicked| clicked.elapsed() >= Duration::from_secs(2));
        }
        Some(LinkTarget::System) => {
            return CLICKED.with(Cell::get).is_none_or(|clicked| clicked.elapsed() >= Duration::from_secs(2));
        }
        Some(LinkTarget::App) => {}
    }
    let view = &app.panel.browser.view;
    let fresh =
        view.title().is_some_and(|title| !title.is_empty() && TITLE.with(|before| *before.borrow() != title.as_str()));
    !view.is_loading() && fresh
}

pub fn finish(scene: &str) -> bool {
    scene == PRESSED || target(scene).is_some()
}

pub fn check(app: &Rc<App>, scene: &str) -> Result<(), String> {
    if scene == BUTTONS {
        return buttons(app);
    }
    if scene == PRESSED && app.settings.borrow().is_none() {
        return Err("settings is not open".to_string());
    }
    let (Some(target), Some(url)) = (target(scene), url()) else { return Ok(()) };
    let showing = app.panel.showing("browser");
    if off_thread(scene) {
        println!("{scene}: browser panel showing={showing}");
        if showing {
            return Err("the Browser panel opened while another page was showing".to_string());
        }
        return Ok(());
    }
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
