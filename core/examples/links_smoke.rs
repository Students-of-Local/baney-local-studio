use local_studio_core::refused_link;

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
];

fn main() -> anyhow::Result<()> {
    for url in OPENED {
        anyhow::ensure!(refused_link(url).is_none(), "{url:?} was refused");
    }
    for url in REFUSED {
        let message = refused_link(url);
        anyhow::ensure!(message.is_some(), "{url:?} was opened");
        anyhow::ensure!(!message.unwrap_or_default().contains('\n'), "{url:?} broke the message over two lines");
    }
    println!("PASS links_smoke");
    Ok(())
}
