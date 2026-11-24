use std::fs::{create_dir_all, read, remove_dir_all, remove_file, rename, write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const MANY: usize = 1000;

struct Smoke {
    binary: PathBuf,
    home: PathBuf,
    data: PathBuf,
    project: PathBuf,
    failed: usize,
}

fn skill(dir: &Path, name: &str, description: &str, body: &str) -> Result<PathBuf, String> {
    let folder = dir.join(name);
    let text = format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n");
    put(&folder.join("SKILL.md"), &text)?;
    Ok(folder)
}

fn put(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

impl Smoke {
    fn check(&mut self, what: &str, passed: bool) {
        println!("{} {what}", if passed { "ok  " } else { "FAIL" });
        self.failed += usize::from(!passed);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg("skills").args(args).current_dir(&self.project).env_clear().envs([
            ("HOME", &self.home),
            ("LOCAL_STUDIO_DATA_DIR", &self.data),
            ("EUKHE_CODING_AGENT_DIR", &self.home.join("agent")),
        ]);
        command
    }

    fn run(&self, args: &[&str]) -> Result<Output, String> {
        self.command(args).output().map_err(|error| error.to_string())
    }

    fn json(&self, args: &[&str]) -> Result<Value, String> {
        let output = self.run(&[args, &["--json"]].concat())?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            return Err(format!("{args:?}: {text}{}", String::from_utf8_lossy(&output.stderr)));
        }
        serde_json::from_str(&text).map_err(|error| format!("{args:?}: {error}: {text}"))
    }

    fn listed(&self, name: &str) -> Result<Option<Value>, String> {
        let listed = self.json(&["list"])?;
        let found = |skill: &&Value| skill["name"] == name && skill["problem"].is_null();
        Ok(listed.as_array().into_iter().flatten().find(found).cloned())
    }

    fn described(&self, name: &str, description: &str) -> Result<bool, String> {
        Ok(self.listed(name)?.is_some_and(|skill| skill["description"] == description))
    }

    fn problem(&self, part: &str) -> Result<bool, String> {
        let listed = self.json(&["list"])?;
        let mut problems = listed.as_array().into_iter().flatten().filter_map(|skill| skill["problem"].as_str());
        Ok(problems.any(|problem| problem.contains(part)))
    }
}

fn naming(smoke: &mut Smoke) -> Result<(), String> {
    let twin = smoke.listed("twin")?.is_some_and(|skill| skill["source"] == "yours");
    smoke.check("your own skill hides a personal one", twin && smoke.problem("another skill has this name")?);
    let shown = smoke.run(&["list"])?.stdout;
    smoke.check("control characters never reach the terminal", !shown.is_empty() && !shown.contains(&27));
    put(&smoke.data.join("skills/beep\u{7}box\tb/SKILL.md"), "---\nname: beeper\ndescription: Beep\n---\n")?;
    let beep = smoke.run(&["list"])?.stdout;
    let beep = String::from_utf8_lossy(&beep).lines().find(|line| line.starts_with("beeper")).map(str::to_string);
    let clean = beep.is_some_and(|line| line.matches('\t').count() == 2 && !line.contains('\u{7}'));
    smoke.check("nor do a folder name's", clean);
    let usage = smoke.run(&["list", "extra"])?.status.code() == Some(2);
    smoke.check("an unknown argument is a usage error", usage);
    Ok(())
}

fn changes(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let folder = skill(own, "draft", "A pelican", "")?;
    smoke.check("a new skill is listed", smoke.described("draft", "A pelican")?);
    skill(own, "draft", "A penguin", "")?;
    smoke.check("an edit of the same length replaces the old text", smoke.described("draft", "A penguin")?);
    rename(&folder, own.join("moved")).map_err(|error| error.to_string())?;
    let moved = smoke.listed("draft")?.and_then(|skill| skill["path"].as_str().map(|path| path.contains("/moved/")));
    smoke.check("a moved folder is followed", moved == Some(true));
    remove_dir_all(own.join("moved")).map_err(|error| error.to_string())?;
    smoke.check("a deleted skill is gone", smoke.listed("draft")?.is_none());
    put(&own.join("broken/SKILL.md"), "---\nname: [\n---\ntoucan\n")?;
    smoke.check("broken front matter is reported", smoke.problem("no description")?);
    put(&own.join("huge/SKILL.md"), &"cassowary ".repeat(30_000))?;
    smoke.check("a huge SKILL.md is reported", smoke.problem("larger than 256 KB")?);
    skill(&smoke.home.join(".agents/skills"), "broken", "Works", "")?;
    smoke.check("a broken skill takes no name from a good one", smoke.described("broken", "Works")?);
    write(own.join("broken/SKILL.md"), [0xff, 0xfe, 0x00]).map_err(|error| error.to_string())?;
    smoke.check("a binary SKILL.md is reported", smoke.problem("cannot be read as text")?);
    skill(&own.join("kitchen"), "inner", "Nested", "")?;
    smoke.check("a SKILL.md inside a skill is not a skill", smoke.listed("inner")?.is_none());
    Ok(())
}

#[cfg(unix)]
fn links(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    use std::os::unix::fs::symlink;
    let outside = smoke.home.join("outside");
    skill(&outside, "linked", "Outside", "albatross")?;
    let link = |target: &Path, name: &Path| symlink(target, name).map_err(|error| error.to_string());
    link(&outside.join("linked"), &own.join("linked"))?;
    smoke.check("your own folder may link a skill", smoke.listed("linked")?.is_some());
    create_dir_all(own.join("loop")).map_err(|error| error.to_string())?;
    link(own, &own.join("loop/back"))?;
    smoke.check("a link loop ends", smoke.listed("kitchen")?.is_some());
    let shared = smoke.home.join(".agents/skills/shared");
    skill(&smoke.home.join(".agents/skills"), "shared", "Shared", "")?;
    link(&shared, &smoke.home.join(".claude/skills/shared"))?;
    let listed = smoke.json(&["list"])?;
    let shared = listed.as_array().into_iter().flatten().filter(|skill| skill["name"] == "shared").count();
    smoke.check("a skill linked from two folders is listed once", shared == 1);
    create_dir_all(own.join("dotfile")).map_err(|error| error.to_string())?;
    link(&outside.join("linked/SKILL.md"), &own.join("dotfile/SKILL.md"))?;
    remove_file(own.join("linked")).map_err(|error| error.to_string())?;
    smoke.check("your own SKILL.md may be a link", smoke.listed("linked")?.is_some());
    remove_dir_all(own.join("dotfile")).map_err(|error| error.to_string())?;
    let secret = smoke.home.join("secret.txt");
    put(&secret, "condor\n")?;
    let project = smoke.project.join(".agents/skills");
    create_dir_all(project.join("leak")).map_err(|error| error.to_string())?;
    link(&secret, &project.join("leak/SKILL.md"))?;
    smoke.check("a project's may not be", !smoke.json(&["list"])?.to_string().contains("/leak/"));
    skill(&smoke.project.join("kept"), "kept", "Inside", "")?;
    for dir in [".claude", ".eukhe"] {
        create_dir_all(smoke.project.join(dir)).map_err(|error| error.to_string())?;
    }
    link(&smoke.project.join("kept"), &smoke.project.join(".eukhe/skills"))?;
    link(&outside, &smoke.project.join(".claude/skills"))?;
    smoke.check("a project may link a skills folder inside it", smoke.listed("kept")?.is_some());
    smoke.check("but not one outside it", smoke.listed("linked")?.is_none());
    link(&outside.join("linked"), &project.join("linked-too"))?;
    smoke.check("a project may not link a skill in", smoke.listed("linked")?.is_none());
    let mode = std::fs::metadata(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let mode = std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777;
    smoke.check("the index is private", mode == 0o600);
    Ok(())
}

fn trust(smoke: &mut Smoke) -> Result<(), String> {
    skill(&smoke.project.join(".agents/skills"), "local-build", "Build this project", "")?;
    smoke.check("an untrusted project adds nothing", smoke.listed("local-build")?.is_none());
    let state = serde_json::json!({ "version": 1, "projects": [], "trusted": [smoke.project] });
    put(&smoke.data.join("state.json"), &state.to_string())?;
    let found = smoke.listed("local-build")?.is_some_and(|skill| skill["source"] == "project");
    smoke.check("a trusted project's skill is listed", found);
    let home = smoke.home.to_string_lossy().into_owned();
    let elsewhere = smoke.json(&["list", "--project", &home])?;
    smoke.check("and only in that project", !elsewhere.to_string().contains("local-build"));
    create_dir_all(smoke.project.join(".git")).map_err(|error| error.to_string())?;
    create_dir_all(smoke.project.join("src")).map_err(|error| error.to_string())?;
    for dir in [".", "src"] {
        let found = smoke.json(&["list", "--project", dir])?.to_string().contains("local-build");
        smoke.check(&format!("it is listed from --project {dir}"), found);
    }
    put(&smoke.data.join("state.json"), r#"{"version":1,"projects":[],"trusted":[]}"#)?;
    smoke.check("without trust it is gone", smoke.listed("local-build")?.is_none());
    let bytes = read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let kept = bytes.windows(18).any(|part| part == b"Build this project");
    smoke.check("and its text leaves the index", !kept);
    put(&smoke.data.join("state.json"), &state.to_string())?;
    Ok(())
}

fn plugins(smoke: &mut Smoke) -> Result<(), String> {
    for (id, local_only) in [("open", false), ("shut", true)] {
        let dir = smoke.data.join("plugins").join(id);
        let manifest = format!(
            "id = \"{id}\"\nversion = \"0.1.0\"\ndescription = \"A plugin\"\ncapabilities = []\nlocal_only = {local_only}\n"
        );
        put(&dir.join("plugin.toml"), &manifest)?;
        skill(&dir.join("skills"), &format!("{id}-skill"), "From a plugin", "")?;
        skill(&dir.join("skills").join(format!("{id}-skill/examples")), &format!("{id}-inner"), "Not declared", "")?;
    }
    let listed = smoke.listed("open-skill")?.is_some_and(|skill| skill["source"] == "plugin open");
    smoke.check("a plugin's skill is listed", listed);
    let all = smoke.json(&["list"])?.to_string();
    smoke.check("a local-only plugin's is not", !all.contains("shut-"));
    smoke.check("nor is a SKILL.md the plugin does not offer", !all.contains("open-inner"));
    Ok(())
}

fn index(smoke: &mut Smoke) -> Result<(), String> {
    let file = smoke.data.join("skills.sqlite");
    let mut good = 0;
    for round in 0..10 {
        if round < 5 {
            remove_file(&file).map_err(|error| error.to_string())?;
        } else {
            write(&file, [0x5a; 8192]).map_err(|error| error.to_string())?;
        }
        let first = || smoke.command(&["list"]).stdout(Stdio::piped()).spawn();
        let searches: Vec<_> = (0..6).map(|_| first()).collect();
        for child in searches {
            let output = child.and_then(Child::wait_with_output).map_err(|error| error.to_string())?;
            good += usize::from(String::from_utf8_lossy(&output.stdout).contains("kitchen\t"));
        }
    }
    smoke.check("six first lists at once all answer, five times with no index and five with a damaged one", good == 60);
    write(&file, b"not a database, long enough to be read as one page header").map_err(|error| error.to_string())?;
    smoke.check("a file that is no index is rebuilt", smoke.listed("kitchen")?.is_some());
    let mut bytes = read(&file).map_err(|error| error.to_string())?;
    for byte in bytes.iter_mut().skip(4096) {
        *byte = 0x5a;
    }
    write(&file, bytes).map_err(|error| error.to_string())?;
    smoke.check("a damaged index is rebuilt", smoke.listed("kitchen")?.is_some());
    let mut bytes = read(&file).map_err(|error| error.to_string())?;
    for at in 0..bytes.len().saturating_sub(12) {
        if bytes[at..].starts_with(b"Cook at home") {
            bytes[at] = 0xff;
        }
    }
    write(&file, bytes).map_err(|error| error.to_string())?;
    smoke.check("one bad byte in stored text is too", smoke.described("kitchen", "Cook at home")?);
    smoke.check("and answers again", smoke.listed("kitchen")?.is_some());
    Ok(())
}

fn many(smoke: &mut Smoke) -> Result<(), String> {
    let personal = smoke.home.join(".agents/skills");
    let filler = "Open the panel, pick the row, check the total, save the sheet and close it.\n".repeat(60);
    for n in 0..MANY {
        let body = format!("## Steps {n}\n{filler}## Notes\nitem{n} {filler}");
        skill(&personal, &format!("filler-{n}"), &format!("Handle sheet {n} of the ledger"), &body)?;
    }
    let timed = |smoke: &Smoke, name: &str| -> Result<(Duration, bool), String> {
        let start = Instant::now();
        let found = smoke.listed(name)?.is_some();
        Ok((start.elapsed(), found))
    };
    let (first, found) = timed(smoke, "kitchen")?;
    let (again, also) = timed(smoke, "filler-977")?;
    println!("     {MANY} skills: first list {first:?}, next {again:?}");
    smoke.check("a thousand more skills are listed", found && also);
    smoke.check("the first index takes under 5 s", first < Duration::from_secs(5));
    smoke.check("the next start reads the index", again < Duration::from_millis(500));
    Ok(())
}

fn run(smoke: &mut Smoke) -> Result<(), String> {
    let own = smoke.data.join("skills");
    skill(&own, "kitchen", "Cook at home", "Jam.")?;
    skill(&own, "colors", "\"Escape \\e[31mred\"", "escape")?;
    skill(&own, "twin", "A duplicate", "mine")?;
    skill(&smoke.home.join(".claude/skills"), "twin", "A duplicate", "theirs")?;
    naming(smoke)?;
    trust(smoke)?;
    plugins(smoke)?;
    changes(smoke, &own)?;
    #[cfg(unix)]
    links(smoke, &own)?;
    index(smoke)?;
    many(smoke)
}

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [binary, scratch] = args.as_slice() else {
        eprintln!("usage: skills_smoke <local-studio> <scratch>");
        return ExitCode::from(2);
    };
    let scratch = scratch.join("skills");
    let _ = remove_dir_all(&scratch);
    let made = ["home", "data", "project"].map(|dir| create_dir_all(scratch.join(dir)));
    let (Ok(binary), Ok(scratch), true) =
        (binary.canonicalize(), scratch.canonicalize(), made.iter().all(Result::is_ok))
    else {
        eprintln!("cannot use {} or {}", binary.display(), scratch.display());
        return ExitCode::FAILURE;
    };
    let dir = |name: &str| scratch.join(name);
    let mut smoke = Smoke { binary, home: dir("home"), data: dir("data"), project: dir("project"), failed: 0 };
    if let Err(error) = run(&mut smoke) {
        println!("FAIL {error}");
        smoke.failed += 1;
    }
    if smoke.failed > 0 {
        println!("{} failed", smoke.failed);
        return ExitCode::FAILURE;
    }
    println!("PASS skills_smoke");
    ExitCode::SUCCESS
}
