#[cfg(unix)]
#[path = "support/links_run.rs"]
mod links_run;

use local_studio_core::{link_words, outside_link, refused_link, studio_link, LinkWords, StudioLink};

const OPENED: &[&str] =
    &["https://example.com/a?b=c#d", "HTTP://example.com", "http://127.0.0.1:8080/", "mailto:someone@example.com"];
const REFUSED: &[&str] = &[
    "",
    "//example.com",
    "example.com",
    "docs/guide.md",
    "/usr/bin/true",
    "~/run.sh",
    "file:///usr/share/applications/org.gnome.Calculator.desktop",
    "FILE://localhost/tmp/run.command",
    "smb://example.com/share",
    "vscode://file/tmp/a",
    "x-apple.systempreferences:com.apple.preference.security",
    "javascript:alert(1)",
    "data:text/html,hello",
    " https://example.com",
    "ht\ntps://example.com",
    "httpsx://example.com",
    "localstudio://settings/providers",
    "localstudio://app/Calculator",
    "file:///tmp/a\u{2028}b\u{2029}c\u{202E}txt.exe",
];

const NOT_LINKS: &[&str] = &[
    "localstudio://app/../x",
    "localstudio://app/-a",
    "localstudio://app/%2da",
    "localstudio://app/%2e%2e",
    "localstudio://app/.x",
    "localstudio://app/x#y",
    "localstudio://app/x?y",
    "localstudio://u@app/x",
    "localstudio://app:1/x",
    "LOCALSTUDIO://app/x",
    "localstudio://app/x/y",
    "localstudio://app/x/",
    "localstudio://app/",
    "localstudio://app",
    "localstudio://App/x",
    "localstudio:app/x",
    "//app/x",
    " localstudio://app/x",
    "localstudio://app/a b",
    "localstudio://app/a\"b",
    "localstudio://app/%20a",
    "localstudio://app/a%20",
    "localstudio://app/a%C2%A0",
    "localstudio://app/a\u{202E}b",
    "localstudio://app/a%E2%80%AEb",
    "localstudio://app/a%D8%9Cb",
    "localstudio://app/a%C2%ADb",
    "localstudio://app/a%E2%80%8Bb",
    "localstudio://app/a%E2%81%A0b",
    "localstudio://app/a%EF%BB%BFb",
    "localstudio://app/a%F3%A0%81%81b",
    "localstudio://app/a%E2%80%A8b",
    "localstudio://app/a\u{3164}b",
    "localstudio://app/a%E3%85%A4b",
    "localstudio://app/a%E1%A0%8Eb",
    "localstudio://app/a%E2%81%AAb",
    "localstudio://app/a%EF%B8%8Fb",
    "localstudio://app/a%CD%8Fb",
    "localstudio://app/a%0Ab",
    "localstudio://app/a%00b",
    "localstudio://app/a%2Fb",
    "localstudio://app/a%5Cb",
    "localstudio://app/%zz",
    "localstudio://app/a%4",
    "localstudio://app/%ff",
    "localstudio://file?path=a",
    "localstudio://file?path=",
    "localstudio://file?path=/",
    "localstudio://file?path=~",
    "localstudio://file?path=~someone/a",
    "localstudio://file?path=/a/../b",
    "localstudio://file?path=/a/%2e%2e/b",
    "localstudio://file?path=/a/%2e%2e%2fb",
    "localstudio://file?path=~/../b",
    "localstudio://file?path=/a/./b",
    "localstudio://file?path=//a",
    "localstudio://file?path=/a//b",
    "localstudio://file?path=/a//",
    "localstudio://file?path=~//",
    "localstudio://file?path=/a&x=1",
    "localstudio://file?path=/a&b",
    "localstudio://file?path=/a=b",
    "localstudio://file?path=/a?b",
    "localstudio://file?path=/a#b",
    "localstudio://file?path=/a b",
    "localstudio://file?path=/a%0Ab",
    "localstudio://file?path=/a%00b",
    "localstudio://file?path=/a%5C..%5Cb",
    "localstudio://file?path=/a%E2%80%AEb",
    "localstudio://file?x=1&path=/a",
    "localstudio://file?PATH=/a",
    "localstudio://file/?path=/a",
    "localstudio://file//a",
    "localstudio://reveal/a",
    "localstudio://app?path=/a",
    "localstudio://settings/nope",
    "localstudio://browser/settings",
    "localstudio://system/nope",
    "localstudio://thread/a%2Fb",
    "localstudio://thread/@mac/a",
    "localstudio://thread/@mac%2Fa",
    "localstudio://thread/a.b",
    "localstudio://thread/-a",
    "localstudio://thread/a%20b",
    "localstudio://pair?url=x",
];

fn link(kind: &str, value: &str) -> StudioLink {
    let value = value.to_string();
    match kind {
        "settings" => StudioLink::Settings { section: value },
        "thread" => StudioLink::Thread { id: value },
        "browser" => StudioLink::Browser { page: value },
        "system" => StudioLink::System { pane: value },
        "app" => StudioLink::App { name: value },
        "reveal" => StudioLink::Reveal { path: value },
        "file" => StudioLink::File { path: value },
        _ => StudioLink::Shortcut { name: value },
    }
}

fn grammar() -> anyhow::Result<()> {
    for (rest, kind, value) in [
        ("settings/PROVIDERS", "settings", "providers"),
        ("settings/Source-Control", "settings", "source-control"),
        ("settings/Sites", "settings", "sites"),
        ("thread/0c5f_A-9", "thread", "0c5f_A-9"),
        ("browser/Clear-Data", "browser", "clear-data"),
        ("system/Displays", "system", "displays"),
        ("app/Visual%20Studio%20Code", "app", "Visual Studio Code"),
        ("app/%2541", "app", "%41"),
        ("app/a+b&c=d", "app", "a+b&c=d"),
        ("app/Caf\u{E9}", "app", "Caf\u{E9}"),
        ("app/Caf%C3%A9", "app", "Caf\u{E9}"),
        ("reveal?path=/tmp/a+b%20c/", "reveal", "/tmp/a+b c/"),
        ("reveal?path=%7E/Documents", "reveal", "~/Documents"),
        ("reveal?path=/tmp/a%26b%3Dc%25", "reveal", "/tmp/a&b=c%"),
        ("file?path=%2Ftmp%2Freport.pdf", "file", "/tmp/report.pdf"),
        ("file?path=~/", "file", "~/"),
        ("file?path=/...", "file", "/..."),
        ("shortcut/Focus", "shortcut", "Focus"),
    ] {
        let parsed = studio_link(&format!("localstudio://{rest}"));
        anyhow::ensure!(parsed == Some(link(kind, value)), "{rest} parsed to {parsed:?}");
    }
    for url in NOT_LINKS {
        anyhow::ensure!(studio_link(url).is_none(), "{url:?} is a link");
    }
    for (prefix, limit) in [("app/", 100), ("thread/", 100), ("file?path=/", 1023)] {
        let url = format!("localstudio://{prefix}{}", "a".repeat(limit));
        anyhow::ensure!(studio_link(&url).is_some(), "{prefix} takes no {limit}");
        anyhow::ensure!(studio_link(&format!("{url}a")).is_none(), "{prefix} takes {limit} and one more");
    }
    Ok(())
}

fn words() -> anyhow::Result<()> {
    let manager = if cfg!(target_os = "macos") { "Finder" } else { "Files" };
    let shown = format!("Show b.pdf in {manager}");
    let padded = format!("/tmp/report.pdf{}.command", " ".repeat(300));
    let long = format!("/tmp/report.pdf{}.command", "_".repeat(300));
    let short = format!("Open report.pdf{}\u{2026}{}.command", "_".repeat(20), "_".repeat(21));
    for (kind, value, title) in [
        ("settings", "providers", "Open Providers settings"),
        ("settings", "sites", "Open Sites settings"),
        ("thread", "abc", "Open the thread"),
        ("browser", "extensions", "Open the browser's Extensions page"),
        ("system", "displays", "Open Displays settings on this computer"),
        ("app", "Calculator", "Open Calculator"),
        ("reveal", "/tmp/secret/b.pdf/", &shown),
        ("file", "~/secret/b.pdf", "Open b.pdf"),
        ("file", &padded, "Open report.pdf .command"),
        ("file", &long, &short),
        ("file", "/tmp/ ", "Open a folder or file with a blank name"),
        ("shortcut", "Focus", "Run the shortcut \u{201C}Focus\u{201D}"),
    ] {
        let want = LinkWords { title: title.to_string(), detail: value.to_string(), confirm: kind == "shortcut" };
        let words = link_words(link(kind, value));
        anyhow::ensure!(words == want, "{kind} {value} reads {words:?}");
    }
    Ok(())
}

fn outside() -> anyhow::Result<()> {
    for rest in ["settings/providers", "settings/Archive", "thread/abc"] {
        let url = format!("localstudio://{rest}");
        anyhow::ensure!(outside_link(&url).is_some_and(|link| Some(link) == studio_link(&url)), "{url} did not act");
    }
    let inside =
        "settings/sites browser/history system/displays app/x reveal?path=/a file?path=/a shortcut/x thread/a/b";
    for rest in inside.split(' ') {
        anyhow::ensure!(outside_link(&format!("localstudio://{rest}")).is_none(), "{rest} acts from outside");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    for url in OPENED {
        anyhow::ensure!(refused_link(url).is_none(), "{url:?} was refused");
    }
    for url in REFUSED {
        let message = refused_link(url);
        anyhow::ensure!(message.is_some(), "{url:?} was opened");
        let breaks = ['\n', '\u{2028}', '\u{2029}', '\u{202E}'];
        anyhow::ensure!(!message.unwrap_or_default().contains(breaks), "{url:?} broke or turned the message");
    }
    grammar()?;
    words()?;
    outside()?;
    #[cfg(unix)]
    links_run::run()?;
    println!("PASS links_smoke");
    Ok(())
}
