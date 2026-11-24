use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use local_studio_core::{
    PromptDraft, Studio, StudioConfig, ThreadSettings, Update, UsagePeriod, Waker, WorktreeChoice,
};

const LIMIT: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_secs(8);
const SIGN_INS: [&str; 14] = [
    ".claude.json",
    ".claude/.claude.json",
    ".claude/.credentials.json",
    ".claude-work/.claude.json",
    ".claude-work/.credentials.json",
    ".codex/auth.json",
    ".codex-work/auth.json",
    ".config/gh/hosts.yml",
    ".config/google-chrome/Default/Cookies",
    ".mozilla/firefox/main.default/cookies.sqlite",
    ".cloudflared/cert.pem",
    "Library/Keychains/login.keychain-db",
    "Library/Application Support/Google/Chrome/Default/Cookies",
    ".t3/userdata/cloud-auth-token.json",
];

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scratch] = args.as_slice() else {
        eprintln!("usage: sign_in_files_smoke <new scratch dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(scratch)) {
        Ok(()) => {
            println!("PASS sign_in_files_smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL sign_in_files_smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn fifo(path: PathBuf) -> Result<PathBuf> {
    fs::create_dir_all(path.parent().context("parent")?)?;
    let status = Command::new("mkfifo").arg(&path).status().context("run mkfifo")?;
    ensure!(status.success(), "mkfifo {} failed", path.display());
    Ok(path)
}

fn being_read(path: &Path) -> bool {
    OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(path).is_ok()
}

fn run(scratch: &Path) -> Result<()> {
    fs::create_dir(scratch).with_context(|| format!("create {} (it must not exist)", scratch.display()))?;
    let scratch = scratch.canonicalize()?;
    let control = fifo(scratch.join("control"))?;
    let read = control.clone();
    std::thread::spawn(move || fs::read(read).ok());
    let started = Instant::now();
    while !being_read(&control) {
        ensure!(started.elapsed() < LIMIT, "a blocked read of the control file was not seen");
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("ok: a read of a watched file is seen");

    let home = scratch.join("home");
    let watched: Vec<PathBuf> = SIGN_INS.iter().map(|name| fifo(home.join(name))).collect::<Result<_>>()?;
    let count = watched.len();
    std::thread::spawn(move || loop {
        if let Some(path) = watched.iter().find(|path| being_read(path)) {
            println!("FAIL sign_in_files_smoke: {} was opened", path.display());
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    });
    let (data, project) = (scratch.join("data"), scratch.join("project"));
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&project)?;
    fs::write(data.join("settings.json"), r#"{"automaticUpdates":false,"startController":false}"#)?;
    for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "LOCAL_STUDIO_DATA_DIR", "T3CODE_HOME", "XDG_CONFIG_HOME"] {
        std::env::remove_var(name);
    }
    std::env::set_var("LOCAL_STUDIO_CONTROLLER_URL", "http://127.0.0.1:9");
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", scratch.join("agent"));
    std::env::set_var("LOCAL_STUDIO_CLAUDE_BIN", "false");
    std::env::set_var("LOCAL_STUDIO_CODEX_BIN", "false");
    let config = StudioConfig {
        socket_path: Some(scratch.join("d.sock").to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: Some(data.to_string_lossy().into_owned()),
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
    studio.load_providers();
    studio.load_usage("usage".to_string(), UsagePeriod::Week);
    studio.local_snapshot("local".to_string());
    studio.add_project(project.to_string_lossy().into_owned());
    let drafts = ["claude-code", "codex"];
    for provider in drafts {
        let settings = ThreadSettings {
            provider: Some(provider.to_string()),
            model: Some("smoke".to_string()),
            thinking_level: None,
            access: None,
            worktree: WorktreeChoice::Local,
        };
        let prompt = PromptDraft { text: "hello".to_string(), images: Vec::new(), files: Vec::new() };
        studio.start_thread(provider.to_string(), Some(project.to_string_lossy().into_owned()), prompt, settings);
    }
    let mut answered = Vec::new();
    let begun = Instant::now();
    while begun.elapsed() < QUIET || answered.len() < drafts.len() + 2 {
        ensure!(begun.elapsed() < LIMIT, "Local Studio answered only {answered:?}");
        for update in studio.tick() {
            match update {
                Update::ThreadStarted { draft: id, .. }
                | Update::RequestFailed { request: id, .. }
                | Update::Usage { request: id, .. } => answered.push(id),
                _ => {}
            }
        }
        answered.sort();
        answered.dedup();
        std::thread::sleep(Duration::from_millis(20));
    }
    studio.shutdown();
    println!("ok: {answered:?} answered and none of {count} sign-in files was opened");
    Ok(())
}
