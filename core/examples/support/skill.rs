use local_studio_core::skills::CREATE;

pub const MARKED: &str = "token: zq81fk77";
pub const LEFT: &str = "sh";
pub const FILES: [(&str, &str); 3] =
    [("asks.txt", "book a zorb\nzorb trip\n"), ("where.txt", "zorb.example\n"), ("scripts/x.py", "print(\"````\")\n")];

pub fn text(rest: &str) -> String {
    let name = rest.split_whitespace().next().unwrap_or_default();
    format!(
        "---\nname: {name}\ndescription: Book a zorb\n---\n## Book\n1. For {rest}\n\n```sh\nzorb\n```\n\n{MARKED}\n"
    )
}

pub fn reply(prompt: &str) -> Option<String> {
    let rest = prompt.strip_prefix(CREATE)?.trim_start_matches("\n\n");
    let head = format!("made of {} with {rest:?}", prompt.matches(CREATE).count());
    if rest.is_empty() {
        return Some(head);
    }
    let [(_, asks), (_, places), (script, code)] = FILES;
    let skill = text(rest);
    Some(format!(
        "{head}\n\n```{LEFT}\nzorb --help\n```\n\n````skill\n{skill}````\n\n```asks\n{asks}```\n\n```where\n{places}```\n\n`````file {script}\n{code}`````\n\nDone."
    ))
}
