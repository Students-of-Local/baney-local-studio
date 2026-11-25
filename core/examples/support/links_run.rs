use std::fs::{self, create_dir_all, set_permissions, write, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use local_studio_core::{link_command, studio_link, LinkEnv, NoticeLevel, Platform, Studio, StudioConfig, Update};

const PANES: &str = "general about software-update storage appearance accessibility displays sound wifi bluetooth \
    network vpn notifications focus screen-time lock-screen privacy screen-recording accessibility-access microphone \
    camera full-disk-access login-items users passwords keyboard mouse trackpad printers battery desktop-dock \
    wallpaper date-time language sharing";

type Notices = Vec<(NoticeLevel, String)>;

struct Quiet;

impl local_studio_core::Waker for Quiet {
    fn wake(&self) {}
}

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
        fake(&root.join("home/docs").join(file), 0)?;
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
        fake(&root.join(program), 0)?;
    }
    for program in ["kde/systemsettings", "kde/xdg-open", "kde/gnome-control-center", "bare/gdbus", "bare/xdg-open"] {
        fake(&root.join(program), 0)?;
    }
    write(
        root.join("share/dbus-1/services/org.fake.FileManager1.service"),
        "[D-BUS Service]\nName=org.freedesktop.FileManager1\n",
    )?;
    fake(&root.join("fail/xdg-open"), 1)?;
    fake(&root.join("bad/xdg-open"), 0)?;
    write(root.join("bad/xdg-open"), "#!/nowhere/sh\n")?;
    create_dir_all(root.join("broken/systemsettings"))?;
    write(root.join("broken/gnome-control-center"), "")?;
    let vars = "HOME=home PATH=gnome LINKS_LOG=log XDG_DATA_HOME=share XDG_DATA_DIRS=more LOCAL_STUDIO_DATA_DIR=data \
        EUKHE_CODING_AGENT_DIR=agent EUKHE_DAEMON_SOCKET=daemon.sock T3CODE_HOME=t3";
    for (name, value) in vars.split_whitespace().filter_map(|var| var.split_once('=')) {
        std::env::set_var(name, root.join(value));
    }
    std::env::remove_var("XDG_CURRENT_DESKTOP");
    std::env::set_current_dir(&root)?;
    plans(&root)?;
    if cfg!(target_os = "linux") {
        clicks(&root)?;
    }
    std::env::set_current_dir("/")?;
    Ok(fs::remove_dir_all(&root)?)
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

fn clicks(root: &Path) -> Result<()> {
    let config = StudioConfig {
        socket_path: None,
        session_dir: None,
        data_dir: None,
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let studio = Studio::new(config, Arc::new(Quiet))?;
    let click = |rest: &str| studio_link(&format!("localstudio://{rest}")).map(|link| studio.open_studio_link(link));
    let settled = |done: &dyn Fn(&Notices) -> bool| {
        let (mut notices, until) = (Notices::new(), Instant::now() + Duration::from_secs(8));
        let mut last = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until && Instant::now() < last {
            notices.extend(studio.tick().into_iter().filter_map(|update| match update {
                Update::Notice { level, text } => Some((level, text)),
                _ => None,
            }));
            if !done(&notices) {
                last = Instant::now() + Duration::from_secs(1);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        notices
    };
    let log = || fs::read_to_string(root.join("log")).unwrap_or_default();
    settled(&|_| true);
    for (index, rest) in
        ["system/displays", "app/Calculator", "file?path=~/docs/b", "reveal?path=~/docs/it's%20%22100%25%22%5D.txt"]
            .into_iter()
            .enumerate()
    {
        let want = format!("{}|", plan(rest, Platform::here(), &LinkEnv::here()));
        click(rest).context("no link")?;
        let notices = settled(&|_| log().lines().count() > index);
        let line = log().lines().nth(index).unwrap_or_default().to_string();
        let pid = line.strip_prefix(&want).with_context(|| format!("{rest} ran {line:?}, not {want:?}"))?;
        ensure!(log().lines().count() == index + 1 && notices.is_empty(), "{rest}: {} {notices:?}", log());
        ensure!(!Path::new("/proc").join(pid).exists(), "{rest} left process {pid}");
    }
    for (bins, rest, want) in [
        ("fail", "file?path=~/docs/a.PDF", "Local Studio could not open a.PDF."),
        ("fail", "reveal?path=~/docs/x.sh", "Local Studio could not show x.sh in Files."),
        ("bad", "file?path=~/docs/a.PDF", "Local Studio could not open a.PDF."),
        ("gnome", "file?path=~/docs/missing.pdf", "missing.pdf is not there."),
    ] {
        std::env::set_var("PATH", root.join(bins));
        click(rest).context("no link")?;
        let notices = settled(&|notices| !notices.is_empty());
        ensure!(notices == [(NoticeLevel::Warning, want.to_string())], "{bins} {rest}: {notices:?}");
    }
    studio.shutdown();
    Ok(())
}
