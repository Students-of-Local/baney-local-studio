use std::time::{SystemTime, UNIX_EPOCH};

use gtk::glib;
use local_studio_core::{AccessMode, Harness, NoticeLevel, ThreadStatus, ToolKind, ToolState};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::icons;

pub const DEEPEST: usize = 4;
pub const ACCESS_MODES: [AccessMode; 4] =
    [AccessMode::Supervised, AccessMode::AutoAcceptEdits, AccessMode::Auto, AccessMode::FullAccess];

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

pub fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

pub fn relative(at: i64, now: i64) -> String {
    let seconds = (now - at).max(0) / 1000;
    match seconds {
        0..60 => "now".to_string(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
}

pub fn countdown(until: i64, now: i64) -> String {
    let seconds = (until - now).max(0) / 1000;
    match seconds {
        0..60 => "soon".to_string(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

pub fn duration(millis: u64) -> String {
    let seconds = millis / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

#[allow(clippy::cast_precision_loss)]
pub fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}k", count as f64 / 1_000.0),
        1_000_000..1_000_000_000 => format!("{:.1}M", count as f64 / 1_000_000.0),
        _ => format!("{:.1}B", count as f64 / 1_000_000_000.0),
    }
}

#[allow(clippy::cast_precision_loss)]
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.0} GB", bytes as f64 / 1_000_000_000.0)
}

pub fn harness_of(provider: &str) -> Harness {
    match provider {
        "claude-code" => Harness::ClaudeCode,
        "codex" => Harness::Codex,
        _ => Harness::Eukhe,
    }
}

pub fn harness_label(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::ClaudeCode => Some("Claude Code"),
        Harness::Codex => Some("Codex"),
        Harness::Eukhe => None,
    }
}

pub fn access_label(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Supervised => "Supervised",
        AccessMode::AutoAcceptEdits => "Auto-accept edits",
        AccessMode::Auto => "Auto",
        AccessMode::FullAccess => "Full access",
    }
}

pub fn access_detail(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Supervised => "Ask before commands and file changes.",
        AccessMode::AutoAcceptEdits => "Apply edits without asking; ask before commands.",
        AccessMode::Auto => "Supported providers approve routine actions; others still ask.",
        AccessMode::FullAccess => "Allow commands and edits without prompts.",
    }
}

pub fn default_access_names() -> Vec<&'static str> {
    std::iter::once("Match the model").chain(ACCESS_MODES.iter().map(|mode| access_label(*mode))).collect()
}

pub fn default_access_index(settings: &local_studio_core::Settings) -> u32 {
    if settings.access_follows_model {
        0
    } else {
        access_index(settings.default_access) + 1
    }
}

pub fn set_default_access(settings: &mut local_studio_core::Settings, index: usize) {
    match index.checked_sub(1).and_then(|index| ACCESS_MODES.get(index)) {
        Some(mode) => {
            settings.access_follows_model = false;
            settings.default_access = *mode;
        }
        None => settings.access_follows_model = true,
    }
}

pub fn access_index(mode: AccessMode) -> u32 {
    match mode {
        AccessMode::Supervised => 0,
        AccessMode::AutoAcceptEdits => 1,
        AccessMode::Auto => 2,
        AccessMode::FullAccess => 3,
    }
}

pub fn status(status: ThreadStatus) -> Option<(&'static str, &'static str, &'static str)> {
    match status {
        ThreadStatus::Idle => None,
        ThreadStatus::Working => Some((icons::WORKING, "Working", "status-working")),
        ThreadStatus::Approval => Some((icons::APPROVAL, "Approval", "status-approval")),
        ThreadStatus::Failed => Some((icons::ERROR, "Failed", "status-failed")),
        ThreadStatus::Done => Some((icons::DONE, "Done", "status-done")),
        ThreadStatus::Woke => Some((icons::TIME, "Woke", "status-woke")),
        ThreadStatus::Monitoring => Some((icons::WATCH, "Monitoring", "status-monitoring")),
    }
}

pub fn tool_icon(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => icons::WATCH,
        ToolKind::Edit => icons::EDIT,
        ToolKind::Command => icons::TERMINAL,
        ToolKind::Search => icons::SEARCH,
        ToolKind::Web => icons::WEB,
        ToolKind::Agent => icons::AGENT,
        ToolKind::Other => icons::TOOL,
    }
}

pub fn tool_state(state: ToolState) -> Option<(&'static str, Option<&'static str>)> {
    match state {
        ToolState::Pending | ToolState::Running => Some((icons::WORKING, Some("status-working"))),
        ToolState::Succeeded => None,
        ToolState::Failed => Some((icons::ERROR, Some("status-failed"))),
        ToolState::Aborted => Some((icons::STOP, None)),
    }
}

pub fn notice(level: NoticeLevel) -> (&'static str, &'static str) {
    match level {
        NoticeLevel::Info => (icons::INFO, "dim-label"),
        NoticeLevel::Warning => (icons::WARNING, "warning"),
        NoticeLevel::Error => (icons::ERROR, "error"),
    }
}

pub fn shortcut_label(binding: &str) -> String {
    let accelerator = accelerator(binding);
    gtk::accelerator_parse(&accelerator)
        .map_or(accelerator, |(key, modifiers)| gtk::accelerator_get_label(key, modifiers).to_string())
}

pub fn accelerator(binding: &str) -> String {
    let mut accelerator = String::new();
    let mut key = "";
    for part in binding.split('+').filter(|part| !part.is_empty()) {
        match part {
            "cmd" | "ctrl" | "mod" => {
                accelerator.push_str(if accelerator.contains("<Control>") { "<Super>" } else { "<Control>" });
            }
            "option" | "alt" => accelerator.push_str("<Alt>"),
            "shift" => accelerator.push_str("<Shift>"),
            other => key = other,
        }
    }
    if binding.ends_with("++") {
        key = "plus";
    }
    let key = match key {
        "return" | "enter" => "Return",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "escape" => "Escape",
        "tab" => "Tab",
        "space" => "space",
        "delete" => "Delete",
        "[" => "bracketleft",
        "]" => "bracketright",
        "=" => "equal",
        "-" => "minus",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        other => other,
    };
    if function_key(key) {
        accelerator.push_str(&key.to_uppercase());
    } else {
        accelerator.push_str(key);
    }
    accelerator
}

pub fn function_key(key: &str) -> bool {
    key.strip_prefix(['f', 'F'])
        .is_some_and(|number| !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()))
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
}

pub struct Span {
    pub text: String,
    pub style: Style,
    pub link: Option<String>,
}

pub enum Markdown {
    Paragraph(Vec<Span>),
    Heading(u8, Vec<Span>),
    Bullet(usize, String, Vec<Span>),
    Code(Option<String>, String),
    Table(Vec<Vec<Vec<Span>>>),
    Rule,
    Place(usize, usize),
}

pub fn markdown(text: &str) -> Vec<Markdown> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut reader = Reader::default();
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        reader.read(event, &text[range.clone()], range.end == text.len());
    }
    reader.blocks
}

#[derive(Default)]
struct Reader {
    blocks: Vec<Markdown>,
    spans: Vec<Span>,
    inline: Vec<(Style, Option<String>)>,
    cells: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    language: Option<String>,
    quotes: usize,
    place: (usize, usize),
}

impl Reader {
    fn read(&mut self, event: Event, source: &str, typing: bool) {
        match &event {
            Event::Start(Tag::Paragraph | Tag::HtmlBlock | Tag::Heading { .. })
            | Event::End(TagEnd::Paragraph | TagEnd::HtmlBlock) => self.flush(false),
            Event::Start(Tag::List(_) | Tag::BlockQuote(_) | Tag::CodeBlock(_) | Tag::Table(_))
            | Event::End(TagEnd::Item)
            | Event::Rule => self.flush(true),
            _ => {}
        }
        let (style, link) = self.inline.last().cloned().unwrap_or_default();
        match event {
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => self.text(&text, false),
            Event::Code(text) => self.text(&text, true),
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Start(Tag::Emphasis) => self.inline.push((Style { italic: true, ..style }, link)),
            Event::Start(Tag::Strong) => self.inline.push((Style { bold: true, ..style }, link)),
            Event::Start(Tag::Strikethrough) => self.inline.push((Style { strike: true, ..style }, link)),
            Event::Start(Tag::Link { link_type, dest_url, .. } | Tag::Image { link_type, dest_url, .. }) => {
                let opens = link_type != LinkType::Email && !dest_url.contains(char::is_whitespace);
                self.inline.push((style, link.or_else(|| opens.then(|| dest_url.to_string()))));
                if source.starts_with("![]") {
                    self.text(&dest_url, false);
                }
            }
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image) => {
                drop(self.inline.pop());
            }
            Event::Start(Tag::List(number)) => self.lists.push(number),
            Event::End(TagEnd::List(_)) => drop(self.lists.pop()),
            Event::Start(Tag::Item) => {
                let number = self.lists.last_mut().and_then(Option::as_mut);
                let marker = number.map(|next| format!("{}.", std::mem::replace(next, *next + 1)));
                self.marker = Some(marker.unwrap_or_else(|| "•".to_string()));
            }
            Event::TaskListMarker(done) => {
                let number = self.marker.take().unwrap_or_default().replace('•', "").replace('.', ". ");
                self.marker = Some(number + if done { "✓" } else { "☐" });
            }
            Event::Start(Tag::BlockQuote(_)) => self.quotes += 1,
            Event::End(TagEnd::BlockQuote(_)) => self.quotes -= 1,
            Event::End(TagEnd::Heading(level)) => {
                let underline = source.rsplit('\n').next().unwrap_or("").trim_start_matches([' ', '>']);
                if typing && underline == "-" {
                    return self.flush(false);
                }
                let spans = std::mem::take(&mut self.spans);
                self.flush(true);
                self.block(Markdown::Heading(level as u8, spans));
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                self.language = Some(info.trim().to_string()).filter(|language| !language.is_empty());
            }
            Event::End(TagEnd::CodeBlock) => {
                let code: String = self.spans.drain(..).map(|span| span.text).collect();
                let language = self.language.take();
                self.block(Markdown::Code(language, code.strip_suffix('\n').unwrap_or(&code).to_string()));
            }
            Event::End(TagEnd::TableCell) => self.cells.push(std::mem::take(&mut self.spans)),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => self.rows.push(std::mem::take(&mut self.cells)),
            Event::End(TagEnd::Table) => {
                let rows = std::mem::take(&mut self.rows);
                self.block(Markdown::Table(rows));
            }
            Event::Rule => self.block(Markdown::Rule),
            _ => {}
        }
    }

    fn text(&mut self, text: &str, code: bool) {
        let (style, link) = self.inline.last().cloned().unwrap_or_default();
        let style = Style { code, ..style };
        match self.spans.last_mut().filter(|last| last.style == style && last.link == link) {
            Some(last) => last.text.push_str(text),
            None => self.spans.push(Span { text: text.to_string(), style, link }),
        }
    }

    fn flush(&mut self, item: bool) {
        let mut spans = std::mem::take(&mut self.spans);
        if let Some(last) = spans.last_mut() {
            last.text.truncate(last.text.trim_end_matches('\n').len());
        }
        if let Some(marker) = self.marker.take_if(|_| item || !spans.is_empty()) {
            self.block(Markdown::Bullet(self.lists.len().clamp(1, DEEPEST) - 1, marker, spans));
        } else if !spans.is_empty() {
            self.block(Markdown::Paragraph(spans));
        }
    }

    fn block(&mut self, block: Markdown) {
        let items = if matches!(block, Markdown::Bullet(..)) { 0 } else { self.lists.len().min(DEEPEST) };
        let place = (self.quotes.min(DEEPEST), items);
        if std::mem::replace(&mut self.place, place) != place {
            self.blocks.push(Markdown::Place(place.0, place.1));
        }
        self.blocks.push(block);
    }
}
