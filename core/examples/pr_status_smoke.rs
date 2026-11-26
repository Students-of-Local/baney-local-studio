use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use local_studio_core::{PullRequest, Studio, StudioConfig, Update, Waker};
use serde_json::json;

// One app second lasts SECOND_MS here, so the 120 s refresh is 1.2 s and the 600 s back-off is 6 s.
const SECOND_MS: u64 = 10;
const BRANCHES: usize = 6;
const LIMIT: Duration = Duration::from_secs(15);

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

fn main() -> ExitCode {
    if !cfg!(feature = "test-hooks") {
        eprintln!("pr_status_smoke needs --features test-hooks");
        return ExitCode::from(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scratch] = args.as_slice() else {
        eprintln!("usage: pr_status_smoke <new scratch dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(scratch)) {
        Ok(()) => {
            println!("PASS pr_status_smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL pr_status_smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn seconds(app_seconds: u64) -> Duration {
    Duration::from_millis(app_seconds * SECOND_MS)
}

struct Shim {
    log: PathBuf,
    pulls: PathBuf,
    fail: PathBuf,
}

impl Shim {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.calls().len()
    }

    fn serve(&self, checks_on_three: &str) -> Result<()> {
        let rollup = |conclusion: &str, status: &str| json!([{ "conclusion": conclusion, "status": status }]);
        let pull = |number: u64, head: &str, state: &str, rollup: serde_json::Value| {
            json!({
                "number": number, "title": format!("PR {number}"), "url": format!("https://github.com/o/r/pull/{number}"),
                "state": state, "headRefName": head, "baseRefName": "main", "statusCheckRollup": rollup,
            })
        };
        let three =
            if checks_on_three == "passing" { rollup("SUCCESS", "COMPLETED") } else { rollup("", "IN_PROGRESS") };
        let fork = |mut pull: serde_json::Value, owner: &str| {
            pull["isCrossRepository"] = json!(true);
            pull["headRepositoryOwner"] = json!({ "login": owner });
            pull
        };
        let pulls = json!([
            fork(pull(17, "thread-2", "OPEN", json!([])), "stranger"),
            fork(pull(16, "thread-6", "MERGED", json!([])), "stranger"),
            fork(pull(15, "thread-5", "CLOSED", json!([])), "Smoke"),
            pull(14, "thread-4", "MERGED", rollup("SUCCESS", "COMPLETED")),
            pull(13, "thread-3", "OPEN", three),
            pull(12, "thread-2", "OPEN", rollup("FAILURE", "COMPLETED")),
            pull(11, "thread-1", "OPEN", rollup("SUCCESS", "COMPLETED")),
            pull(3, "thread-1", "CLOSED", json!([])),
        ]);
        std::fs::create_dir_all(&self.pulls)?;
        std::fs::write(self.pulls.join("all.json"), pulls.to_string())?;
        for index in 1..=BRANCHES {
            let head = format!("thread-{index}");
            let newest: Vec<&serde_json::Value> =
                pulls.as_array().into_iter().flatten().filter(|pull| pull["headRefName"] == head).take(1).collect();
            std::fs::write(self.pulls.join(format!("{head}.json")), json!(newest).to_string())?;
        }
        Ok(())
    }
}

fn shim(scratch: &Path) -> Result<Shim> {
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin)?;
    let shim = Shim { log: scratch.join("gh.log"), pulls: scratch.join("pulls"), fail: scratch.join("gh-fails") };
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ -e '{}' ]; then\n  echo 'GraphQL: API rate limit exceeded for user ID 1.' >&2\n  exit 1\nfi\nhead=$(printf '%s\\n' \"$@\" | sed -n '/^--head$/{{n;p;}}')\ncat '{}'/\"${{head:-all}}\".json\n",
        shim.log.display(),
        shim.fail.display(),
        shim.pulls.display()
    );
    let gh = bin.join("gh");
    std::fs::write(&gh, script)?;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs)?);
    Ok(shim)
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(dir).status().with_context(|| format!("git {args:?}"))?;
    ensure!(status.success(), "git {args:?} failed in {}", dir.display());
    Ok(())
}

fn repository(scratch: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo)?;
    git(&repo, &["init", "--quiet", "--initial-branch", "main"])?;
    std::fs::write(repo.join("README.md"), "smoke\n")?;
    git(&repo, &["add", "README.md"])?;
    git(&repo, &["-c", "user.name=Smoke", "-c", "user.email=smoke@localhost", "commit", "--quiet", "-m", "start"])?;
    let mut worktrees = Vec::new();
    for index in 1..=BRANCHES {
        let worktree = scratch.join("worktrees").join(format!("thread-{index}"));
        let path = worktree.to_string_lossy().into_owned();
        git(&repo, &["worktree", "add", "--quiet", "-b", &format!("thread-{index}"), &path])?;
        worktrees.push(worktree);
    }
    git(&repo, &["remote", "add", "fork", "git@github.com:smoke/r.git"])?;
    Ok((repo, worktrees))
}

struct App {
    studio: Arc<Studio>,
    rows: BTreeMap<String, Option<PullRequest>>,
}

impl App {
    fn open(scratch: &Path, repo: &Path, worktrees: &[PathBuf]) -> Result<Self> {
        let data = scratch.join("data");
        std::fs::create_dir_all(&data)?;
        let threads: serde_json::Map<String, serde_json::Value> = worktrees
            .iter()
            .enumerate()
            .map(|(index, worktree)| {
                let thread = json!({ "harness": "claudeCode", "cwd": worktree, "project": repo, "title": format!("thread {}", index + 1) });
                (format!("t{}", index + 1), thread)
            })
            .collect();
        let state = json!({ "version": 1, "projects": [{ "path": repo, "name": "repo" }], "threads": threads });
        std::fs::write(data.join("state.json"), state.to_string())?;
        let config = StudioConfig {
            socket_path: Some(scratch.join("d.sock").to_string_lossy().into_owned()),
            session_dir: None,
            data_dir: Some(data.to_string_lossy().into_owned()),
            launch: None,
            controller_main: None,
            take_over_serve: false,
        };
        let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Self { studio, rows: BTreeMap::new() })
    }

    fn pump(&mut self) {
        for update in self.studio.tick() {
            if let Update::Threads { threads } = update {
                self.rows = threads.into_iter().map(|row| (row.id, row.pull_request)).collect();
            }
        }
    }

    fn checks(&self, thread: &str) -> Option<String> {
        self.rows.get(thread).cloned().flatten().map(|pull| format!("#{} {} {}", pull.number, pull.state, pull.checks))
    }

    fn settle(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<()> {
        let started = Instant::now();
        loop {
            self.pump();
            if done(self) {
                return Ok(());
            }
            if started.elapsed() > LIMIT {
                bail!("timed out waiting for {what}; rows {:?}", self.rows);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn hold(&mut self, shim: &Shim, calls: usize, how_long: Duration, repo: &str, what: &str) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < how_long {
            self.studio.git_status(repo.to_string());
            self.pump();
            ensure!(shim.count() == calls, "{what}: gh ran {} more times: {:?}", shim.count() - calls, shim.calls());
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

fn chips_filled(app: &App, three: &str) -> bool {
    app.checks("t1").as_deref() == Some("#11 OPEN passing")
        && app.checks("t2").as_deref() == Some("#12 OPEN failing")
        && app.checks("t3") == Some(format!("#13 OPEN {three}"))
        && app.checks("t4").as_deref() == Some("#14 MERGED passing")
        && app.checks("t5").as_deref() == Some("#15 CLOSED none")
        && app.rows.contains_key("t6")
        && app.checks("t6").is_none()
}

fn run(scratch: &Path) -> Result<()> {
    std::fs::create_dir(scratch).with_context(|| format!("create {} (it must not exist)", scratch.display()))?;
    let scratch = scratch.canonicalize()?;
    let home = scratch.join("home");
    std::fs::create_dir_all(&home)?;
    std::env::set_var("HOME", &home);
    std::env::set_var("EUKHE_CODING_AGENT_DIR", scratch.join("agent"));
    std::env::set_var("LOCAL_STUDIO_NO_UPDATE_CHECK", "1");
    std::env::set_var("LOCAL_STUDIO_TEST_SECOND_MS", SECOND_MS.to_string());
    std::env::set_var("LOCAL_STUDIO_TEST_TIMER_MS", "100");
    for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "LOCAL_STUDIO_DATA_DIR", "GH_TOKEN", "GITHUB_TOKEN"] {
        std::env::remove_var(name);
    }
    let shim = shim(&scratch)?;
    shim.serve("pending")?;
    let (repo, worktrees) = repository(&scratch)?;
    let repo_text = repo.to_string_lossy().into_owned();
    let mut app = App::open(&scratch, &repo, &worktrees)?;

    app.settle("every chip from the first list", |app| chips_filled(app, "pending"))?;
    let first = Instant::now();
    let calls = shim.calls();
    ensure!(calls.len() == 1, "{BRANCHES} branches of one repository took {} gh calls: {calls:?}", calls.len());
    ensure!(
        calls[0]
            == "pr list --state all --limit 100 --json number,title,url,state,headRefName,baseRefName,statusCheckRollup,isCrossRepository,headRepositoryOwner",
        "unexpected gh call {:?}",
        calls[0]
    );
    println!(
        "ok: {BRANCHES} thread branches of one repository took 1 gh call; the branch with no PR, and the branches with a stranger's fork PR of the same name, show none of those"
    );

    app.hold(&shim, 1, seconds(30), &repo_text, "asked again 30 s later")?;
    println!("ok: asked again within 30 s (thread list and git status) made no new call");

    shim.serve("passing")?;
    std::thread::sleep(seconds(121).saturating_sub(first.elapsed()));
    app.settle("the chip refreshed after 120 s", |app| chips_filled(app, "passing"))?;
    ensure!(shim.count() == 2, "the refresh after 120 s took {} calls", shim.count() - 1);
    println!("ok: after 121 s one call refreshed the chips");

    std::fs::write(&shim.fail, "")?;
    let refreshed = Instant::now();
    let started = Instant::now();
    while shim.count() < 3 {
        app.pump();
        ensure!(started.elapsed() < LIMIT, "no refresh after the second 120 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(refreshed.elapsed() >= seconds(110), "refreshed again after {:?}", refreshed.elapsed());
    app.hold(&shim, 3, seconds(590), &repo_text, "after a rate-limited gh call")?;
    ensure!(chips_filled(&app, "passing"), "a failed refresh dropped the chips: {:?}", app.rows);
    println!("ok: a rate-limited gh call made no further call for 590 s and kept the chips");

    std::fs::remove_file(&shim.fail)?;
    let started = Instant::now();
    while shim.count() < 4 {
        app.pump();
        ensure!(started.elapsed() < LIMIT, "no call after the 600 s back-off");
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("ok: the repository was asked again after the back-off");
    app.studio.shutdown();
    Ok(())
}
