use std::fs::{create_dir_all, set_permissions, write, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use local_studio_core::{link_command, studio_link, LinkEnv, Platform};

const PANES: &str = "general about software-update storage appearance accessibility displays sound wifi bluetooth \
    network vpn notifications focus screen-time lock-screen privacy screen-recording accessibility-access microphone \
    camera full-disk-access login-items users passwords keyboard mouse trackpad printers battery desktop-dock \
    wallpaper date-time language sharing";

pub fn run() -> Result<()> {
    let base = std::env::args_os().nth(1).map_or_else(std::env::temp_dir, PathBuf::from);
    create_dir_all(&base)?;
    let root = base.canonicalize()?.join(format!("links-{}", std::process::id()));
    for dir in [
        "home/docs/x.app",
        "home/docs/deck.pages",
        "home/docs/Tool/Contents",
        "share/applications/sub",
        "more/applications/folder.desktop",
    ] {
        create_dir_all(root.join(dir))?;
    }
    for file in ["x.command", "x.sh", "runner", "a.PDF", "it's \"100%\"].txt"] {
        fake(&root.join("home/docs").join(file), 0)?;
    }
    for (link, to) in [("trap.pdf", "x.sh"), ("b", "a.PDF"), ("gone", "nowhere")] {
        symlink(to, root.join("home/docs").join(link))?;
    }
    for (file, text) in [
        ("share/applications/org.fake.Calc", "Type=Application\nName = Calculator\n[Desktop Action a]\nName=Other\n"),
        ("share/applications/gone", "Type=Application\nName=Gone\nHidden=true\n"),
        ("more/applications/gone", "Type=Application\nName=Gone\n"),
        ("share/applications/site", "Type=Link\nName=Site\n"),
        ("share/applications/-dash", "Type=Application\nName=Dash\n"),
        ("share/applications/sub/inner", "Type=Application\nName=Inner\n"),
        ("more/applications/far", "Type=Application\nName=Far\n"),
    ] {
        write(root.join(format!("{file}.desktop")), format!("[Desktop Entry]\n{text}"))?;
    }
    for program in ["gnome/gnome-control-center", "gnome/gdbus", "gnome/xdg-open", "gnome/gtk-launch"] {
        fake(&root.join(program), 0)?;
    }
    for (program, exit) in [("kde/systemsettings", 0), ("kde/xdg-open", 0)] {
        fake(&root.join(program), exit)?;
    }
    create_dir_all(root.join("broken/systemsettings"))?;
    write(root.join("broken/gnome-control-center"), "")?;
    let vars = "HOME=home PATH=gnome LINKS_LOG=log XDG_DATA_HOME=share XDG_DATA_DIRS=more LOCAL_STUDIO_DATA_DIR=data \
        EUKHE_CODING_AGENT_DIR=agent EUKHE_DAEMON_SOCKET=daemon.sock T3CODE_HOME=t3";
    for (name, value) in vars.split_whitespace().filter_map(|var| var.split_once('=')) {
        std::env::set_var(name, root.join(value));
    }
    std::env::set_current_dir(&root)?;
    plans(&root)?;
    std::env::set_current_dir("/")?;
    Ok(std::fs::remove_dir_all(&root)?)
}

fn fake(path: &Path, exit: u8) -> Result<()> {
    create_dir_all(path.parent().context("no folder")?)?;
    write(path, format!("#!/bin/sh\n{{ printf '%s|' \"$0\" \"$@\"; echo $$; }} >> \"$LINKS_LOG\"\nexit {exit}\n"))?;
    Ok(set_permissions(path, Permissions::from_mode(0o755))?)
}

fn plan(rest: &str, on: Platform, env: &LinkEnv) -> String {
    let Some(link) = studio_link(&format!("localstudio://{rest}")) else { return "no link".to_string() };
    let argv = link_command(&link, on, env);
    argv.map_or_else(|error| format!("! {error}"), |argv| argv.join("|".as_ref()).to_string_lossy().into_owned())
}

fn plans(root: &Path) -> Result<()> {
    let shown = root.to_string_lossy();
    for case in include_str!("links_plans.txt").lines() {
        let case = case.replace("$R", &shown);
        let (asked, want) = case.split_once(" -> ").context("no ->")?;
        let [on, bins, rest] = asked.split(' ').collect::<Vec<_>>()[..] else { anyhow::bail!("{asked}") };
        let on = match on {
            "mac" => Platform::Mac,
            "windows" => Platform::Windows,
            _ => Platform::Linux,
        };
        let path = std::env::join_paths([PathBuf::from("gnome"), root.join("broken"), root.join(bins)])?;
        let got = plan(rest, on, &LinkEnv { path, ..LinkEnv::here() });
        ensure!(got == want, "{asked}\n  wanted {want}\n  got    {got}");
    }
    for pane in PANES.split_whitespace() {
        let got = plan(&format!("system/{pane}"), Platform::Mac, &LinkEnv::here());
        ensure!(got.starts_with("/usr/bin/open|x-apple.systempreferences:com.apple."), "{pane}: {got}");
    }
    Ok(())
}
