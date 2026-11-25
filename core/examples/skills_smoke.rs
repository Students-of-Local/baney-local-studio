use std::fs::{create_dir_all, read, remove_dir_all, remove_file, rename, write};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use local_studio_core::{Mention, MentionKind, Studio, StudioConfig, Update, Waker};
use serde_json::{json, Value};

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

fn names(hits: &Value) -> Vec<&str> {
    hits.as_array().into_iter().flatten().filter_map(|hit| hit["name"].as_str()).collect()
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

    fn first(&self, args: &[&str]) -> Result<String, String> {
        Ok(names(&self.json(&[&["search"], args].concat())?).first().copied().unwrap_or_default().to_string())
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

fn words(smoke: &mut Smoke) -> Result<(), String> {
    let hits = smoke.json(&["search", "marmalade"])?;
    let part = hits[0]["parts"][0] == "Preserves" && names(&hits) == ["kitchen"];
    smoke.check("a word in one section finds that skill and part", part);
    let found = smoke.first(&["why did the glow vanish from my lamp"])? == "bulbs";
    smoke.check("asks.txt finds a skill whose SKILL.md shares no word", found);
    let asked = smoke.run(&["read", "bulbs"])?;
    let text = String::from_utf8_lossy(&asked.stdout).into_owned();
    smoke.check("a short skill reads whole", asked.status.success() && text.contains("Turn it off first"));
    smoke.check("asks.txt is never shown", !text.contains("vanish") && !text.contains("asks.txt"));
    let asks = smoke.data.join("skills/bulbs/asks.txt");
    put(&asks, "my ceiling went dark\n")?;
    let fresh = smoke.first(&["why is the ceiling dark"])? == "bulbs" && smoke.first(&["glow vanish"])?.is_empty();
    smoke.check("an edit of asks.txt alone is seen", fresh);
    let shown = smoke.run(&["search", "windy"])?.stdout;
    smoke.check(
        "a long description is cut to one short line",
        shown.len() < 400 && String::from_utf8_lossy(&shown).lines().next().is_some_and(|line| line.ends_with("...")),
    );
    let nothing = smoke.run(&["search", "what", "is", "the"])?;
    let quiet = nothing.status.success() && String::from_utf8_lossy(&nothing.stdout) == "nothing found\n";
    smoke.check("stop words alone find nothing, exit 0", quiet);
    let long = "jam ".repeat(2500);
    for odd in ["\"jam* NEAR(a b) -c : ^ AND OR NOT", "name:jam {x} 😀 (", long.as_str()] {
        let found = smoke.first(&["--", odd])? == "kitchen";
        smoke.check(&format!("{odd:.20} is plain words"), found);
    }
    smoke.check("accents fold", smoke.first(&["creme brulee"])? == "kitchen");
    smoke.check("an unspaced language is found by part of a word", smoke.first(&["发票"])? == "invoices");
    let usage = smoke.run(&["search"])?.status.code() == Some(2) && smoke.run(&["read"])?.status.code() == Some(2);
    smoke.check("no words is a usage error", usage);
    let control = smoke.run(&["search", "escape"])?.stdout;
    smoke.check("a search prints no control character", !control.is_empty() && !control.contains(&27));
    let stop = smoke.run(&["search", "what's", "the"])?.stdout;
    smoke.check("a contraction alone finds nothing", stop == b"nothing found\n");
    smoke.check("a whole sentence without spaces is found", smoke.first(&["如何开发票"])? == "invoices");
    smoke.check("Thai too", smoke.first(&["ราคา"])? == "prices");
    smoke.check("accents in two parts fold", smoke.first(&["cre\u{300}me"])? == "kitchen");
    let inside = smoke.first(&["开票"])? == "invoices" && smoke.first(&["pdf发票"])? == "invoices";
    smoke.check("nor is its section text or a name in capitals missed", inside);
    let hits = smoke.json(&["search", "ladder rung", "--in", "ladder", "--limit", "8"])?;
    let parts = hits[0]["parts"].as_array().map_or(0, Vec::len);
    smoke.check("--in looks in one skill, at --limit parts", names(&hits) == ["ladder"] && parts == 8);
    let shown = smoke.run(&["search", "ladder rung", "--in", "ladder", "--limit", "8"])?.stdout;
    smoke.check("and prints them all", String::from_utf8_lossy(&shown).matches("Rung").count() == 8);
    let none = !smoke.run(&["search", "rung", "--in", "lader"])?.status.success();
    smoke.check("--in with no such skill is an error", none);
    smoke.check("--limit cuts the list", names(&smoke.json(&["search", "marmalade rung", "--limit", "1"])?).len() == 1);
    let example = smoke.json(&["search", "gargoyle"])?;
    smoke.check("a heading in a longer fence is no part", example[0]["parts"].as_array().is_some_and(Vec::is_empty));
    Ok(())
}

fn places(smoke: &mut Smoke) -> Result<(), String> {
    let ask = "marmalade jam video";
    let plain = smoke.json(&["search", ask])?;
    let video = smoke.first(&["--for", "https://m.tubes.test/watch?v=x", ask])? == "tubes";
    smoke.check("a site's skill comes first there", video && names(&plain).first() == Some(&"kitchen"));
    let none = smoke.json(&["search", "--for", "https://nothing.test", ask])?;
    smoke.check("a site with no skill changes nothing", names(&none) == names(&plain));
    let other = smoke.first(&["--for", "https://eviltubes.test/", ask])? == "kitchen";
    smoke.check("a host that only ends in the name is another site", other);
    let app = smoke.json(&["search", "--for", "Paint Pro"])?;
    smoke.check("where.txt names an app, no words needed", names(&app) == ["painter"]);
    let apps = smoke.first(&["--for", "draw#"])? == "painter" && smoke.first(&["--for", "Ink/Wash"])? == "painter";
    let less = smoke.first(&["--for", "Draw"])?.is_empty() && smoke.first(&["--for", "Ink"])?.is_empty();
    smoke.check("an app's name is not cut like an address", apps && less);
    let words = smoke.first(&["open paint pro please"])? == "painter";
    smoke.check("its words are searched too", words && smoke.first(&["a zither hunt now"])? == "site-learning");
    let alone = smoke.json(&["search", "--for", "tubes.test", "marmalade"])?;
    smoke.check("a site's skill with none of the words comes first", names(&alone) == ["tubes", "kitchen"]);
    for (place, skill) in [
        ("tubes.test:8080", "tubes"),
        ("tubes.test/in?next=https://notes.test/", "tubes"),
        ("http://localhost:3000/a", "workbench"),
        ("localhost:3000", "workbench"),
        ("http://[fd00::1]:80/", "workbench"),
        ("notes.test", "workbench"),
    ] {
        smoke.check(&format!("{place} is {skill}"), smoke.first(&["--for", place])? == skill);
    }
    let sites = smoke.json(&["search", "--for", "notes.test"])?[0]["places"].clone();
    let sites_only = sites == serde_json::json!(["[fd00::1]", "bold.test", "localhost", "notes.test", "pages.test"]);
    smoke.check("a note beside a site is not a place", sites_only);
    let shown = smoke.run(&["search", "beacon"])?.stdout;
    smoke.check("a place prints no control character", !shown.is_empty() && !shown.contains(&27));
    let unrelated = smoke.json(&["search", "--for", "docs.forms.test", "marmalade"])?;
    smoke.check("a skill that only links the site needs a word too", names(&unrelated) == ["kitchen"]);
    let away = smoke.first(&["fill brushes draw"])? == "painter";
    let linked = smoke.json(&["search", "--for", "docs.forms.test", "fill brushes draw"])?;
    let listed = away && linked[0]["name"] == "forms" && linked[0]["places"].as_array().is_some_and(Vec::is_empty);
    smoke.check("a site linked twice counts, and is not listed as the skill's own", listed);
    Ok(())
}

fn named(smoke: &mut Smoke) -> Result<(), String> {
    for form in ["site learning", "SiteLearning", "site-learning", "old tricks"] {
        smoke.check(&format!("{form} is site-learning"), smoke.first(&[form])? == "site-learning");
    }
    for name in ["mark", "mark++"] {
        let within = smoke.json(&["search", "pen", "--in", name])?;
        smoke.check(&format!("{name} is itself"), names(&within) == [name] && smoke.first(&[name])? == name);
    }
    let read = smoke.json(&["read", "Old-Tricks"])?;
    smoke.check("read takes an old name", read["name"] == "site-learning");
    let unknown = smoke.run(&["read", "site", "lerning"])?;
    let text = String::from_utf8_lossy(&unknown.stdout).into_owned();
    smoke.check("an unknown name names the nearest", !unknown.status.success() && text.contains("site-learning"));
    let typo = String::from_utf8_lossy(&smoke.run(&["read", "kitchn"])?.stdout).into_owned();
    smoke.check("a misspelled word is near too", typo.contains("near it: kitchen"));
    let hits = smoke.json(&["search", "duplicate"])?;
    let own = names(&hits) == ["twin"] && hits[0]["source"] == "yours";
    smoke.check("a hidden skill is not found", own);
    Ok(())
}

fn naming(smoke: &mut Smoke) -> Result<(), String> {
    let twin = smoke.listed("twin")?.is_some_and(|skill| skill["source"] == "yours");
    smoke.check("your own skill hides a personal one", twin && smoke.problem("another skill has this name")?);
    let shown = smoke.run(&["list"])?.stdout;
    smoke.check("control characters never reach the terminal", !shown.is_empty() && !shown.contains(&27));
    put(&smoke.data.join("skills/beep\u{7}box\tb\u{9b}c/SKILL.md"), "---\nname: beeper\ndescription: Beep\n---\n")?;
    let beep = smoke.run(&["list"])?.stdout;
    let beep = String::from_utf8_lossy(&beep).lines().find(|line| line.starts_with("beeper")).map(str::to_string);
    let clean = beep
        .is_some_and(|line| line.matches('\t').count() == 2 && !line.contains(|c: char| c.is_control() && c != '\t'));
    smoke.check("nor do a folder name's", clean);
    let raw = smoke.run(&["list", "--json"])?.stdout;
    smoke.check("nor do they in the JSON", !String::from_utf8_lossy(&raw).trim_end().contains(char::is_control));
    let typo = smoke.run(&["list", "--project", "nowhere"])?;
    smoke.check("a project folder that is not there is an error", !typo.status.success());
    let usage = smoke.run(&["list", "extra"])?.status.code() == Some(2);
    smoke.check("an unknown argument is a usage error", usage);
    Ok(())
}

fn reading(smoke: &mut Smoke) -> Result<(), String> {
    let long = smoke.json(&["read", "long"])?;
    let text = long["text"].as_str().unwrap_or_default();
    let parts = long["parts"].as_array().map_or(0, Vec::len);
    let whole = text.contains("Introduction.") && text.contains("first words") && !text.contains("second words");
    smoke.check("a long skill reads as introduction, first part and the list", whole && parts == 3);
    smoke.check("and says it is shortened", text.contains("[Shortened at "));
    let same = smoke.json(&["read", "scroll"])?["text"].as_str().is_some_and(|text| !text.contains("[Shortened"));
    smoke.check("6,000 characters count as characters, not bytes", same);
    let wall = smoke.json(&["read", "wall"])?["text"].as_str().map_or(0, |text| text.chars().count());
    smoke.check("one long part is cut", (6000..6300).contains(&wall));
    let rest = smoke.json(&["read", "wall", "--from", "6000"])?;
    let rest = rest["text"].as_str().unwrap_or_default();
    smoke.check("--from reads the rest", rest.chars().count() == 5999 && !rest.contains("[Shortened"));
    let beyond = smoke.run(&["read", "wall", "--from", "12000"])?;
    smoke.check("--from past the end is an error", !beyond.status.success());
    let cut = text.split("from ").last().and_then(|from| from.strip_suffix(']')).unwrap_or("none");
    let on = smoke.json(&["read", "long", "--from", cut])?;
    let on = on["text"].as_str().is_some_and(|text| text.trim_start().starts_with("## Second"));
    smoke.check("a shortened skill says where to read on", on);
    for (wanted, words) in [("quill", "see words"), ("Quill++", "plus words")] {
        let part = smoke.json(&["read", "scroll", "--part", wanted])?;
        let text = part["text"].as_str().unwrap_or_default();
        let only = text.contains(words) && text.matches("words").count() == 1;
        smoke.check(&format!("--part {wanted} is that part alone"), only);
    }
    let start = smoke.json(&["read", "kitchen", "--part", "Swee..."])?;
    smoke.check("the start of a heading is enough", start["part"] == "Sweets");
    let many = smoke.run(&["read", "ladder", "--part", "Rung"])?;
    smoke.check("but not of several", !many.status.success());
    let empty = smoke.run(&["read", "scroll", "--part", ""])?;
    smoke.check("an empty part is no part", !empty.status.success());
    let part = smoke.json(&["read", "long", "--part", "second"])?;
    let text = part["text"].as_str().unwrap_or_default();
    smoke.check("a part reads alone", text.contains("second words") && !text.contains("first words"));
    smoke.check("a heading in a code fence is no part", text.contains("## not a part"));
    let files = long["files"].as_array().cloned().unwrap_or_default();
    smoke.check("files beside it are listed, never inlined", files == ["scripts/run.py"]);
    let missing = smoke.run(&["read", "long", "--part", "nope"])?;
    let text = String::from_utf8_lossy(&missing.stdout).into_owned();
    smoke.check("an unknown part lists the parts", !missing.status.success() && text.contains("First, Second, Third"));
    let within = smoke.json(&["search", "--in", "long", "third"])?;
    smoke.check("--in searches one skill", names(&within) == ["long"] && within[0]["parts"][0] == "Third");
    Ok(())
}

fn changes(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let folder = skill(own, "draft", "A pelican", "magpies")?;
    smoke.check("a new skill is listed", smoke.described("draft", "A pelican")?);
    skill(own, "draft", "A penguin", "buzzard")?;
    smoke.check("an edit of the same length replaces the old text", smoke.described("draft", "A penguin")?);
    let fresh = smoke.first(&["buzzard"])? == "draft" && smoke.first(&["magpies"])?.is_empty();
    smoke.check("in the search too", fresh);
    let bytes = read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    smoke.check("and the old words leave the index", !bytes.windows(5).any(|part| part == b"magpi"));
    rename(&folder, own.join("moved")).map_err(|error| error.to_string())?;
    let moved = smoke.listed("draft")?.and_then(|skill| skill["path"].as_str().map(|path| path.contains("/moved/")));
    smoke.check("a moved folder is followed", moved == Some(true));
    remove_dir_all(own.join("moved")).map_err(|error| error.to_string())?;
    let gone = smoke.listed("draft")?.is_none() && smoke.first(&["buzzard"])?.is_empty();
    smoke.check("a deleted skill is gone", gone);
    put(&own.join("broken/SKILL.md"), "---\nname: [\n---\ntoucan\n")?;
    let broken = smoke.problem("no description")? && smoke.first(&["toucan"])? == "broken";
    smoke.check("broken front matter is reported, and still found", broken);
    put(&own.join("huge/SKILL.md"), &"cassowary ".repeat(30_000))?;
    let huge = smoke.problem("larger than 256 KB")? && smoke.first(&["cassowary"])?.is_empty();
    smoke.check("a huge SKILL.md is reported, and skipped", huge);
    let why = String::from_utf8_lossy(&smoke.run(&["read", "huge"])?.stdout).into_owned();
    smoke.check("read says why it skips it", why.contains("larger than 256 KB"));
    skill(&smoke.home.join(".agents/skills"), "broken", "Works", "")?;
    smoke.check("a broken skill takes no name from a good one", smoke.described("broken", "Works")?);
    smoke.check("and is not found where it can't be read", smoke.first(&["toucan"])?.is_empty());
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
    let large = smoke.home.join(".agents/skills/large");
    put(&large.join("SKILL.md"), &"emu ".repeat(80_000))?;
    link(&large, &smoke.home.join(".claude/skills/large"))?;
    let listed = smoke.json(&["list"])?;
    let large = listed.as_array().into_iter().flatten().filter(|skill| skill["name"] == "large").count();
    smoke.check("a skill linked from two folders is listed once", shared == 1 && large == 1);
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
    let nosy = skill(&project, "nosy", "Nosy", "macaw")?;
    link(&secret, &nosy.join("where.txt"))?;
    link(&secret, &nosy.join("asks.txt"))?;
    link(&secret, &nosy.join("notes.md"))?;
    let tooled = skill(own, "tooled", "Tooled", "")?;
    link(&secret, &tooled.join("notes.md"))?;
    let theirs = smoke.json(&["read", "nosy"])?["files"] == json!([]);
    let mine = smoke.json(&["read", "tooled"])?["files"] == json!(["notes.md"]);
    smoke.check("linked files are listed in your own skill, not in a project's", theirs && mine);
    let hits = smoke.json(&["search", "macaw"])?;
    let unread = names(&hits) == ["nosy"] && smoke.first(&["condor"])?.is_empty();
    smoke.check("a file beside it that links elsewhere is not read", unread && !hits.to_string().contains("condor"));
    skill(&smoke.project.join("kept"), "kept", "Inside", "")?;
    for dir in [".claude", ".eukhe"] {
        create_dir_all(smoke.project.join(dir)).map_err(|error| error.to_string())?;
    }
    link(&smoke.project.join("kept"), &smoke.project.join(".eukhe/skills"))?;
    link(&outside, &smoke.project.join(".claude/skills"))?;
    smoke.check("a project may link a skills folder inside it", smoke.listed("kept")?.is_some());
    smoke.check("but not one outside it", smoke.listed("linked")?.is_none());
    skill(&smoke.project.join("kept"), "cuckoo", "Cuckoo", "")?;
    let indexed = smoke.listed("cuckoo")?.is_some();
    let aside = smoke.project.join("aside");
    rename(smoke.project.join(".agents"), &aside).map_err(|error| error.to_string())?;
    remove_dir_all(smoke.project.join("kept")).map_err(|error| error.to_string())?;
    skill(&outside.join("nest"), "cuckoo", "Cuckoo", "")?;
    link(&outside.join("nest"), &smoke.project.join("kept"))?;
    let bytes = || read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string());
    let left = smoke.listed("cuckoo")?.is_none() && !bytes()?.windows(6).any(|part| part == b"Cuckoo");
    smoke.check("a project's only folder that becomes a link out leaves the index", indexed && left);
    rename(&aside, smoke.project.join(".agents")).map_err(|error| error.to_string())?;
    link(&outside.join("linked"), &project.join("linked-too"))?;
    smoke.check("a project may not link a skill in", smoke.listed("linked")?.is_none());
    let mode = std::fs::metadata(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let mode = std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777;
    smoke.check("the index is private", mode == 0o600);
    Ok(())
}

fn trust(smoke: &mut Smoke) -> Result<(), String> {
    skill(&smoke.project.join(".agents/skills"), "local-build", "Build this project", "kookaburra")?;
    smoke.check("an untrusted project adds nothing", smoke.listed("local-build")?.is_none());
    let state = serde_json::json!({ "version": 1, "projects": [], "trusted": [smoke.project] });
    put(&smoke.data.join("state.json"), &state.to_string())?;
    let found = smoke.listed("local-build")?.is_some_and(|skill| skill["source"] == "project");
    smoke.check("a trusted project's skill is listed", found && smoke.first(&["kookaburra"])? == "local-build");
    let home = smoke.home.to_string_lossy().into_owned();
    let elsewhere = smoke.json(&["list", "--project", &home])?;
    let hits = smoke.json(&["search", "--project", &home, "kookaburra"])?;
    let only = !elsewhere.to_string().contains("local-build") && names(&hits).is_empty();
    smoke.check("and only in that project", only);
    create_dir_all(smoke.project.join(".git")).map_err(|error| error.to_string())?;
    create_dir_all(smoke.project.join("src")).map_err(|error| error.to_string())?;
    for dir in [".", "src"] {
        let found = smoke.json(&["list", "--project", dir])?.to_string().contains("local-build");
        smoke.check(&format!("it is listed from --project {dir}"), found);
    }
    let own = skill(&smoke.project.join(".agents/skills"), "own-git", "Its own checkout", "wallaby")?;
    create_dir_all(own.join(".git")).map_err(|error| error.to_string())?;
    let found = smoke.first(&["wallaby"])? == "own-git";
    put(&own.join("SKILL.md"), &"x".repeat(300_000))?;
    let left = smoke.first(&["wallaby"])?.is_empty();
    let bytes = read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let left = left && !bytes.windows(7).any(|part| part == b"wallaby");
    smoke.check("a skill that is its own checkout leaves the index too", found && left);
    put(&smoke.data.join("state.json"), "{")?;
    let unread = smoke.run(&["list"])?;
    smoke.check("state that cannot be read is an error", !unread.status.success());
    put(&smoke.data.join("state.json"), r#"{"version":1,"projects":[],"trusted":[]}"#)?;
    smoke.check("without trust it is gone", smoke.listed("local-build")?.is_none());
    let bytes = read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let kept = bytes.windows(10).any(|part| part == b"kookaburra" || part == b"Build this");
    smoke.check("and its text leaves the index", !kept);
    let nested = smoke.project.join("nested");
    skill(&nested.join(".agents/skills"), "magpie", "Magpie", "")?;
    create_dir_all(nested.join(".git")).map_err(|error| error.to_string())?;
    let both = serde_json::json!({ "version": 1, "projects": [], "trusted": [smoke.project, nested] });
    put(&smoke.data.join("state.json"), &both.to_string())?;
    let listed = smoke.json(&["list", "--project", "nested"])?.to_string().contains("magpie");
    let outer = smoke.listed("local-build")?.is_some() && smoke.listed("magpie")?.is_none();
    let bytes = read(smoke.data.join("skills.sqlite")).map_err(|error| error.to_string())?;
    let kept = bytes.windows(6).any(|part| part == b"Magpie");
    smoke.check("a trusted checkout inside it keeps its own rows", listed && outer && kept);
    remove_dir_all(&nested).map_err(|error| error.to_string())?;
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

fn save(smoke: &Smoke, text: &str) -> Result<(bool, String), String> {
    let reply = smoke.home.join("reply.md");
    put(&reply, text)?;
    let output = smoke.run(&["create", "--save", &reply.to_string_lossy()])?;
    let shown = [output.stdout, output.stderr].concat();
    Ok((output.status.success(), String::from_utf8_lossy(&shown).into_owned()))
}

fn block(name: &str, body: &str) -> String {
    format!("Done.\n\n````skill\n---\nname: {name}\ndescription: Grow ferns\n---\n{body}\n````\n")
}

fn creating(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let reply = smoke.home.join("reply.md");
    let asked = smoke.run(&["create"])?.stdout;
    smoke.check("create says what to reply with", String::from_utf8_lossy(&asked).contains("````skill"));
    smoke.check("and in JSON", smoke.json(&["create"])?["ask"].as_str().is_some_and(|ask| ask.contains("````skill")));
    let body = "## Water\n```\nmist 2\n```\n~~~~\n```\n~~~~\n## Light\nshade";
    let extras =
        "```asks\n1. my frond went brown\n```\n````where\n- `ferns.test`: the shop\n\n````\n````file scripts/mist.py\nprint(2)\n\tmore\n````\n````file scripts/__init__.py\n````\n";
    put(&reply, &format!("{}{extras}", block("Fern Care", body)))?;
    let shown = smoke.json(&["create", &reply.to_string_lossy()])?;
    let plain = smoke.run(&["create", &reply.to_string_lossy()])?.stdout;
    let plain = String::from_utf8_lossy(&plain).into_owned();
    smoke.check(
        "create alone shows every line",
        plain.contains("  shade\n") && plain.contains("  print(2)\n      more\n"),
    );
    let unsaved = shown["folder"] == "fern-care" && shown["saved"].is_null() && !own.join("fern-care").exists();
    smoke.check("create alone saves nothing", unsaved);
    let (saved, _) = save(smoke, &format!("{}{extras}", block("Fern Care", body)))?;
    let text =
        |name: &str| read(own.join("fern-care").join(name)).map(|text| String::from_utf8_lossy(&text).into_owned());
    let kept = text("SKILL.md").is_ok_and(|text| text.contains("## Light\nshade"))
        && text("asks.txt").is_ok_and(|text| text == "my frond went brown\n")
        && text("scripts/mist.py").is_ok_and(|text| text == "print(2)\n\tmore\n")
        && text("scripts/__init__.py").is_ok_and(|text| text.is_empty());
    smoke.check("a saved skill holds its text, phrasings and files", saved && kept);
    let found = smoke.first(&["why is the frond brown"])? == "Fern Care"
        && smoke.first(&["--for", "ferns.test"])? == "Fern Care";
    smoke.check("and is found at once", found);
    put(&own.join("fern-care/names.txt"), "frond keeper\n")?;
    let (again, shown) = save(smoke, &block("fern care", "## Soil\npeat\n```bash v1\nls\n"))?;
    smoke.check("what it replaces is named", shown.contains("is now in") && shown.contains("fern-care-1"));
    smoke.check("its other names stay", text("names.txt").is_ok_and(|text| text == "frond keeper\n"));
    let one = smoke.first(&["grow ferns"])? == "fern care" && !smoke.json(&["list"])?.to_string().contains("Fern Care");
    let old = own.join(".replaced/fern-care-1/asks.txt").exists() && !own.join("fern-care/asks.txt").exists();
    smoke.check("the same name replaces it, and the old one is kept aside", again && one && old);
    refusing(smoke, own)?;
    sharing(smoke, own)
}

fn sharing(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let big = format!("---\nname: Release\ndescription: Release it\n---\n{}", "word ".repeat(60_000));
    put(&own.join("plum/SKILL.md"), &big)?;
    let (beside, shown) = save(smoke, &block("plum", "jam"))?;
    let alone = read(own.join("plum/SKILL.md")).is_ok_and(|text| text.len() == big.len());
    smoke.check("a skill too large to read is not replaced by its folder's name", beside && alone);
    smoke.check("a new name hides nothing", own.join("plum-2/SKILL.md").exists() && !shown.contains("instead of"));
    skill(&smoke.home.join(".claude/skills"), "sewing", "Sew a hem", "theirs")?;
    let (hidden, shown) = save(smoke, &block("Sewing", "yours"))?;
    let path = smoke.home.join(".claude/skills/sewing/SKILL.md");
    let named = shown.contains("instead of") && shown.contains(&*path.to_string_lossy());
    smoke.check("the skill a new one hides is named", hidden && named);
    let (_, shown) = save(smoke, &block("sewing", "yours again"))?;
    smoke.check("and only the first time", !shown.contains("instead of") && shown.contains("is now in"));
    for dir in [own.join("plum"), smoke.home.join(".claude/skills/sewing")] {
        remove_dir_all(dir).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn refusing(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let x = block("x", "");
    for (what, reply, error) in [
        ("no description", x.replace("description: Grow ferns\n", ""), "a name and a description"),
        ("no name", block("--- ", ""), "a name and a description"),
        ("two skills", format!("{x}{x}"), "one skill block"),
        ("no skill", "Done.".to_string(), "one skill block"),
        ("too much text", block("x", &"word ".repeat(14_000)), "at most"),
        ("three backticks", x.replace("````", "```"), "needs four backticks"),
        ("a file in three backticks", format!("{x}```file a.py\nx\n```\n"), "needs four backticks"),
        ("a reply cut short", format!("{x}````file a.py\nx\n"), "never ends"),
        (
            "a skill its own code block ends",
            x.replace("\n````\n", "\n````sh\nls\n````\nmore\n````\n````asks\ny\n````\n"),
            "no label",
        ),
        ("a label near a real one", format!("{x}````Asks:\nx\n````\n"), "labeled"),
    ] {
        let (saved, shown) = save(smoke, &reply)?;
        smoke.check(&format!("{what} is refused"), !saved && shown.contains(error));
    }
    let (loose, shown) = save(smoke, &format!("{x}```a `b` ```\n````toml\nx\n````\n"))?;
    smoke.check("a block it has no place for is named", loose && shown.contains("left out: a block labeled \"toml\""));
    for path in ["../x", "/tmp/x", "a/../../x", ".git/config", "a/.env", "skill.md", "asks.txt", "a\\x", "c:x", ""] {
        let (saved, shown) = save(smoke, &format!("{x}````file {path}\nx\n````\n"))?;
        smoke.check(&format!("the file \"{path}\" is refused"), !saved && shown.contains("cannot hold"));
    }
    let (odd, _) = save(smoke, &block("../../Over There/é", ""))?;
    smoke.check("a name is a plain folder", odd && own.join("over-there-é/SKILL.md").exists());
    let made_up = ["plum", "jam9"].concat();
    let secrets = [
        format!("password: {made_up}\npassword: <yours>\nAsk for the password."),
        format!("token token_id={made_up}\nx-api-key: {made_up}"),
        format!("**Password:** {made_up}\ntool --token {made_up}"),
    ];
    let (marked, shown) = save(smoke, &block("keys", &secrets.join("\n")))?;
    smoke.check("secrets are marked", marked && shown.matches("may be private").count() == 5);
    let marks = save(smoke, &block("mark", "first"))?.0 && save(smoke, &block("mark++", "second"))?.0;
    let both = |one: &str, other: &str| {
        let holds = |folder: &str, text: &str| {
            read(own.join(folder).join("SKILL.md")).is_ok_and(|all| String::from_utf8_lossy(&all).contains(text))
        };
        holds("mark", one) && holds("mark-2", other)
    };
    smoke.check("a name with the same folder keeps the other skill", marks && both("first", "second"));
    let again = save(smoke, &block("mark++", "third"))?.0;
    smoke.check("and replaces only its own", again && both("first", "third"));
    skill(&own.join("group"), "inner", "In a group", "")?;
    let (inner, shown) = save(smoke, &block("inner", ""))?;
    smoke.check("a name in use deeper is refused", !inner && shown.contains("already has this name"));
    let group = save(smoke, &block("group", ""))?.0 && own.join("group/inner/SKILL.md").exists();
    smoke.check("a group's folder is left alone", group && own.join("group-2/SKILL.md").exists());
    let files = std::fs::read_dir(own).map_err(|error| error.to_string())?;
    let making = files.flatten().any(|file| file.file_name().to_string_lossy().starts_with(".making"));
    smoke.check("nothing lands elsewhere", !smoke.home.join("x").exists() && !making);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| path.metadata().map_or(0, |meta| meta.permissions().mode() & 0o777);
        let folder = own.join("fern-care");
        smoke.check("a saved skill is private", mode(&folder) == 0o700 && mode(&folder.join("SKILL.md")) == 0o600);
        let set =
            |mode| std::fs::set_permissions(own, PermissionsExt::from_mode(mode)).map_err(|error| error.to_string());
        set(0o500)?;
        let (stuck, _) = save(smoke, &block("fern care", "## New\nnew"))?;
        set(0o700)?;
        smoke.check(
            "a folder that can't be written keeps the old skill",
            !stuck && smoke.first(&["peat"])? == "fern care",
        );
        let stuck = own.join("fern-z");
        put(&stuck.join("SKILL.md"), "---\nname: fern care\ndescription: Same name\n---\n")?;
        let set =
            |mode| std::fs::set_permissions(&stuck, PermissionsExt::from_mode(mode)).map_err(|error| error.to_string());
        set(0o500)?;
        let (half, _) = save(smoke, &block("fern care", "## New\nnew"))?;
        set(0o700)?;
        let back = own.join("fern-care/SKILL.md").exists() && smoke.first(&["peat"])? == "fern care";
        smoke.check("a replace that fails half way puts the old skill back", !half && back);
    }
    remove_dir_all(own.join("group")).map_err(|error| error.to_string())
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
        let first = || smoke.command(&["search", "marmalade"]).stdout(Stdio::piped()).spawn();
        let searches: Vec<_> = (0..6).map(|_| first()).collect();
        for child in searches {
            let output = child.and_then(Child::wait_with_output).map_err(|error| error.to_string())?;
            good += usize::from(String::from_utf8_lossy(&output.stdout).starts_with("kitchen "));
        }
    }
    smoke.check(
        "six first searches at once all answer, five times with no index and five with a damaged one",
        good == 60,
    );
    let (mut mixed, agents) = (0, [smoke.home.join("agent-a"), smoke.home.join("agent-b")]);
    for agent in &agents {
        skill(&agent.join("skills"), "each", &format!("Mixed {}", agent.display()), "")?;
    }
    for _ in 0..10 {
        let start = |agent| {
            smoke.command(&["search", "mixed"]).env("EUKHE_CODING_AGENT_DIR", agent).stdout(Stdio::piped()).spawn()
        };
        let lists: Vec<_> = agents.iter().cycle().take(6).map(|agent| (agent, start(agent))).collect();
        for (agent, child) in lists {
            let output = child.and_then(Child::wait_with_output).map_err(|error| error.to_string())?;
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            mixed += usize::from(text.contains(&*agent.to_string_lossy()));
        }
    }
    smoke.check("lists with other folders at the same time keep their own skills", mixed == 60);
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
    smoke.check("and answers again", smoke.first(&["marmalade"])? == "kitchen");
    Ok(())
}

fn served(smoke: &mut Smoke, own: &Path) -> Result<(), String> {
    let mut child = smoke
        .command(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let (Some(mut to), Some(from)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("the skills server has no pipes".to_string());
    };
    let (lines, answers) = std::sync::mpsc::channel();
    std::thread::spawn(move || BufReader::new(from).lines().try_for_each(|line| lines.send(line)));
    let mut ask = |id: u32, method: &str, params: Value| -> Result<Value, String> {
        writeln!(to, "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .map_err(|error| error.to_string())?;
        let line = answers.recv_timeout(Duration::from_secs(20)).map_err(|error| format!("{method}: {error}"))?;
        let line = line.map_err(|error| error.to_string())?;
        if id == 1 {
            writeln!(to, "{}", json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .map_err(|error| error.to_string())?;
        }
        let answer: Value = serde_json::from_str(&line).map_err(|error| format!("{method}: {error}: {line}"))?;
        let right = answer["id"] == id;
        right.then(|| answer["result"].clone()).ok_or_else(|| format!("{method}: {answer}"))
    };
    let client = json!({ "name": "smoke", "version": "1" });
    let opened =
        ask(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": client }))?;
    let mut call = |id: u32, tool: &str, given: Value| -> Result<(bool, String), String> {
        let answer = ask(id, "tools/call", json!({ "name": tool, "arguments": given }))?;
        let failed = answer["isError"] == true;
        Ok((failed, answer["content"][0]["text"].as_str().unwrap_or_default().to_string()))
    };
    let found = call(2, "skill_search", json!({ "words": "marmalade" }))?;
    let placed = call(3, "skill_search", json!({ "words": "marmalade jam video", "for": "https://tubes.test/" }))?;
    let nothing = call(4, "skill_search", json!({ "words": "what is the" }))?;
    let part = call(5, "skill_read", json!({ "name": "long", "part": "Third" }))?;
    let unknown = call(6, "skill_read", json!({ "name": "long-one" }))?;
    let missing = call(7, "skill_read", json!({}))?;
    let added = skill(own, "kites", "Fly a kite", "Run into the wind.")?;
    let late = call(8, "skill_search", json!({ "words": "kite" }))?;
    remove_dir_all(added).map_err(|error| error.to_string())?;
    let wall = call(9, "skill_read", json!({ "name": "wall", "from": 6000 }))?;
    let wrong = call(10, "skill_search", json!({ "query": "marmalade" }))?;
    let within = call(11, "skill_search", json!({ "words": "third", "in": "long" }))?;
    let stuck = call(12, "skill_search", json!({ "words": "marmalade", "in": "long" }))?;
    let state = smoke.data.join("state.json");
    let trusted = std::fs::read_to_string(&state).map_err(|error| error.to_string())?;
    let held = call(13, "skill_search", json!({ "words": "kookaburra" }))?;
    put(&state, r#"{"version":1,"projects":[],"trusted":[]}"#)?;
    let revoked = call(14, "skill_search", json!({ "words": "kookaburra" }))?;
    put(&state, &trusted)?;
    write(smoke.data.join("skills.sqlite"), [0x5a; 8192]).map_err(|error| error.to_string())?;
    let rebuilt = call(15, "skill_search", json!({ "words": "marmalade" }))?;
    let tools = ask(16, "tools/list", json!({}))?;
    drop(to);
    let ended = child.wait().map_err(|error| error.to_string())?.success();
    let tools: Vec<(&str, &Value)> = tools["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| (tool["name"].as_str().unwrap_or_default(), &tool["annotations"]["readOnlyHint"]))
        .collect();
    let right = tools == [("skill_search", &json!(true)), ("skill_read", &json!(true))];
    smoke.check("the server offers two read-only tools and says when to use them", {
        right && opened["instructions"].as_str().is_some_and(|text| text.contains("skill_search"))
    });
    let closest = !found.0 && found.1.starts_with("kitchen (yours)") && found.1.contains("not always a fit");
    smoke.check("the server searches and says the list may not fit", closest);
    smoke.check("the server puts a site's skill first there", placed.1.starts_with("tubes "));
    smoke.check("the server says when nothing is found", !nothing.0 && nothing.1.starts_with("nothing found"));
    smoke.check("the server reads one part", !part.0 && part.1.starts_with("## Third\n"));
    smoke.check("the server fails an unknown name, naming a near one", {
        unknown.0 && unknown.1.contains("near it: long")
    });
    smoke.check("the server fails a call with no name", missing.0);
    smoke.check("a skill added while it runs is found at once", late.1.starts_with("kites "));
    smoke.check("the server reads on from a number", !wall.0 && wall.1.starts_with("brick ") && wall.1.len() > 5000);
    smoke.check("a search with no words is an error that names them", wrong.0 && wrong.1.contains("\"words\""));
    smoke.check("a search in one skill offers its part", within.1.contains("Third") && within.1.contains("\"part\""));
    smoke.check("and does not send the model away from it", stuck.1.starts_with("no part of long"));
    smoke.check(
        "trust taken back while it runs counts at once",
        held.1.starts_with("local-build ") && { revoked.1.starts_with("nothing found") },
    );
    smoke.check("an index damaged while it runs is made again", rebuilt.1.starts_with("kitchen "));
    smoke.check("the server ends with its input", ended);
    Ok(())
}

fn many(smoke: &mut Smoke) -> Result<(), String> {
    let personal = smoke.home.join(".agents/skills");
    let filler = "Open the panel, pick the row, check the total, save the sheet and close it.\n".repeat(60);
    for n in 0..MANY {
        let body = format!("## Steps {n}\n{filler}## Notes\nitem{n} {filler}");
        skill(&personal, &format!("filler-{n}"), &format!("Handle sheet {n} of the ledger"), &body)?;
    }
    let timed = |smoke: &Smoke, words: &str| -> Result<(Duration, String), String> {
        let start = Instant::now();
        let found = smoke.first(&[words])?;
        Ok((start.elapsed(), found))
    };
    let (first, found) = timed(smoke, "marmalade")?;
    let (again, also) = timed(smoke, "item977 of the ledger")?;
    println!("     {MANY} skills: first search {first:?}, next {again:?}");
    smoke.check("a thousand more skills leave the answer alone", found == "kitchen" && also == "filler-977");
    smoke.check("the first index takes under 5 s", first < Duration::from_secs(5));
    smoke.check("the next start reads the index", again < Duration::from_millis(500));
    Ok(())
}

struct Quiet;

impl Waker for Quiet {
    fn wake(&self) {}
}

struct At {
    studio: Arc<Studio>,
    project: PathBuf,
    asked: usize,
}

impl At {
    fn ask(&mut self, project: Option<String>, query: &str, limit: u32) -> String {
        self.asked += 1;
        let asked = format!("at-{}", self.asked);
        self.studio.mentions(asked.clone(), project, query.to_string(), limit);
        asked
    }

    fn once(&mut self, project: Option<String>, query: &str, limit: u32) -> Result<Vec<Mention>, String> {
        let asked = self.ask(project, query, limit);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            for update in self.studio.tick() {
                match update {
                    Update::Mentions { request, items } if request == asked => return Ok(items),
                    Update::RequestFailed { request, message } if request == asked => return Err(message),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(format!("no answer for @{query}"))
    }

    fn list(&mut self, query: &str, holds: &str) -> Result<Vec<Mention>, String> {
        let project = self.project.to_string_lossy().into_owned();
        self.list_in(Some(&project), query, 0, holds)
    }

    fn list_in(&mut self, project: Option<&str>, query: &str, limit: u32, holds: &str) -> Result<Vec<Mention>, String> {
        let mut items = Vec::new();
        for _ in 0..20 {
            items = self.once(project.map(String::from), query, limit)?;
            if titles(&items).contains(&holds) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(items)
    }
}

fn titles(items: &[Mention]) -> Vec<&str> {
    items.iter().map(|item| item.title.as_str()).collect()
}

fn files(items: &[Mention]) -> usize {
    items.iter().filter(|item| item.kind == MentionKind::File).count()
}

fn mentions(smoke: &mut Smoke) -> Result<(), String> {
    let (top, text) = (smoke.data.with_file_name("at"), |error: std::io::Error| error.to_string());
    let (data, project, own) = (top.join("data"), top.join("project"), top.join("data/skills"));
    create_dir_all(top.join("home")).map_err(text)?;
    skill(&own, "marmalade", "Cook oranges", "src main rs zebra")?;
    skill(&own, "smart-mart", "Shop", "")?;
    skill(&own, "outdoors", "Go outside", "Take a kite, or a mar.")?;
    put(&own.join("ledger/SKILL.md"), "Broken, with no front matter. mar")?;
    skill(&own, "marzipan", "Hidden twin", "")?;
    skill(&top.join("home/.claude/skills"), "marzipan", "The other twin", "")?;
    skill(&project.join(".agents/skills"), "marmot", "Dig", "")?;
    for name in ["src/main.rs", "marble.txt", "notes/smarts.txt"] {
        put(&project.join(name), "x")?;
    }
    let project = project.canonicalize().map_err(text)?;
    std::env::set_var("HOME", top.join("home"));
    std::env::set_var("EUKHE_CODING_AGENT_DIR", top.join("home/agent"));
    std::env::set_var("LOCAL_STUDIO_DATA_DIR", &data);
    std::env::set_var("EUKHE_DAEMON_SOCKET", top.join("d.sock"));
    let config = StudioConfig {
        socket_path: Some(top.join("d.sock").to_string_lossy().into_owned()),
        session_dir: None,
        data_dir: None,
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let studio = Studio::new(config, Arc::new(Quiet)).map_err(|error| error.to_string())?;
    let mut at = At { studio: Arc::clone(&studio), project, asked: 0 };
    let outcome = at_list(smoke, &mut at, &data);
    studio.shutdown();
    outcome
}

fn at_list(smoke: &mut Smoke, at: &mut At, data: &Path) -> Result<(), String> {
    let row = |kind, name: &str, detail: &str, badge: &str, install: &str| Mention {
        kind,
        title: format!("@{name}"),
        detail: detail.to_string(),
        badge: badge.to_string(),
        insert: format!("@{name} "),
        install: install.to_string(),
    };
    let items = at.list("Mar", "@marmalade")?;
    smoke.check(
        "@mar lists its skill first",
        items.first() == Some(&row(MentionKind::Skill, "marmalade", "Cook oranges", "yours", "")),
    );
    let order = ["@marmalade", "@marzipan", "@smart-mart", "@outdoors"];
    smoke.check("then names holding it, then skills found by words, hidden ones never", {
        titles(&items).get(..4) == Some(&order) && items[4..].iter().all(|item| item.kind == MentionKind::File)
    });
    smoke.check(
        "then files, as today's rows",
        items.contains(&row(MentionKind::File, "marble.txt", "", "", ""))
            && items.contains(&row(MentionKind::File, "notes/smarts.txt", "notes", "", "")),
    );
    smoke.check("an untrusted project's skill is not listed", !titles(&items).contains(&"@marmot"));
    let state = json!({ "version": 1, "projects": [], "trusted": [at.project] });
    put(&data.join("state.json"), &state.to_string())?;
    let items = at.list("mar", "@marmot")?;
    let trusted = row(MentionKind::Skill, "marmot", "Dig", "project", "");
    smoke.check("and a trusted project's is", items.contains(&trusted));
    let nowhere = at.list_in(None, "mar", 0, "@marmalade")?;
    let own = titles(&nowhere).contains(&"@marmalade") && !titles(&nowhere).contains(&"@marmot");
    smoke.check("with no project, skills are still listed, and no files", own && files(&nowhere) == 0);
    smoke.check("@kite finds a skill by its text", titles(&at.list("kite", "@outdoors")?) == ["@outdoors"]);
    for name in ["src/ma", "main.rs"] {
        let items = at.list(name, "@src/main.rs")?;
        smoke.check(&format!("@{name} lists files only"), !items.is_empty() && files(&items) == items.len());
    }
    let offered = at.list("gan", "@gantrel")?;
    let offer = offered.iter().find(|item| item.title == "@gantrel");
    let install = |item: &Mention| item.badge == "Install" && item.install == "gantrel" && item.insert == "@gantrel ";
    smoke.check("@gan offers the catalog's gantrel", offer.is_some_and(install));
    let empty = at.list("", "@marble.txt")?;
    let shown = titles(&empty).contains(&"@marble.txt");
    smoke.check("an empty @ lists files, and no plugin that isn't installed", shown && files(&empty) == empty.len());
    let manifest =
        "id = \"gantrel\"\nname = \"Crane\"\nversion = \"0.1.0\"\ndescription = \"Lift\"\ncapabilities = []\n";
    put(&data.join("plugins/gantrel/plugin.toml"), manifest)?;
    let plugin = row(MentionKind::Plugin, "gantrel", "Lift", "Plugin", "");
    for query in ["gan", "crane"] {
        let items = at.list(query, "@gantrel")?;
        smoke.check(&format!("installed, @{query} lists it as a plugin"), items == [plugin.clone()]);
    }
    for n in 0..40 {
        put(&at.project.join(format!("many/zebra-{n}.txt")), "x")?;
    }
    let empty = at.list("", "@gantrel")?;
    smoke.check(
        "an empty @ lists files, then installed plugins, in thirty rows",
        empty.len() == 30 && files(&empty[..29]) == 29 && empty.last() == Some(&plugin),
    );
    for n in 0..9 {
        skill(&data.join("skills"), &format!("zebra-{n}"), "Stripes", "")?;
        skill(&data.join("skills"), &format!("okapi-{n}"), "Stripes", "")?;
    }
    let items = at.list("zebra", "@zebra-0")?;
    smoke.check("six skills above files", items.len() == 30 && files(&items[6..]) == 24 && files(&items[..6]) == 0);
    let items = at.list("okapi", "@okapi-0")?;
    smoke.check("more when no file matches", items.len() == 9 && files(&items) == 0);
    at_slow(smoke, at, data, &plugin)?;
    let here = at.project.to_string_lossy().into_owned();
    let few = at.list_in(Some(&here), "zebra", 4, "@zebra-0")?;
    smoke.check("a short list still has files", few.len() == 4 && files(&few) == 2);
    let found = at.list("stripes", "@okapi-0")?;
    smoke.check("five skills found by their text", found.len() == 5 && files(&found) == 0);
    for n in 0..7 {
        skill(&data.join("skills"), &format!("gantrel-{n}"), "Crates", "")?;
    }
    put(&at.project.join("gantrel.txt"), "x")?;
    let items = at.list("gantrel", "@gantrel-0")?;
    smoke.check("the plugin of that exact name comes first", items.first() == Some(&plugin));
    let remote = at.once(Some("@desk/home".to_string()), "mar", 0);
    let refused = "Searching files works only in Local Studio on ";
    smoke.check("a project on another machine is refused", remote.is_err_and(|said| said.starts_with(refused)));
    let index = data.join("skills.sqlite");
    for name in ["skills.sqlite", "skills.sqlite-wal", "skills.sqlite-shm"] {
        let _ = std::fs::remove_file(data.join(name));
    }
    create_dir_all(index.join("in-the-way")).map_err(|error| error.to_string())?;
    let items = at.list("gantrel", "@gantrel")?;
    smoke.check("with no index, plugins and files are still listed", items.len() == 2 && items[0] == plugin);
    Ok(())
}

fn at_slow(smoke: &mut Smoke, at: &mut At, data: &Path, plugin: &Mention) -> Result<(), String> {
    let lock = std::fs::File::open(data.join("skills.lock")).map_err(|error| error.to_string())?;
    lock.lock().map_err(|error| error.to_string())?;
    let held = Instant::now();
    let here = at.project.to_string_lossy().into_owned();
    let items = at.once(Some(here.clone()), "zebra", 0)?;
    let (took, only) = (held.elapsed(), items.len() == 30 && files(&items) == 30);
    let slow = at.once(Some(here.clone()), "gan", 0)?;
    let empty = at.once(Some(here.clone()), "", 0)?;
    for n in 0..60 {
        at.ask(Some(here.clone()), &format!("zebra-{n}"), 0);
    }
    lock.unlock().map_err(|error| error.to_string())?;
    smoke.check("a store that takes too long leaves files alone", only && took < Duration::from_secs(3));
    smoke.check("and a plugin keeps its row", slow == [plugin.clone()] && empty.last() == Some(plugin));
    let last = at.once(Some(here.clone()), "zebra", 0)?;
    smoke.check("the last of many has its skills", titles(&last).contains(&"@zebra-0"));
    Ok(())
}

fn run(smoke: &mut Smoke) -> Result<(), String> {
    let own = smoke.data.join("skills");
    skill(&own, "kitchen", "Cook at home", "Jam.\n\n## Preserves\nmarmalade and jam\n\n## Sweets\ncrème brûlée")?;
    let bulbs = skill(&own, "bulbs", "Replace lighting", "Turn it off first. No old tricks.")?;
    put(&bulbs.join("asks.txt"), "why did the glow vanish from my lamp\n")?;
    skill(&own, "windy", &"windy ".repeat(100), "")?;
    skill(&own, "invoices", "开发票的步骤 PDF发票", "## 怎么开票\nSteps.")?;
    let rungs: Vec<String> = (0..9).map(|n| format!("## Rung {n}\nstep up the ladder\n")).collect();
    skill(&own, "ladder", "Climb", &rungs.concat())?;
    skill(&own, "prices", "ตรวจราคา", "")?;
    skill(&own, "mark", "One pen", "pen")?;
    skill(&own, "mark++", "Two pens", "pen")?;
    skill(&own, "quoting", "Show text", "````\n```\n## Example\ngargoyle\n```\n````")?;
    let sites = "- ```sh\n  make\n  ```\n## Sites\n- `notes.test`: the notes\n- localhost (the dev page)\n- [fd00::1]\n- not a site\n- [Pages](https://www.pages.test/a) and **more**\n- **bold.test**\n";
    skill(&own, "workbench", "Try things", sites)?;
    let beacon = skill(&own, "beacon", "Light", "beacon")?;
    put(&beacon.join("where.txt"), "Lamp\u{1b}[2J Pro\u{9b}\n")?;
    skill(&own, "colors", "\"Escape \\e[31mred\"", "escape")?;
    skill(&own, "tubes", "Watch clips", "## Sites\n- `tubes.test`\n\n## Captions\nvideo words")?;
    let painter = skill(&own, "painter", "Draw", "Brushes.")?;
    put(&painter.join("where.txt"), "Paint Pro\nDraw#\nInk/Wash\n")?;
    skill(&own, "forms", "Fill out", "https://docs.forms.test/a https://docs.forms.test/b fill")?;
    let learning = skill(&own, "site-learning", "Learn a site", "Try it.")?;
    put(&learning.join("names.txt"), "old-tricks\nzither-hunt\n")?;
    skill(&own, "twin", "A duplicate", "mine")?;
    skill(&smoke.home.join(".claude/skills"), "twin", "A duplicate", "theirs")?;
    let filler = "more words here\n".repeat(150);
    let body = format!(
        "Introduction.\n\n## First\nfirst words\n{filler}\n## Second\nsecond words\n```\n## not a part\n```\n{filler}\n## Third\nthird\n{filler}"
    );
    let long = skill(&own, "long", "A long one", &body)?;
    put(&long.join("scripts/run.py"), "print()")?;
    let scroll = format!("{}\n\n## Quill\nsee words\n\n## Quill++\nplus words\n", "山".repeat(3000));
    skill(&own, "scroll", "A scroll", &scroll)?;
    skill(&own, "wall", "A wall", &"brick ".repeat(2000))?;
    words(smoke)?;
    places(smoke)?;
    named(smoke)?;
    naming(smoke)?;
    reading(smoke)?;
    trust(smoke)?;
    plugins(smoke)?;
    changes(smoke, &own)?;
    creating(smoke, &own)?;
    #[cfg(unix)]
    links(smoke, &own)?;
    served(smoke, &own)?;
    index(smoke)?;
    many(smoke)?;
    mentions(smoke)
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
