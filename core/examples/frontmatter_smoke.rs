use std::process::ExitCode;

use local_studio_core::frontmatter::{field, strip};

const SKILLS: [(&str, &str, Option<&str>, &str); 12] = [
    ("plain", "---\nname: a\ndescription: Do a thing\n---\nBody\n", Some("Do a thing"), "Body"),
    (
        "folded",
        "---\ndescription: >-\n  Use when the user\n  asks: go\nname: a\n---\nBody\n",
        Some("Use when the user asks: go"),
        "Body",
    ),
    (
        "literal",
        "---\ndescription: |\n  First line\n  second: line\nname: a\n---\nBody\n",
        Some("First line\nsecond: line"),
        "Body",
    ),
    ("double quoted", "---\ndescription: \"Use when: \\\"go\\\"\"\n---\nBody\n", Some("Use when: \"go\""), "Body"),
    ("single quoted", "---\ndescription: 'Use when: it''s time'\n---\nBody\n", Some("Use when: it's time"), "Body"),
    ("comment", "---\ndescription: Do a thing # not this\n---\nBody\n", Some("Do a thing"), "Body"),
    ("nested key", "---\nmetadata:\n  description: inner\ndescription: outer\n---\nBody\n", Some("outer"), "Body"),
    ("no frontmatter", "# Title\n\nBody", None, "# Title\n\nBody"),
    ("unterminated", "---\ndescription: lost\nBody", None, "---\ndescription: lost\nBody"),
    ("empty block", "---\n---\nBody", None, "---\n---\nBody"),
    (
        "crlf",
        "---\r\ndescription: >-\r\n  Use when\r\n  asked\r\nname: a\r\n---\r\nBody\r\n",
        Some("Use when asked"),
        "Body",
    ),
    ("bom", "\u{feff}---\ndescription: Do a thing\n---\nBody", Some("Do a thing"), "Body"),
];

fn main() -> ExitCode {
    let mut failed = 0;
    for (case, text, description, body) in SKILLS {
        let found = field(text, "description");
        let rest = strip(text);
        if found.as_deref() == description && rest == body {
            println!("ok   {case}");
        } else {
            failed += 1;
            println!("FAIL {case}: description {found:?}, want {description:?}; body {rest:?}, want {body:?}");
        }
    }
    println!("{} of {} cases pass", SKILLS.len() - failed, SKILLS.len());
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
