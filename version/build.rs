use std::process::Command;

const CHANNELS: [&str; 3] = ["stable", "nightly", "dev"];

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

fn stamped(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn main() {
    let base = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = stamped("LOCAL_STUDIO_VERSION").unwrap_or_else(|| {
        if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
            println!("cargo:rerun-if-changed={dir}/HEAD");
            println!("cargo:rerun-if-changed={dir}/logs/HEAD");
        }
        let sha = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "0000000".to_string());
        format!("{base}-dev.0+local.{sha}")
    });
    let channel = stamped("LOCAL_STUDIO_CHANNEL").unwrap_or_else(|| "dev".to_string());
    assert!(
        version == base || version.starts_with(&format!("{base}-")),
        "LOCAL_STUDIO_VERSION {version} is not the workspace version {base} or one of its pre-releases"
    );
    assert!(CHANNELS.contains(&channel.as_str()), "LOCAL_STUDIO_CHANNEL {channel} is not one of {CHANNELS:?}");
    println!("cargo:rustc-env=LOCAL_STUDIO_BUILD_VERSION={version}");
    println!("cargo:rustc-env=LOCAL_STUDIO_BUILD_CHANNEL={channel}");
}
