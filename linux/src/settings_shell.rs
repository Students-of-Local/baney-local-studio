use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

#[derive(Clone)]
pub struct Shell {
    pub dialog: adw::Dialog,
    stack: adw::ViewStack,
    search: gtk::SearchEntry,
    pages: Rc<RefCell<Vec<adw::PreferencesPage>>>,
}

fn children(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(next) = child {
        child = next.next_sibling();
        found.push(next);
    }
    found
}

fn collect<T: IsA<gtk::Widget>>(widget: &gtk::Widget, found: &mut Vec<T>) {
    for child in children(widget) {
        match child.downcast::<T>() {
            Ok(hit) => found.push(hit),
            Err(child) => collect(&child, found),
        }
    }
}

fn text(widget: &gtk::Widget) -> String {
    let mut labels: Vec<gtk::Label> = Vec::new();
    collect(widget, &mut labels);
    let mut text = widget.downcast_ref::<adw::PreferencesRow>().map(|row| row.title().to_string()).unwrap_or_default();
    for label in labels {
        text.push(' ');
        text.push_str(&label.text());
    }
    text.to_lowercase()
}

impl Shell {
    pub fn new(width: i32, height: i32) -> Self {
        let dialog = adw::Dialog::builder().title("Settings").content_width(width).content_height(height).build();
        dialog.add_css_class("preferences");
        dialog.add_css_class("ls-settings");
        let stack = adw::ViewStack::builder().vhomogeneous(false).build();
        let search = gtk::SearchEntry::builder().placeholder_text("Search settings").hexpand(true).build();
        let switcher = adw::ViewSwitcherSidebar::builder().stack(&stack).vexpand(true).build();
        let side_view = adw::ToolbarView::new();
        side_view.add_top_bar(&adw::HeaderBar::builder().title_widget(&search).show_end_title_buttons(false).build());
        side_view.set_content(Some(&switcher));
        let title = gtk::Label::new(None);
        let trail = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        trail.add_css_class("ls-settings-trail");
        let root = gtk::Label::new(Some("Settings"));
        root.add_css_class("dim-label");
        let slash = gtk::Label::new(Some("/"));
        slash.add_css_class("dim-label");
        trail.append(&root);
        trail.append(&slash);
        trail.append(&title);
        let content_bar = adw::HeaderBar::builder().show_title(false).build();
        content_bar.pack_start(&trail);
        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&content_bar);
        content_view.set_content(Some(&stack));
        let split = adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::builder().title("Settings").child(&side_view).build())
            .content(&adw::NavigationPage::builder().title("Settings").tag("content").child(&content_view).build())
            .min_sidebar_width(200.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.25)
            .show_content(true)
            .build();
        dialog.set_child(Some(&split));
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        dialog.add_breakpoint(breakpoint);
        let shown = split.clone();
        stack.connect_visible_child_notify(move |stack| {
            let name = stack.visible_child().map(|child| stack.page(&child).title().unwrap_or_default().to_string());
            title.set_label(name.as_deref().unwrap_or_default());
            shown.set_show_content(true);
        });
        let shell = Self { dialog, stack, search: search.clone(), pages: Rc::new(RefCell::new(Vec::new())) };
        let hidden: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
        let filter = shell.clone();
        search.connect_search_changed(move |entry| filter.filter(&entry.text().trim().to_lowercase(), &hidden));
        let focus = search.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, modifiers| {
            if keyval == gtk::gdk::Key::f && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                focus.grab_focus();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        shell.dialog.add_controller(key);
        shell
    }

    pub fn add(&self, page: &adw::PreferencesPage) {
        let name = page.name().unwrap_or_default();
        let title = page.title();
        let icon = page.icon_name().unwrap_or_default();
        self.stack.add_titled_with_icon(page, Some(name.as_str()), &title, &icon);
        self.pages.borrow_mut().push(page.clone());
    }

    pub fn search(&self, query: &str) {
        self.search.set_text(query);
    }

    pub fn set_visible_page_name(&self, name: &str) {
        self.stack.set_visible_child_name(name);
    }

    pub fn close(&self) -> bool {
        self.dialog.close()
    }

    pub fn present(&self, parent: Option<&impl IsA<gtk::Widget>>) {
        self.dialog.present(parent);
    }

    pub fn connect_closed(&self, closed: impl Fn(&adw::Dialog) + 'static) {
        self.dialog.connect_closed(closed);
    }

    fn filter(&self, query: &str, hidden: &RefCell<Vec<gtk::Widget>>) {
        for widget in hidden.borrow_mut().drain(..) {
            widget.set_visible(true);
        }
        let pages = self.pages.borrow();
        for page in pages.iter() {
            self.stack.page(page).set_visible(true);
        }
        if query.is_empty() {
            return;
        }
        let mut first = None;
        for page in pages.iter() {
            let mut groups: Vec<adw::PreferencesGroup> = Vec::new();
            collect(page.upcast_ref(), &mut groups);
            let whole = page.title().to_lowercase().contains(query);
            let mut page_hit = whole;
            for group in groups.iter().filter(|group| group.is_visible()) {
                if whole
                    || format!("{} {}", group.title(), group.description().unwrap_or_default())
                        .to_lowercase()
                        .contains(query)
                {
                    page_hit = true;
                    continue;
                }
                let mut rows: Vec<adw::PreferencesRow> = Vec::new();
                collect(group.upcast_ref(), &mut rows);
                let mut group_hit = false;
                for row in rows.iter().filter(|row| row.is_visible()) {
                    if text(row.upcast_ref()).contains(query) {
                        group_hit = true;
                    } else {
                        row.set_visible(false);
                        hidden.borrow_mut().push(row.clone().upcast());
                    }
                }
                if !group_hit {
                    group.set_visible(false);
                    hidden.borrow_mut().push(group.clone().upcast());
                }
                page_hit |= group_hit;
            }
            if page_hit {
                first.get_or_insert_with(|| page.clone());
            } else {
                self.stack.page(page).set_visible(false);
            }
        }
        if let Some(page) = first {
            if self.stack.visible_child().is_none_or(|child| !self.stack.page(&child).is_visible()) {
                self.stack.set_visible_child(&page);
            }
        }
    }
}
