use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use local_studio_core::{Block, ItemKind, TranscriptOp, Update};

use crate::app::App;
use crate::fixtures::{conversation, item, FOCUSED};
use crate::format::{markdown, Markdown, Span};

pub const SCENES: [&str; 4] = ["58-markdown-1", "58-markdown-2", "58-markdown-3", "58-answer"];
const CASES: [(&str, &str, &str); 30] = [
    ("M5", "- one\n  still one\n- two", "0• one still one|0• two"),
    ("M6", "1. a\n\n   ```sh\n   ls\n   ```\n2. b", "01. a|in 0,1|code sh:ls|in 0,0|02. b"),
    ("M7, M8", "- a\n  - b\n    - c\n\n3. d\n   - e\n     - f\n4. g", "0• a|1• b|2• c|03. d|1• e|2• f|04. g"),
    ("M8 four", "- a\n    - b\n\n      more\n        - c", "0• a|1• b|in 0,2|more|in 0,0|2• c"),
    ("M9", "- [ ] todo\n- [x] done\n\n1. [x] first", "0☐ todo|0✓ done|01. ✓ first"),
    ("M17, M18", "> a\n> b\n>\n> c\n> - item\n> ```\n> x\n> ```", "in 1,0|a b|c|0• item|code :x"),
    ("M1, M2", "**bold *both* bold** ***x***", "bold (b)both(b)(i) bold(b) x(b)(i)"),
    ("M3, M15", r"snake_case_name 2 * 3 * 4 \*not\* &amp;", "snake_case_name 2 * 3 * 4 *not* &"),
    (
        "M12, M13",
        "[w](https://x.test/A_(b)) <https://x.test> <a@b.c>",
        "w<https://x.test/A_(b)> https://x.test<https://x.test> a@b.c",
    ),
    ("M14, M16", "`` a`b `` ~~old~~", "a`b(c) old(s)"),
    ("M22, M24", "one  \ntwo\\\nthree\nfour [a][1]\n\n[1]: https://x.test", "one\ntwo\nthree four a<https://x.test>"),
    ("rule 4", "![alt](/a) ![](/b) [![ok](/c)](/d) [e](<f g>)", "alt</a> /b</b> ok</d> e"),
    ("M4, M23", "Title\n===\n\nSub\n---\n\na\n\n* * *\n\n---", "#1 Title|#2 Sub|a|rule|rule"),
    ("M10", "~~~py\nx\n~~~\n\n    four\n\n     five", "code py:x|code :four\n\n five"),
    ("M11", "|x|y|\n|:-|-:|\n|a \\| b|`c`|", "x;y;/a | b;c(c);/"),
    ("M19", "<div>\n*x*\n</div>\n\n<b>x</b>", "<div>\n*x*\n</div>|<b>x</b>"),
    ("M20", "```rust\nfn", "code rust:fn"),
    ("M21", "", ""),
    ("M21", "\n\n\n", ""),
    ("M21", "```", "code :"),
    ("rule 6", "Steps **open\n-", "Steps **open"),
    ("rule 6", "Steps\n- ", "Steps"),
    ("rule 6", "1. Steps\n\t-", "01. Steps"),
    ("table", "Rows:\n| a | b |\n|---|---|\n| 1 | 2 |\n\nend", "Rows:|a;b;/1;2;/|end"),
    ("table", "- Rows:\n  | a |\n  |--|\n  | 1 |\n- x", "0• Rows:|in 0,1|a;/1;/|in 0,0|0• x"),
    ("table", "> one\n> two\n> | a \\| b | c |\n> |:-:|-:|", "in 1,0|one two|a | b;c;/"),
    ("table", "a\n| b | c |\n|--|\n\n```\na\n|b|\n|-|\n```", "a | b | c | |--||code :a\n|b|\n|-|"),
    ("item", "1. ### In\n   x\n- ## To", "01. In(b)|in 0,1|x|in 0,0|0• To(b)"),
    ("M4", "a\n-\n\nb", "#2 a|b"),
    ("rule 6", "a\n- b", "a|0• b"),
];

pub fn cases(scene: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    SCENES.iter().position(|name| *name == scene).and_then(|page| CASES.chunks(10).nth(page)).unwrap_or_default()
}

pub fn stage(app: &Rc<App>, scene: &str) -> bool {
    if scene != "58-answer" {
        return false;
    }
    conversation(app, false);
    let index = u64::try_from(app.timeline.items().len()).unwrap_or(0);
    let answer = |text: &str, streaming| {
        let thinking = Block::Thinking { text: "They say hi.".to_string() };
        let blocks = vec![thinking, Block::Text { text: text.to_string() }];
        item(90, 3, 1, ItemKind::Assistant { blocks, streaming, error: None, changed_files: Vec::new() })
    };
    let update = |op| Update::Transcript { thread: FOCUSED.to_string(), op };
    app.apply(vec![update(TranscriptOp::Insert { index, item: answer("", true) })]);
    let app = Rc::clone(app);
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        for streaming in [true, false] {
            app.apply(vec![update(TranscriptOp::Update { index, item: answer("Hi there.", streaming) })]);
        }
    });
    true
}

pub fn drawn(widget: &gtk::Widget) -> Result<(), String> {
    if !widget.is_mapped() {
        return Ok(());
    }
    if let Some(view) = widget.downcast_ref::<gtk::TextView>().filter(|view| view.has_css_class("ls-markdown")) {
        let (start, end) = view.buffer().bounds();
        if view.height() == 0 && start != end {
            return Err(format!("{:?} is not drawn", view.buffer().text(&start, &end, false)));
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        drawn(&current)?;
        child = current.next_sibling();
    }
    Ok(())
}

pub fn check(scene: &str) -> Result<(), String> {
    let wrong = cases(scene).iter().filter_map(|(name, text, expected)| {
        let read = markdown(text).iter().map(show).collect::<Vec<_>>().join("|");
        (read != *expected).then(|| format!("\n  {name}: {text:?} read as {read:?}, not {expected:?}"))
    });
    Some(wrong.collect::<String>()).filter(|wrong| !wrong.is_empty()).map_or(Ok(()), Err)
}

fn show(block: &Markdown) -> String {
    match block {
        Markdown::Paragraph(spans) => line(spans),
        Markdown::Heading(level, spans) => format!("#{level} {}", line(spans)),
        Markdown::Bullet(depth, marker, spans) => format!("{depth}{marker} {}", line(spans)),
        Markdown::Code(language, code) => format!("code {}:{code}", language.as_deref().unwrap_or("")),
        Markdown::Table(rows) => {
            rows.iter().map(|row| row.iter().map(|cell| line(cell) + ";").collect::<String>() + "/").collect()
        }
        Markdown::Rule => "rule".to_string(),
        Markdown::Place(quotes, items) => format!("in {quotes},{items}"),
    }
}

fn line(spans: &[Span]) -> String {
    let show = |Span { text, style, link }: &Span| {
        let flags = [(style.bold, "(b)"), (style.italic, "(i)"), (style.code, "(c)"), (style.strike, "(s)")];
        let flags: String = flags.iter().filter(|(set, _)| *set).map(|(_, name)| *name).collect();
        text.clone() + &flags + &link.as_ref().map(|url| format!("<{url}>")).unwrap_or_default()
    };
    spans.iter().map(show).collect()
}
