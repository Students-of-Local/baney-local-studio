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
        "home/docs/site.com",
        "more/applications/folder.desktop",
        "share/dbus-1/services",
    ] {
        create_dir_all(root.join(dir))?;
    }
    for file in ["x.command", "x.sh", "runner", "script.pdf"] {
        fake(&root.join("home/docs").join(file))?;
    }
    for (file, bytes) in [
        ("a.PDF", &b"%PDF-1.7"[..]),
        ("it's \"100%\"].txt", b"text"),
        ("jar.pdf", b"PK\x03\x04"),
        ("b.docx", b"PK\x03\x04"),
        ("program.txt", b"\x7fELF\x02"),
        ("alias.pdf", b"book\0\0\0\0mark\0\0\0\0"),
    ] {
        write(root.join("home/docs").join(file), bytes)?;
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
        ("share/applications/userapp", "Type=Application\nName=Fox\nNoDisplay=true\n"),
        ("share/applications/wolf", "Type=Application\nName=Fox\n"),
        ("more/applications/fox", "Type=Application\nName=The Fox\n"),
    ] {
        write(root.join(format!("{file}.desktop")), format!("[Desktop Entry]\n{text}"))?;
    }
    for program in ["gnome/gnome-control-center", "gnome/gdbus", "gnome/xdg-open", "gnome/gtk-launch"] {
        fake(&root.join(program))?;
    }
    for program in ["kde/systemsettings", "kde/xdg-open", "kde/gnome-control-center", "bare/gdbus", "bare/xdg-open"] {
        fake(&root.join(program))?;
    }
    write(
        root.join("share/dbus-1/services/org.fake.FileManager1.service"),
        "[D-BUS Service]\nName=org.freedesktop.FileManager1\n",
    )?;
    create_dir_all(root.join("broken/systemsettings"))?;
    write(root.join("broken/gnome-control-center"), "")?;
    let vars = "HOME=home PATH=gnome XDG_DATA_HOME=share XDG_DATA_DIRS=more LOCAL_STUDIO_DATA_DIR=data \
        EUKHE_CODING_AGENT_DIR=agent EUKHE_DAEMON_SOCKET=daemon.sock T3CODE_HOME=t3";
    for (name, value) in vars.split_whitespace().filter_map(|var| var.split_once('=')) {
        std::env::set_var(name, root.join(value));
    }
    std::env::set_current_dir(&root)?;
    plans(&root)?;
    std::env::set_current_dir("/")?;
    Ok(std::fs::remove_dir_all(&root)?)
}

fn fake(path: &Path) -> Result<()> {
    create_dir_all(path.parent().context("no folder")?)?;
    write(path, "#!/bin/sh\n")?;
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
        let mut env = LinkEnv { path, desktop: String::new(), ..LinkEnv::here() };
        match bins {
            "kde" => env.desktop = "KDE".to_string(),
            "bare" => env.data_dirs.clear(),
            _ => {}
        }
        let got = plan(rest, on, &env);
        ensure!(got == want, "{asked}\n  wanted {want}\n  got    {got}");
    }
    for pane in PANES.split_whitespace() {
        let got = plan(&format!("system/{pane}"), Platform::Mac, &LinkEnv::here());
        ensure!(got.starts_with("/usr/bin/open|x-apple.systempreferences:com.apple."), "{pane}: {got}");
    }
    Ok(())
}
