use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::colors::Colors;
use crate::format::{self, Markdown, Span, DEEPEST};

const CACHE: usize = 96;
const MARKER_STOP: i32 = 16;
const TEXT_STOP: i32 = 24;
const DEPTH_STEP: i32 = 20;
const QUOTE_MARGIN: i32 = 14;
const CODE_INSET: i32 = 12;
const CELL_GAP: i32 = 28;
const HIDDEN: char = '\u{200B}';
const PAD: char = '\u{00A0}';
const BREAK: &str = "\u{2028}";

enum Kind {
    Text { prefix: String, skip: i32 },
    Code,
    Header,
    Cell,
    Rule,
    Spacer,
}

struct Line {
    group: usize,
    kind: Kind,
}

struct Link {
    tag: gtk::TextTag,
    url: String,
    detail: Option<String>,
}

#[derive(Default)]
struct Doc {
    lines: Vec<Line>,
    links: Vec<Link>,
    buttons: bool,
    generation: u64,
}

struct Entry {
    text: String,
    style: (Colors, String),
    view: gtk::TextView,
    doc: Rc<RefCell<Doc>>,
}

#[derive(Default)]
pub struct Views {
    entries: HashMap<(u64, usize), Entry>,
    order: VecDeque<(u64, usize)>,
}

impl Views {
    pub fn view(&mut self, key: (u64, usize), text: &str) -> gtk::TextView {
        let style = (crate::colors::current(), crate::theme::code_font());
        if let Some(entry) = self.entries.get_mut(&key).filter(|entry| entry.style == style) {
            if let Some(column) = entry.view.parent().and_downcast::<gtk::Box>() {
                column.remove(&entry.view);
            }
            if entry.text != text {
                fill(&entry.view, &entry.doc, text);
                entry.text = text.to_string();
            }
            return entry.view.clone();
        }
        let (view, doc) = create();
        doc.borrow_mut().buttons = true;
        fill(&view, &doc, text);
        let entry = Entry { text: text.to_string(), style, view: view.clone(), doc };
        if self.entries.insert(key, entry).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CACHE {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        view
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

pub fn view(text: &str) -> gtk::TextView {
    let (view, doc) = create();
    fill(&view, &doc, text);
    view
}

fn create() -> (gtk::TextView, Rc<RefCell<Doc>>) {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .accepts_tab(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .hexpand(true)
        .build();
    view.add_css_class("ls-markdown");
    let buffer = view.buffer();
    styles(&buffer, &crate::colors::current());
    crate::highlight::ensure_tags(&buffer);
    let doc = Rc::new(RefCell::new(Doc::default()));
    let copied = Rc::clone(&doc);
    view.connect_copy_clipboard(move |view| {
        if let Some(text) = copy_text(&view.buffer(), &copied.borrow()) {
            view.clipboard().set_text(&text);
        }
        view.stop_signal_emission_by_name("copy-clipboard");
    });
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    let clicked = Rc::clone(&doc);
    click.connect_released(move |gesture, presses, x, y| {
        let Some(view) = gesture.widget().and_downcast::<gtk::TextView>() else { return };
        if presses != 1 || view.buffer().has_selection() {
            return;
        }
        let Some((url, detail)) = link_at(&view, &clicked.borrow(), x, y) else { return };
        if detail.is_some() {
            let _ = view.activate_action("links.open", Some(&url.to_variant()));
            return;
        }
        let handled = view.activate_action("win.open-link", Some(&url.to_variant())).is_ok();
        if !handled && local_studio_core::refused_link(&url).is_none() {
            let window = view.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&url).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
        }
    });
    view.add_controller(click);
    let motion = gtk::EventControllerMotion::new();
    let hovered = Rc::clone(&doc);
    motion.connect_motion(move |motion, x, y| {
        let Some(view) = motion.widget().and_downcast::<gtk::TextView>() else { return };
        let over = !hovered.borrow().links.is_empty() && link_at(&view, &hovered.borrow(), x, y).is_some();
        view.set_cursor_from_name(Some(if over { "pointer" } else { "text" }));
    });
    view.add_controller(motion);
    view.set_has_tooltip(true);
    let shown = Rc::clone(&doc);
    view.connect_query_tooltip(move |view, x, y, keyboard, tooltip| {
        let detail = link_at(view, &shown.borrow(), f64::from(x), f64::from(y)).and_then(|(_, detail)| detail);
        tooltip.set_text(detail.as_deref());
        detail.is_some() && !keyboard
    });
    view.connect_map(|view| {
        let view = view.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(view) = view.upgrade() {
                view.queue_resize();
            }
        });
    });
    (view, doc)
}

fn link_at(view: &gtk::TextView, doc: &Doc, x: f64, y: f64) -> Option<(String, Option<String>)> {
    #[allow(clippy::cast_possible_truncation)]
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let iter = view.iter_at_location(bx, by)?;
    doc.links.iter().find(|link| iter.has_tag(&link.tag)).map(|link| (link.url.clone(), link.detail.clone()))
}

fn rgba(color: &str, alpha: f32) -> Option<gdk::RGBA> {
    gdk::RGBA::parse(color).ok().map(|color| color.with_alpha(color.alpha() * alpha))
}

fn styles(buffer: &gtk::TextBuffer, colors: &Colors) {
    let table = buffer.tag_table();
    let add = |name: &str| {
        let tag = gtk::TextTag::new(Some(name));
        table.add(&tag);
        tag
    };
    let muted = rgba(colors.muted, 1.0);
    let code_bg = rgba(colors.code_bg, 1.0);
    add("bold").set_weight(700);
    add("italic").set_style(pango::Style::Italic);
    add("strike").set_strikethrough(true);
    add("mono").set_family(Some(&crate::theme::code_family()));
    add("muted").set_foreground_rgba(muted.as_ref());
    for (level, step) in (1..=DEEPEST).zip(0..) {
        let (quote, inside) = (add(&format!("quote-{level}")), add(&format!("inside-{level}")));
        quote.set_foreground_rgba(muted.as_ref());
        quote.set_left_margin((step + 1) * QUOTE_MARGIN);
        inside.set_left_margin(step * DEPTH_STEP + TEXT_STOP);
        quote.set_accumulative_margin(true);
        inside.set_accumulative_margin(true);
    }
    let link = add("link");
    link.set_foreground_rgba(rgba(colors.accent, 1.0).as_ref());
    link.set_underline(pango::Underline::Single);
    let button = add("button");
    button.set_foreground_rgba(rgba(colors.accent, 1.0).as_ref());
    button.set_background_rgba(rgba(colors.accent, 0.14).as_ref());
    button.set_weight(600);
    for (name, scale) in [("h1", 1.4), ("h2", 1.25), ("h3", 1.1), ("h4", 1.0)] {
        let heading = add(name);
        heading.set_weight(700);
        heading.set_scale(scale);
    }
    add("tight").set_pixels_above_lines(4);
    for (name, pixels) in [("gap", 7), ("gap-wide", 13)] {
        let gap = add(name);
        gap.set_font(Some("Sans 1px"));
        gap.set_pixels_above_lines(pixels);
    }
    let code = add("code");
    code.set_font(Some(&crate::theme::code_font()));
    code.set_paragraph_background_rgba(code_bg.as_ref());
    code.set_wrap_mode(gtk::WrapMode::Char);
    code.set_indent(CODE_INSET);
    add("code-top").set_pixels_above_lines(8);
    add("code-bottom").set_pixels_below_lines(8);
    let head = add("code-head");
    head.set_paragraph_background_rgba(code_bg.as_ref());
    head.set_foreground_rgba(muted.as_ref());
    head.set_scale(0.85);
    head.set_indent(CODE_INSET);
    head.set_pixels_above_lines(8);
    head.set_pixels_below_lines(2);
    let row = add("row");
    row.set_pixels_above_lines(3);
    row.set_pixels_below_lines(3);
    let header = add("row-head");
    header.set_weight(700);
    header.set_paragraph_background_rgba(code_bg.as_ref());
    let rule = add("rule");
    rule.set_font(Some("Sans 1px"));
    rule.set_pixels_below_lines(1);
    rule.set_paragraph_background_rgba(rgba(colors.muted, 0.45).as_ref());
}

struct Job {
    start: i32,
    end: i32,
    code: String,
    language: Option<String>,
}

struct Builder<'a> {
    view: &'a gtk::TextView,
    buffer: gtk::TextBuffer,
    starts: Vec<(i32, Vec<String>)>,
    lines: Vec<Line>,
    links: Vec<Link>,
    buttons: bool,
    jobs: Vec<Job>,
    group: usize,
    tables: usize,
    nested: Vec<String>,
}

impl Builder<'_> {
    fn begin(&mut self, kind: Kind, tags: &[&str]) {
        if !self.starts.is_empty() {
            self.buffer.insert(&mut self.buffer.end_iter(), "\n");
        }
        let tags = tags.iter().map(ToString::to_string).chain(self.nested.iter().cloned()).collect();
        self.starts.push((self.buffer.char_count(), tags));
        self.lines.push(Line { group: self.group, kind });
    }

    fn insert(&self, text: &str, tags: &[&str]) {
        self.buffer.insert_with_tags_by_name(&mut self.buffer.end_iter(), text, tags);
    }

    fn spans(&mut self, spans: &[Span], extra: &[&str]) {
        let mut last = None;
        for span in spans {
            let words = span.link.as_deref().filter(|_| self.buttons).and_then(crate::app_links::words);
            let again = words.is_some() && span.link == last;
            let link =
                span.link.as_ref().filter(|url| words.is_some() || !self.buttons || !crate::app_links::ours(url));
            last.clone_from(&span.link);
            if again {
                continue;
            }
            let mut tags = extra.to_vec();
            if span.style.bold {
                tags.push("bold");
            }
            if span.style.italic {
                tags.push("italic");
            }
            if span.style.code {
                tags.push("mono");
            }
            if span.style.strike {
                tags.push("strike");
            }
            if link.is_some() && words.is_none() {
                tags.push("link");
            }
            let start = self.buffer.char_count();
            match &words {
                Some(words) => self.insert(&format!("{PAD}{}{PAD}", words.title), &["button"]),
                None => self.insert(&span.text.replace(['\n', '\r', '\u{2029}'], BREAK), &tags),
            }
            if let Some(url) = link {
                let tag = gtk::TextTag::new(None);
                self.buffer.tag_table().add(&tag);
                self.buffer.apply_tag(&tag, &self.buffer.iter_at_offset(start), &self.buffer.end_iter());
                let detail = words.map(|words| words.detail);
                self.links.push(Link { tag, url: url.clone(), detail });
            }
        }
    }

    fn block(&mut self, block: &Markdown, joined: bool) {
        if !joined {
            if !self.starts.is_empty() {
                self.begin(Kind::Spacer, &[if matches!(block, Markdown::Heading(..)) { "gap-wide" } else { "gap" }]);
            }
            self.group += 1;
        }
        match block {
            Markdown::Paragraph(spans) => {
                self.begin(Kind::Text { prefix: String::new(), skip: 0 }, &[]);
                self.spans(spans, &[]);
            }
            Markdown::Heading(level, spans) => {
                let name = format!("h{}", (*level).clamp(1, 4));
                self.begin(Kind::Text { prefix: String::new(), skip: 0 }, &[name.as_str()]);
                self.spans(spans, &[]);
            }
            Markdown::Bullet(depth, marker, spans) => {
                let name = self.item_tag(*depth);
                let leader = format!("\t{marker}\t");
                let skip = i32::try_from(leader.chars().count()).unwrap_or(0);
                let tags: &[&str] = if joined { &[name.as_str(), "tight"] } else { &[name.as_str()] };
                self.begin(Kind::Text { prefix: format!("{}{marker} ", "\t".repeat(*depth)), skip }, tags);
                self.insert(&leader, &["muted"]);
                self.spans(spans, &[]);
            }
            Markdown::Code(language, code) => self.code(language.as_deref(), code),
            Markdown::Table(rows) => self.table(rows),
            Markdown::Place(quotes, items) => {
                let tags = [("quote", quotes), ("inside", items)].into_iter().filter(|(_, level)| **level > 0);
                self.nested = tags.map(|(name, level)| format!("{name}-{level}")).collect();
            }
            Markdown::Rule => {
                self.begin(Kind::Rule, &["rule"]);
                self.insert(&HIDDEN.to_string(), &[]);
            }
        }
    }

    fn item_tag(&self, depth: usize) -> String {
        let name = format!("item-{depth}");
        let table = self.buffer.tag_table();
        if table.lookup(&name).is_none() {
            let tag = gtk::TextTag::new(Some(&name));
            let base = i32::try_from(depth).unwrap_or(0) * DEPTH_STEP;
            let mut tabs = pango::TabArray::new(2, true);
            tabs.set_tab(0, pango::TabAlign::Right, MARKER_STOP);
            tabs.set_tab(1, pango::TabAlign::Left, TEXT_STOP);
            tag.set_left_margin(base);
            tag.set_indent(-TEXT_STOP);
            tag.set_tabs(Some(&tabs));
            table.add(&tag);
        }
        name
    }

    fn code(&mut self, language: Option<&str>, code: &str) {
        if let Some(language) = language {
            self.begin(Kind::Header, &["code-head"]);
            self.insert(language, &[]);
        }
        let lines: Vec<&str> = code.split('\n').collect();
        let mut start = 0;
        for (index, line) in lines.iter().enumerate() {
            let mut tags = vec!["code"];
            if index == 0 && language.is_none() {
                tags.push("code-top");
            }
            if index + 1 == lines.len() {
                tags.push("code-bottom");
            }
            self.begin(Kind::Code, &tags);
            if index == 0 {
                start = self.buffer.char_count();
            }
            self.insert(line, &[]);
        }
        let end = self.buffer.char_count();
        self.jobs.push(Job { start, end, code: code.to_string(), language: language.map(str::to_string) });
    }

    fn table(&mut self, rows: &[Vec<Vec<Span>>]) {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0; columns];
        for (index, row) in rows.iter().enumerate() {
            for (column, cell) in row.iter().enumerate() {
                let text: String = cell.iter().map(|span| span.text.as_str()).collect();
                let layout = self.view.create_pango_layout(None);
                if index == 0 {
                    layout.set_markup(&format!("<b>{}</b>", format::escape(&text)));
                } else {
                    layout.set_text(&text);
                }
                widths[column] = widths[column].max(layout.pixel_size().0);
            }
        }
        let name = format!("table-{}", self.tables);
        self.tables += 1;
        let table = self.buffer.tag_table();
        let tag = table.lookup(&name).unwrap_or_else(|| {
            let tag = gtk::TextTag::new(Some(&name));
            table.add(&tag);
            tag
        });
        let mut tabs = pango::TabArray::new(i32::try_from(columns.saturating_sub(1)).unwrap_or(0), true);
        let mut position = 0;
        for (index, width) in widths.iter().take(columns.saturating_sub(1)).enumerate() {
            position += width + CELL_GAP;
            tabs.set_tab(i32::try_from(index).unwrap_or(0), pango::TabAlign::Left, position);
        }
        tag.set_tabs(Some(&tabs));
        for (index, row) in rows.iter().enumerate() {
            let header = index == 0;
            let paragraph: &[&str] = if header { &[name.as_str(), "row", "row-head"] } else { &[name.as_str(), "row"] };
            self.begin(Kind::Cell, paragraph);
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    self.insert("\t", &[]);
                }
                self.spans(cell, &[]);
            }
        }
    }

    fn finish(self, doc: &Rc<RefCell<Doc>>, generation: u64) {
        let buffer = &self.buffer;
        if self.starts.last().is_some_and(|(start, _)| *start == buffer.char_count()) {
            buffer.insert(&mut buffer.end_iter(), &HIDDEN.to_string());
        }
        let total = buffer.char_count();
        for (index, (start, tags)) in self.starts.iter().enumerate() {
            let end = self.starts.get(index + 1).map_or(total, |next| next.0);
            let (from, to) = (buffer.iter_at_offset(*start), buffer.iter_at_offset(end));
            for tag in tags {
                buffer.apply_tag_by_name(tag, &from, &to);
            }
        }
        {
            let mut doc = doc.borrow_mut();
            doc.lines = self.lines;
            doc.links = self.links;
        }
        for job in self.jobs {
            crate::highlight::paint_plain(buffer, job.start, job.end);
            let (weak, start) = (buffer.downgrade(), job.start);
            let owner: Weak<RefCell<Doc>> = Rc::downgrade(doc);
            let ready = move |segments: crate::highlight::Segments| {
                let current = owner.upgrade().is_some_and(|doc| doc.borrow().generation == generation);
                if let Some(buffer) = weak.upgrade().filter(|_| current) {
                    crate::highlight::paint_segments(&buffer, start, &segments);
                }
            };
            if let Some(segments) = crate::highlight::segments(&job.code, job.language.as_deref(), None, ready) {
                crate::highlight::paint_segments(buffer, job.start, &segments);
            }
        }
    }
}

fn fill(view: &gtk::TextView, doc: &Rc<RefCell<Doc>>, text: &str) {
    let buffer = view.buffer();
    let (generation, buttons) = {
        let mut doc = doc.borrow_mut();
        let table = buffer.tag_table();
        for link in doc.links.drain(..) {
            table.remove(&link.tag);
        }
        doc.generation += 1;
        (doc.generation, doc.buttons)
    };
    buffer.set_text("");
    let mut builder = Builder {
        view,
        buffer,
        starts: Vec::new(),
        lines: Vec::new(),
        links: Vec::new(),
        buttons,
        jobs: Vec::new(),
        group: 0,
        tables: 0,
        nested: Vec::new(),
    };
    let mut previous: Option<Markdown> = None;
    for block in format::markdown(text) {
        let joined = matches!(
            (&previous, &block),
            (Some(Markdown::Bullet(..)), Markdown::Bullet(..)) | (_, Markdown::Place(..))
        );
        builder.block(&block, joined);
        previous = Some(block);
    }
    builder.finish(doc, generation);
}

fn copy_text(buffer: &gtk::TextBuffer, doc: &Doc) -> Option<String> {
    let (start, end) = buffer.selection_bounds()?;
    let (from, to) = (start.offset(), end.offset());
    let first = start.line();
    let last = if end.starts_line() && end.line() > first { end.line() - 1 } else { end.line() };
    let mut parts: Vec<(usize, String)> = Vec::new();
    for number in first..=last {
        let Some(info) = usize::try_from(number).ok().and_then(|index| doc.lines.get(index)) else { continue };
        let Some(begin) = buffer.iter_at_line(number) else { continue };
        let mut finish = begin;
        if !finish.ends_line() {
            finish.forward_to_line_end();
        }
        let (line_start, line_end) = (begin.offset(), finish.offset());
        let slice = |offset: i32| {
            let (low, high) = (offset.max(from), line_end.min(to));
            if high <= low {
                return String::new();
            }
            buffer.text(&buffer.iter_at_offset(low), &buffer.iter_at_offset(high), false).replace(HIDDEN, "")
        };
        match &info.kind {
            Kind::Spacer | Kind::Header | Kind::Rule => {}
            Kind::Code => parts.push((info.group, slice(line_start))),
            Kind::Cell => {
                let cells: Vec<String> = slice(line_start).split('\t').map(|cell| cell.trim().to_string()).collect();
                parts.push((info.group, cells.join("\t")));
            }
            Kind::Text { prefix, skip } => {
                let content = line_start + skip;
                let lead = if from < content { prefix.as_str() } else { "" };
                parts.push((info.group, format!("{lead}{}", slice(content))));
            }
        }
    }
    let mut result = String::new();
    let mut previous = None;
    for (group, text) in parts {
        if let Some(previous) = previous {
            result.push_str(if previous == group { "\n" } else { "\n\n" });
        }
        result.push_str(&text);
        previous = Some(group);
    }
    Some(result.replace(BREAK, "\n"))
}
