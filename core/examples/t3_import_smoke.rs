use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use local_studio_core::{
    ChatWidth, Studio, StudioConfig, StudioError, T3Choices, T3ItemState, T3Report, Update, Waker,
};
use serde_json::{json, Value};

const CANARY: &str = "t3-smoke-canary-9f2c41d7e8b3";
const RUN_LIMIT: Duration = Duration::from_secs(40);
const WAIT_LIMIT: Duration = Duration::from_secs(25);

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("PASS t3_import_smoke");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL t3_import_smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}

struct Fixture {
    home: PathBuf,
    t3: PathBuf,
    personal: PathBuf,
    work: PathBuf,
    codex: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
}

fn fifo(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let status = Command::new("mkfifo").arg(path).status().context("run mkfifo")?;
    ensure!(status.success(), "mkfifo {} failed", path.display());
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?).with_context(|| format!("write {}", path.display()))
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn t3_settings(fixture: &Fixture) -> Value {
    json!({
        "providerInstances": {
            "claudeAgent": {
                "driver": "claudeAgent",
                "displayName": "personal",
                "enabled": true,
                "config": { "binaryPath": "/nowhere/claude", "homePath": "", "launchArgs": "" }
            },
            "claudeAgent_work": {
                "driver": "claudeAgent",
                "displayName": "work",
                "accentColor": "#aa5500",
                "enabled": true,
                "config": { "binaryPath": "/nowhere/claude", "homePath": fixture.work.to_string_lossy() }
            },
            "codex": {
                "driver": "codex",
                "accentColor": "#16a34a",
                "enabled": true,
                "config": { "binaryPath": "/nowhere/codex", "homePath": "", "shadowHomePath": "" }
            },
            "claudeAgent_off": {
                "driver": "claudeAgent",
                "displayName": "off",
                "enabled": false,
                "config": { "homePath": fixture.work.to_string_lossy() }
            },
            "cursor": {
                "driver": "cursor",
                "enabled": true,
                "config": { "binaryPath": "/nowhere/cursor-agent", "apiEndpoint": "", "apiKey": CANARY }
            },
            "opencode": {
                "driver": "opencode",
                "enabled": true,
                "config": { "binaryPath": "/nowhere/opencode", "serverUrl": "", "serverPassword": CANARY }
            }
        },
        "defaultModelSelection": {
            "instanceId": "claudeAgent",
            "model": "claude-opus-5-5",
            "options": [{ "id": "effort", "value": "high" }]
        },
        "textGenerationModelSelection": {
            "instanceId": "claudeAgent",
            "model": "claude-sonnet-5-5",
            "options": [{ "id": "effort", "value": "low" }]
        },
        "favorites": [
            { "provider": "claudeAgent", "model": "claude-opus-5-5" },
            { "provider": "codex", "model": "gpt-6.1-sol" },
            { "provider": "cursor", "model": "kimi-k3p1-pro" },
            { "provider": "claudeAgent_work", "model": "claude-haiku-5-5" }
        ],
        "providerModelPreferences": {
            "claudeAgent": { "hiddenModels": ["claude-sonnet-5-5", "claude-opus-4-8"], "modelOrder": [] }
        },
        "projectSettingsOverrides": {
            "p-alpha": {
                "defaultModelSelection": {
                    "instanceId": "codex",
                    "model": "gpt-6.1-sol",
                    "options": [{ "id": "reasoningEffort", "value": "medium" }]
                }
            }
        },
        "defaultTheme": "smoke-theme",
        "defaultAppearance": "dark",
        "addProjectBaseDirectory": fixture.home.join("code").to_string_lossy()
    })
}

fn t3_client(font: u32, width: &str) -> Value {
    json!({
        "fontSizeInterface": font,
        "chatWidth": width,
        "timestampFormat": "24-hour",
        "sendShortcut": "mod-enter",
        "confirmQuit": "double-click",
        "diffColorScheme": "blue-orange",
        "favorites": [{ "provider": "claudeAgent", "model": "claude-opus-5-5" }]
    })
}

fn build(scratch: &Path) -> Result<Fixture> {
    if scratch.exists() {
        fs::remove_dir_all(scratch).with_context(|| format!("clear {}", scratch.display()))?;
    }
    let home = scratch.join("home");
    let t3 = home.join(".t3").join("userdata");
    let personal = home.join(".claude");
    let work = home.join("accounts").join("claude-work");
    let codex = home.join(".codex");
    let alpha = scratch.join("projects").join("alpha");
    let beta = scratch.join("projects").join("beta");
    let gamma = scratch.join("projects").join("gamma");
    for dir in [&t3, &alpha, &beta, &gamma] {
        fs::create_dir_all(dir)?;
    }
    for account in [&personal, &work] {
        fifo(&account.join(".credentials.json"))?;
        fifo(&account.join(".claude.json"))?;
        fs::write(account.join("settings.json"), "{}\n")?;
    }
    fifo(&codex.join("auth.json"))?;
    fifo(&t3.join("secrets").join("provider-key"))?;
    fifo(&t3.join("providers").join("cursor.json"))?;
    fifo(&t3.join("cloud-auth-token.json"))?;
    let fixture = Fixture {
        home: fs::canonicalize(&home)?,
        t3: fs::canonicalize(&t3)?,
        personal: fs::canonicalize(&personal)?,
        work: fs::canonicalize(&work)?,
        codex: fs::canonicalize(&codex)?,
        alpha: fs::canonicalize(&alpha)?,
        beta: fs::canonicalize(&beta)?,
    };
    write_json(&fixture.t3.join("settings.json"), &t3_settings(&fixture))?;
    write_json(&fixture.t3.join("client-settings.json"), &t3_client(15, "wide"))?;
    write_json(
        &fixture.t3.join("keybindings.json"),
        &json!([
            { "key": "mod+shift+o", "command": "chat.new", "when": "!terminalFocus" },
            { "key": "mod+b", "command": "sidebar.toggle" },
            { "key": "mod+shift+d", "command": "composer.dictation", "when": "!terminalFocus" },
            { "key": "mod+alt+u", "command": "usage.open" }
        ]),
    )?;
    write_json(
        &fixture.t3.join("themes").join("smoke-theme.json"),
        &json!({
            "name": "Smoke Theme",
            "appearance": "dark",
            "colors": {
                "canvas": "#101010", "sidebar": "#141414", "surfaceRaised": "#181818",
                "text": "#eeeeee", "accent": "#d4af5d"
            },
            "css": ""
        }),
    )?;
    let database = rusqlite::Connection::open(fixture.t3.join("statev2.sqlite"))?;
    database.execute_batch(
        "CREATE TABLE projection_projects (project_id TEXT PRIMARY KEY, title TEXT NOT NULL, workspace_root TEXT NOT NULL, \
         scripts_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT, \
         default_model_selection_json TEXT, archived_at TEXT);
         CREATE TABLE auth_sessions (id TEXT, token TEXT);",
    )?;
    database.execute("INSERT INTO auth_sessions VALUES ('s', ?1)", [CANARY])?;
    let rows: [ProjectRow; 5] = [
        ("p-alpha", "Alpha App", fixture.alpha.to_string_lossy().into_owned(), "2026-11-20", None, None),
        ("p-beta", "beta", fixture.beta.to_string_lossy().into_owned(), "2026-11-19", None, None),
        ("p-gamma", "Gamma", gamma.to_string_lossy().into_owned(), "2026-11-18", None, Some("2026-11-01")),
        ("p-gone", "Gone", scratch.join("projects/gone").to_string_lossy().into_owned(), "2026-11-17", None, None),
        ("p-deleted", "Deleted", fixture.alpha.to_string_lossy().into_owned(), "2026-11-16", Some("2026-11-02"), None),
    ];
    for (id, title, path, updated, deleted, archived) in rows {
        database.execute(
            "INSERT INTO projection_projects VALUES (?1, ?2, ?3, '[]', '2026-01-01', ?4, ?5, NULL, ?6)",
            rusqlite::params![id, title, path, updated, deleted, archived],
        )?;
    }
    build_threads(&database, &fixture)?;
    drop(database);
    Ok(fixture)
}

struct Conversation {
    id: &'static str,
    driver: &'static str,
    instance: &'static str,
    access: &'static str,
    status: &'static str,
    payload: Value,
}

fn conversations(worktree: &str) -> Vec<Conversation> {
    let thread = |id, driver, instance, access, payload| Conversation {
        id,
        driver,
        instance,
        access,
        status: "completed",
        payload,
    };
    let claude = |id, payload| thread(id, "claudeAgent", "claudeAgent", "full-access", payload);
    vec![
        thread(
            "t-tree",
            "claudeAgent",
            "claudeAgent_work",
            "auto-accept-edits",
            json!({
                "worktreePath": worktree,
                "notes": CANARY,
                "modelSelection": {
                    "instanceId": "claudeAgent_work", "model": "claude-opus-5-5",
                    "options": [{ "id": "effort", "value": "high" }]
                }
            }),
        ),
        claude("t-settled", json!({ "settledAt": "2026-11-21T08:00:00.000Z" })),
        claude("t-archived", json!({})),
        thread(
            "t-codex",
            "codex",
            "codex",
            "approval-required",
            json!({
                "snoozedUntil": "2030-01-01T00:00:00.000Z",
                "modelSelection": {
                    "instanceId": "codex", "model": "gpt-6.1-sol",
                    "options": [{ "id": "reasoningEffort", "value": "medium" }]
                }
            }),
        ),
        Conversation { status: "running", ..claude("t-running", json!({})) },
        claude("t-gone", json!({})),
        thread("t-off", "claudeAgent", "claudeAgent_off", "full-access", json!({})),
        claude(
            "t-folder",
            json!({
                "worktreePath": format!("{worktree}-gone"),
                "modelSelection": { "instanceId": "codex", "model": "gpt-6.1-sol", "options": [] }
            }),
        ),
        claude("t-child", json!({ "lineage": { "parentThreadId": "t-tree" } })),
        claude("t-deleted", json!({})),
        thread("t-cursor", "cursor", "cursor", "full-access", json!({})),
        claude("t-never", json!({})),
    ]
}

fn build_threads(database: &rusqlite::Connection, fixture: &Fixture) -> Result<()> {
    database.execute_batch(
        "CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT PRIMARY KEY, project_id TEXT, title TEXT, \
         runtime_mode TEXT, active_provider_thread_id TEXT, updated_at TEXT, archived_at TEXT, deleted_at TEXT, \
         payload_json TEXT);
         CREATE TABLE orchestration_v2_projection_provider_threads (provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, \
         provider_session_id TEXT, payload_json TEXT, driver TEXT, provider_instance_id TEXT);
         CREATE TABLE orchestration_v2_projection_provider_sessions (provider_session_id TEXT PRIMARY KEY, \
         payload_json TEXT);
         CREATE TABLE orchestration_v2_projection_runs (thread_id TEXT, status TEXT, requested_at TEXT, completed_at TEXT);",
    )?;
    let tree = fixture.alpha.with_file_name("alpha-tree");
    fs::create_dir_all(&tree)?;
    let tree = path_text(&tree);
    for known in conversations(&tree) {
        let id = known.id;
        let archived = (id == "t-archived").then_some("2026-11-05T00:00:00.000Z");
        let deleted = (id == "t-deleted").then_some("2026-11-06T00:00:00.000Z");
        let active = (id != "t-never").then(|| format!("provider-{id}"));
        database.execute(
            "INSERT INTO orchestration_v2_projection_threads VALUES \
             (?1, 'p-alpha', ?1, ?2, ?3, '2026-11-20T00:00:00.000Z', ?4, ?5, ?6)",
            rusqlite::params![id, known.access, active, archived, deleted, known.payload.to_string()],
        )?;
        let native = json!({ "nativeThreadRef": { "nativeId": format!("native-{id}") } }).to_string();
        database.execute(
            "INSERT INTO orchestration_v2_projection_provider_threads VALUES (?1, ?2, 'shared', ?3, ?4, ?5)",
            rusqlite::params![format!("provider-{id}"), id, native, known.driver, known.instance],
        )?;
        let done = (known.status == "completed").then_some("2026-11-21T09:30:00.000Z");
        for (status, at, completed) in [
            ("completed", "2026-11-19T09:00:00.000Z", Some("2026-11-19T09:10:00.000Z")),
            (known.status, "2026-11-21T09:00:00.000Z", done),
        ] {
            database.execute(
                "INSERT INTO orchestration_v2_projection_runs VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, status, at, completed],
            )?;
        }
        let folder = match id {
            "t-tree" => tree.clone(),
            "t-folder" => format!("{tree}-gone"),
            _ => path_text(&fixture.alpha),
        };
        let escaped: String = folder.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let session = match (known.driver, id) {
            (_, "t-gone") | ("cursor", _) => continue,
            ("codex", _) => {
                fixture.codex.join(format!("sessions/2026/11/21/rollout-2026-11-21T09-00-00-native-{id}.jsonl"))
            }
            (_, "t-tree") => fixture.work.join("projects").join(escaped).join(format!("native-{id}.jsonl")),
            _ => fixture.personal.join("projects").join(escaped).join(format!("native-{id}.jsonl")),
        };
        fs::create_dir_all(session.parent().context("session folder")?)?;
        fs::write(session, "{}\n")?;
    }
    database.execute(
        "INSERT INTO orchestration_v2_projection_threads VALUES \
         ('t-broken', 'p-alpha', 't-broken', 'auto', 'provider-t-tree', '2026-11-20T00:00:00.000Z', NULL, NULL, '{broken')",
        [],
    )?;
    database.execute(
        "INSERT INTO orchestration_v2_projection_provider_sessions VALUES ('shared', ?1)",
        [json!({ "cwd": fixture.home }).to_string()],
    )?;
    Ok(())
}

fn threads_of(data: &Path) -> Result<BTreeMap<String, Value>> {
    let state = read_json(&data.join("state.json"))?;
    let threads = state["threads"].as_object().cloned().unwrap_or_default();
    Ok(threads.into_iter().collect())
}

fn thread_of(data: &Path, title: &str) -> Result<Option<Value>> {
    let mut found = threads_of(data)?.into_iter().filter(|(_, flags)| flags["title"] == title);
    let Some((id, flags)) = found.next() else { return Ok(None) };
    ensure!(found.next().is_none(), "{title} came over twice");
    ensure!(
        flags["harness"] == "codex" || flags["nativeSession"] == id.as_str(),
        "{title} continues session {} and not one of its own",
        flags["nativeSession"]
    );
    ensure!(flags["forkOf"] == format!("native-{title}"), "{title} is a copy of {}", flags["forkOf"]);
    Ok(Some(flags))
}

fn check_threads(fixture: &Fixture, data: &Path, report: &T3Report) -> Result<()> {
    let alpha = json!(path_text(&fixture.alpha));
    let requested = json!(1_795_251_600_000_i64);
    let expected = [
        (
            "t-tree",
            json!({
                "harness": "claudeCode", "account": path_text(&fixture.work), "model": "claude-opus-5-5",
                "thinkingLevel": "high", "access": "autoAcceptEdits", "project": alpha, "activityAt": requested,
                "cwd": path_text(&fixture.alpha.with_file_name("alpha-tree")),
            }),
        ),
        (
            "t-settled",
            json!({ "account": null, "cwd": alpha, "access": "fullAccess", "settledAt": 1_795_248_000_000_i64 }),
        ),
        ("t-archived", json!({ "archived": true, "cwd": alpha })),
        (
            "t-folder",
            json!({ "cwd": path_text(&fixture.alpha.with_file_name("alpha-tree-gone")), "model": null, "thinkingLevel": null }),
        ),
        (
            "t-codex",
            json!({
                "harness": "codex", "nativeSession": null, "cwd": alpha, "access": "supervised", "model": "gpt-6.1-sol",
                "thinkingLevel": "medium", "snoozedUntil": 1_893_456_000_000_i64,
            }),
        ),
    ];
    for (title, fields) in &expected {
        expect_state(report, &format!("thread:{title}"), T3ItemState::Import)?;
        let flags = thread_of(data, title)?.with_context(|| format!("{title} did not come over"))?;
        for (key, want) in fields.as_object().into_iter().flatten() {
            let found = flags.get(key).unwrap_or(&Value::Null);
            ensure!(found == want, "{title}: {key} is {found}, expected {want}");
        }
    }
    ensure!(threads_of(data)?.len() == expected.len(), "threads are {:?}", threads_of(data)?);
    for (title, words) in [
        ("t-running", "Running in T3 Code"),
        ("t-gone", "not on this machine"),
        ("t-cursor", "Not supported yet"),
        ("t-off", "turned off in T3 Code"),
    ] {
        expect_state(report, &format!("thread:{title}"), T3ItemState::Unsupported)?;
        let detail = item_detail(report, &format!("thread:{title}"));
        ensure!(detail.contains(words), "{title}: {detail}");
    }
    let folder = item_detail(report, "thread:t-folder");
    ensure!(folder.contains("Its folder is gone"), "t-folder: {folder}");
    for title in ["t-child", "t-deleted", "t-never", "t-broken"] {
        let found = state_of(report, &format!("thread:{title}"));
        ensure!(found.is_none(), "{title} is listed as {found:?}");
    }
    println!("threads: {} came over with their account, folder, model, access and shelf", expected.len());
    Ok(())
}

fn thread_stage(binary: &Path, fixture: &Fixture, data: &Path) -> Result<()> {
    edit(&data.join("state.json"), |state| {
        if let Some(threads) = state["threads"].as_object_mut() {
            threads.retain(|_, flags| flags["title"] != "t-settled");
        }
    })?;
    let database = rusqlite::Connection::open(fixture.t3.join("statev2.sqlite"))?;
    database.execute(
        "UPDATE orchestration_v2_projection_runs SET status = 'completed', completed_at = '2026-11-21T10:00:00.000Z' \
         WHERE thread_id = 't-running'",
        [],
    )?;
    drop(database);
    let next = report(&cli(binary, fixture, data, &["--json"])?)?;
    expect_state(&next, "thread:t-running", T3ItemState::Import)?;
    ensure!(thread_of(data, "t-running")?.is_some(), "a thread that stopped in T3 Code did not come over");
    expect_state(&next, "thread:t-settled", T3ItemState::Kept)?;
    ensure!(thread_of(data, "t-settled")?.is_none(), "a thread deleted in Local Studio came back");
    println!("threads: a stopped one came over, a deleted one stayed away");
    Ok(())
}

type ProjectRow<'a> = (&'a str, &'a str, String, &'a str, Option<&'a str>, Option<&'a str>);

fn tree(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let name = path.strip_prefix(root)?.to_string_lossy().into_owned();
            let kind = metadata.file_type();
            if kind.is_dir() {
                out.insert(name, format!("dir {:o}", metadata.mode()));
                pending.push(path);
            } else if kind.is_fifo() {
                out.insert(name, format!("fifo {} {}", metadata.mtime(), metadata.mtime_nsec()));
            } else {
                out.insert(name, format!("file {}", hex(&fs::read(&path)?)));
            }
        }
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let mut out = String::new();
    let _ = write!(out, "{hash:016x}:{}", bytes.len());
    out
}

fn cli(binary: &Path, fixture: &Fixture, data: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new(binary)
        .args(["import", "t3"])
        .args(args)
        .env("HOME", &fixture.home)
        .env("LOCAL_STUDIO_DATA_DIR", data)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("T3CODE_HOME")
        .env_remove("XDG_DATA_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {}", binary.display()))?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > RUN_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "`import t3 {}` did not finish in {} s: it opened a FIFO standing in for a secret",
                args.join(" "),
                RUN_LIMIT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)?;
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr)?;
    }
    let status = child.wait()?;
    ensure!(status.success(), "`import t3 {}` exited {status}: {stdout}{stderr}", args.join(" "));
    ensure!(!stdout.contains(CANARY) && !stderr.contains(CANARY), "the import printed a secret value");
    Ok(stdout)
}

fn report(text: &str) -> Result<T3Report> {
    serde_json::from_str(text).with_context(|| format!("parse the report: {text}"))
}

fn state_of(report: &T3Report, key: &str) -> Option<T3ItemState> {
    report.groups.iter().flat_map(|group| &group.items).find(|item| item.key == key).map(|item| item.state)
}

fn item_detail(report: &T3Report, key: &str) -> String {
    report
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .find(|item| item.key == key)
        .map(|item| item.detail.clone())
        .unwrap_or_default()
}

fn expect_state(report: &T3Report, key: &str, state: T3ItemState) -> Result<()> {
    let found = state_of(report, key);
    ensure!(found == Some(state), "{key}: expected {state:?}, got {found:?}");
    Ok(())
}

fn count(report: &T3Report, state: T3ItemState) -> usize {
    report.groups.iter().flat_map(|group| &group.items).filter(|item| item.state == state).count()
}

fn no_canary(root: &Path) -> Result<()> {
    for (name, digest) in tree(root)? {
        if digest.starts_with("file") {
            let bytes = fs::read(root.join(&name))?;
            ensure!(!String::from_utf8_lossy(&bytes).contains(CANARY), "{name} holds a secret value from T3 Code");
            for secret in [".credentials.json", ".claude.json", "auth.json", "cloud-auth-token.json"] {
                ensure!(!name.ends_with(secret), "{name} looks like a copied sign-in file");
            }
        }
    }
    Ok(())
}

fn theme_value() -> &'static str {
    if cfg!(target_os = "macos") {
        "smoke-theme"
    } else {
        "Smoke Theme"
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn check_first(fixture: &Fixture, data: &Path, first: &T3Report) -> Result<()> {
    ensure!(first.applied && first.enabled && first.offered, "the import did not turn sync on: {first:?}");
    let settings = read_json(&data.join("settings.json"))?;
    let accounts = &settings["harnessAccounts"];
    let claude: Vec<String> = serde_json::from_value(accounts["claude"].clone())?;
    let codex: Vec<String> = serde_json::from_value(accounts["codex"].clone())?;
    ensure!(
        claude == vec![path_text(&fixture.personal), path_text(&fixture.work)],
        "Claude Code accounts are {claude:?}, not the discovered ~/.claude then the T3 work dir"
    );
    ensure!(codex.is_empty(), "Codex accounts are {codex:?}, though discovery already finds ~/.codex");
    expect_state(first, "account:codex", T3ItemState::Import)?;
    let labels = accounts["labels"].as_array().context("no account labels")?;
    let label = |dir: &Path| labels.iter().find(|label| label["dir"] == path_text(dir)).cloned().unwrap_or(Value::Null);
    ensure!(label(&fixture.personal)["name"] == "personal", "the personal account is not named: {labels:?}");
    ensure!(
        label(&fixture.work)["name"] == "work" && label(&fixture.work)["accent"] == "#aa5500",
        "work label: {labels:?}"
    );
    ensure!(labels.iter().all(|label| label["source"] == "T3 Code"), "an imported account does not name its source");
    for (key, want) in [
        ("defaultProvider", json!("claude-code")),
        ("defaultModel", json!("claude-opus-5-5")),
        ("defaultThinkingLevel", json!("high")),
        ("textGenerationProvider", json!("claude-code")),
        ("textGenerationModel", json!("claude-sonnet-5-5")),
        ("colorScheme", json!("dark")),
        ("theme", json!(theme_value())),
        ("interfaceFontSize", json!(15)),
        ("chatWidth", json!("wide")),
        ("timeFormat", json!("twentyFourHour")),
        ("sendShortcut", json!("modEnter")),
        ("quitConfirmation", json!("doubleClick")),
        ("diffColors", json!("blueOrange")),
        ("addProjectBaseDirectory", json!(path_text(&fixture.home.join("code")))),
    ] {
        ensure!(settings[key] == want, "settings.{key} is {}, expected {want}", settings[key]);
    }
    let themes: Value = serde_json::from_str(settings["customThemes"].as_str().unwrap_or("[]"))?;
    let theme = themes.as_array().and_then(|themes| themes.first()).context("the T3 theme was not added")?;
    ensure!(
        theme["id"] == "smoke-theme" && theme["name"] == "Smoke Theme" && theme["canvas"] == "#101010",
        "theme {theme}"
    );
    ensure!(theme["colors"]["accent"] == "#d4af5d" && theme["card"] == "#181818", "theme colors {theme}");
    let keys = read_json(&data.join("keybindings.json"))?;
    ensure!(keys["chat.new"] == "cmd+shift+o", "chat.new is {}", keys["chat.new"]);
    ensure!(keys["usage.open"] == "cmd+option+u", "usage.open is {}", keys["usage.open"]);
    ensure!(keys.get("sidebar.toggle").is_none(), "a T3 binding equal to the default was written as an override");
    check_lists(fixture, data)?;
    check_threads(fixture, data, first)?;
    for (key, word) in
        [("account:cursor", "API key set in T3 Code"), ("account:opencode", "server password set in T3 Code")]
    {
        expect_state(first, key, T3ItemState::Unsupported)?;
        let detail = item_detail(first, key);
        ensure!(detail.contains("Not supported yet") && detail.contains(word), "{key}: {detail}");
    }
    expect_state(first, "project:p-gone", T3ItemState::Unsupported).or_else(|_| {
        let gone = first
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .any(|item| item.label == "Gone" && item.state == T3ItemState::Unsupported);
        ensure!(gone, "the missing project folder is not reported");
        Ok(())
    })?;
    expect_state(first, "keybinding:unknown", T3ItemState::Unsupported)?;
    expect_state(first, "hidden:unlisted", T3ItemState::Unsupported)?;
    for dir in [&fixture.personal, &fixture.work, &fixture.codex] {
        ensure!(
            !data.join(dir.file_name().unwrap_or_default()).exists(),
            "{} was copied into the data dir",
            dir.display()
        );
    }
    no_canary(data)
}

fn check_lists(fixture: &Fixture, data: &Path) -> Result<()> {
    let state = read_json(&data.join("state.json"))?;
    let projects: Vec<(String, String)> = state["projects"]
        .as_array()
        .context("no projects")?
        .iter()
        .map(|project| {
            (project["path"].as_str().unwrap_or("").to_string(), project["name"].as_str().unwrap_or("").to_string())
        })
        .collect();
    ensure!(
        projects
            == vec![
                (path_text(&fixture.alpha), "Alpha App".to_string()),
                (path_text(&fixture.beta), "beta".to_string())
            ],
        "projects are {projects:?}: only folders that exist and are not archived or deleted belong"
    );
    let keys_of = |name: &str| -> Vec<String> {
        state[name]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|key| {
                        format!("{}/{}", key["provider"].as_str().unwrap_or(""), key["model"].as_str().unwrap_or(""))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    ensure!(
        keys_of("favorites")
            == vec!["claude-code/claude-opus-5-5", "codex/gpt-6.1-sol", "claude-code/claude-haiku-5-5"],
        "favorites are {:?}",
        keys_of("favorites")
    );
    ensure!(keys_of("hidden") == vec!["claude-code/claude-sonnet-5-5"], "hidden are {:?}", keys_of("hidden"));
    let workspace = read_json(&data.join("project-workspace.json"))?;
    let alpha = &workspace["projects"][path_text(&fixture.alpha)]["settings"];
    ensure!(
        alpha["defaultProvider"] == "codex"
            && alpha["defaultModel"] == "gpt-6.1-sol"
            && alpha["defaultThinkingLevel"] == "medium",
        "the Alpha project default model is {alpha}"
    );
    Ok(())
}

fn files(data: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    ["settings.json", "keybindings.json", "state.json", "project-workspace.json"]
        .into_iter()
        .map(|name| Ok((name.to_string(), fs::read(data.join(name)).with_context(|| format!("read {name}"))?)))
        .collect()
}

fn edit(path: &Path, change: impl FnOnce(&mut Value)) -> Result<()> {
    let mut value = read_json(path)?;
    change(&mut value);
    write_json(path, &value)
}

fn cli_stages(binary: &Path, fixture: &Fixture, scratch: &Path) -> Result<()> {
    let data = scratch.join("data");
    let config_before = [&fixture.personal, &fixture.work, &fixture.codex].map(|dir| tree(dir));
    let t3_before = tree(&fixture.t3)?;

    let dry = report(&cli(binary, fixture, &data, &["--dry-run", "--json"])?)?;
    ensure!(dry.found && !dry.applied, "dry run report: found {} applied {}", dry.found, dry.applied);
    ensure!(!data.exists() || fs::read_dir(&data)?.next().is_none(), "the dry run wrote to {}", data.display());
    ensure!(
        count(&dry, T3ItemState::Import) > 20,
        "the dry run found only {} items to import",
        count(&dry, T3ItemState::Import)
    );
    println!(
        "dry run: {} to import, {} not supported",
        count(&dry, T3ItemState::Import),
        count(&dry, T3ItemState::Unsupported)
    );

    let first = report(&cli(binary, fixture, &data, &["--json"])?)?;
    check_first(fixture, &data, &first)?;
    println!("first import: {} imported", count(&first, T3ItemState::Import));
    let saved = files(&data)?;

    let second = report(&cli(binary, fixture, &data, &["--json"])?)?;
    ensure!(
        count(&second, T3ItemState::Import) == 0,
        "the second import changed {} items",
        count(&second, T3ItemState::Import)
    );
    ensure!(files(&data)? == saved, "the second import rewrote Local Studio state");
    println!("second import: idempotent ({} in sync)", count(&second, T3ItemState::Synced));

    edit(&data.join("settings.json"), |settings| settings["chatWidth"] = json!("full"))?;
    edit(&data.join("keybindings.json"), |keys| keys["chat.new"] = json!("cmd+shift+y"))?;
    edit(&data.join("state.json"), |state| {
        if let Some(list) = state["favorites"].as_array_mut() {
            list.retain(|key| key["model"] != "gpt-6.1-sol");
        }
        if let Some(list) = state["projects"].as_array_mut() {
            list.retain(|project| !project["path"].as_str().unwrap_or("").ends_with("/beta"));
        }
    })?;
    write_json(&fixture.t3.join("client-settings.json"), &t3_client(17, "comfortable"))?;
    let third = report(&cli(binary, fixture, &data, &["--json"])?)?;
    let settings = read_json(&data.join("settings.json"))?;
    ensure!(
        settings["chatWidth"] == "full",
        "a chat width changed in Local Studio was overwritten: {}",
        settings["chatWidth"]
    );
    ensure!(
        settings["interfaceFontSize"] == 17,
        "a T3 change to an untouched value did not sync: {}",
        settings["interfaceFontSize"]
    );
    let keys = read_json(&data.join("keybindings.json"))?;
    ensure!(keys["chat.new"] == "cmd+shift+y", "a shortcut changed in Local Studio was overwritten");
    let state = read_json(&data.join("state.json"))?;
    let text = state.to_string();
    ensure!(!state["favorites"].to_string().contains("gpt-6.1-sol"), "a favorite removed in Local Studio came back");
    ensure!(!text.contains("/beta"), "a project removed in Local Studio came back");
    expect_state(&third, "setting.chatWidth", T3ItemState::Kept)?;
    expect_state(&third, "keybinding:chat.new", T3ItemState::Kept)?;
    expect_state(&third, "favorite:codex/gpt-6.1-sol", T3ItemState::Kept)?;
    expect_state(&third, &format!("project:{}", path_text(&fixture.beta)), T3ItemState::Kept)?;
    println!("changes made in Local Studio kept: {}", count(&third, T3ItemState::Kept));
    write_json(&fixture.t3.join("client-settings.json"), &t3_client(15, "wide"))?;

    let mut t3_after = tree(&fixture.t3)?;
    t3_after.remove("client-settings.json");
    let mut t3_before = t3_before;
    t3_before.remove("client-settings.json");
    ensure!(t3_after == t3_before, "the import wrote into T3 Code's data dir");
    for (before, dir) in config_before.into_iter().zip([&fixture.personal, &fixture.work, &fixture.codex]) {
        ensure!(before? == tree(dir)?, "the import changed {}", dir.display());
    }
    thread_stage(binary, fixture, &data)?;
    no_canary(&data)
}

struct Nudge;

impl Waker for Nudge {
    fn wake(&self) {}
}

struct App {
    studio: Arc<Studio>,
    report: Option<T3Report>,
    settings: Option<local_studio_core::Settings>,
    projects: Vec<String>,
}

impl App {
    fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<()> {
        let started = Instant::now();
        loop {
            for update in self.studio.tick() {
                match update {
                    Update::T3Sync { report } => self.report = Some(report),
                    Update::Settings { settings } => self.settings = Some(settings),
                    Update::Projects { projects } => {
                        self.projects = projects.into_iter().map(|project| project.path).collect();
                    }
                    _ => {}
                }
            }
            if done(self) {
                return Ok(());
            }
            if started.elapsed() > WAIT_LIMIT {
                bail!("timed out waiting for {what}; last report {:?}", self.report);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn studio_stage(fixture: &Fixture, scratch: &Path) -> Result<()> {
    std::env::set_var("HOME", &fixture.home);
    std::env::set_var("LOCAL_STUDIO_CLAUDE_BIN", "/usr/bin/false");
    std::env::set_var("LOCAL_STUDIO_CODEX_BIN", "/usr/bin/false");
    std::env::set_var("EUKHE_CODING_AGENT_DIR", scratch.join("agent"));
    for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "T3CODE_HOME", "XDG_DATA_HOME", "LOCAL_STUDIO_DATA_DIR"] {
        std::env::remove_var(name);
    }
    let data = scratch.join("app");
    let config = StudioConfig {
        socket_path: Some(path_text(&scratch.join("d.sock"))),
        session_dir: None,
        data_dir: Some(path_text(&data)),
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let second = StudioConfig { socket_path: Some(path_text(&scratch.join("second.sock"))), ..config.clone() };
    let studio = Studio::new(config, Arc::new(Nudge)).map_err(|error| anyhow::anyhow!("{error}"))?;
    match Studio::new(second, Arc::new(Nudge)) {
        Err(StudioError::Owned { pid, path, waited }) => {
            ensure!(pid == Some(std::process::id()), "the refusal names process {pid:?}, not the owner");
            ensure!(path == path_text(&data) && !waited, "the refusal names {path} (waited {waited})");
            println!("app lock: a second app on the same data dir is refused and told which process owns it");
        }
        Err(error) => bail!("a second app on the same data dir failed with {error:?}, not Owned"),
        Ok(_) => bail!("a second app opened the data dir the first one owns"),
    }
    let mut app = App { studio, report: None, settings: None, projects: Vec::new() };
    app.wait("the first-run offer", |app| {
        app.report.as_ref().is_some_and(|report| report.found && !report.offered && !report.applied)
    })?;
    ensure!(!data.join("keybindings.json").exists(), "the offer wrote before the user chose");
    app.studio.import_t3(T3Choices { keybindings: false, ..T3Choices::default() });
    app.wait("the import", |app| {
        app.report.as_ref().is_some_and(|report| report.applied && report.enabled)
            && app.settings.as_ref().is_some_and(|settings| settings.harness_accounts.claude.len() == 2)
            && app.projects.len() == 2
            && thread_of(&data, "t-tree").is_ok_and(|thread| thread.is_some())
    })?;
    let report = app.report.clone().context("no report")?;
    ensure!(
        !report.groups.iter().any(|group| group.kind == local_studio_core::T3Kind::Keybindings && group.chosen),
        "keybindings were chosen"
    );
    ensure!(!data.join("keybindings.json").exists(), "keybindings were imported though they were not chosen");
    println!("app import: {} imported, accounts and projects pushed", count(&report, T3ItemState::Import));

    write_json(&fixture.t3.join("client-settings.json"), &t3_client(18, "wide"))?;
    app.wait("the change in T3 Code to sync", |app| {
        app.settings.as_ref().is_some_and(|settings| settings.interface_font_size == 18)
    })?;
    println!("app sync: a T3 Code change arrived on its own");

    let mut changed = app.settings.clone().context("no settings")?;
    changed.chat_width = ChatWidth::Full;
    app.studio.update_settings(changed);
    app.wait("the user change", |app| {
        app.settings.as_ref().is_some_and(|settings| settings.chat_width == ChatWidth::Full)
    })?;
    write_json(&fixture.t3.join("client-settings.json"), &t3_client(16, "comfortable"))?;
    app.wait("the next sync", |app| {
        app.settings.as_ref().is_some_and(|settings| settings.interface_font_size == 16)
            && app
                .report
                .as_ref()
                .is_some_and(|report| state_of(report, "setting.chatWidth") == Some(T3ItemState::Kept))
    })?;
    ensure!(
        app.settings.as_ref().is_some_and(|settings| settings.chat_width == ChatWidth::Full),
        "the user's chat width was overwritten"
    );
    println!("app sync: the user's own change was kept");

    app.studio.set_t3_sync(false);
    app.wait("sync off", |app| app.report.as_ref().is_some_and(|report| !report.enabled))?;
    write_json(&fixture.t3.join("client-settings.json"), &t3_client(20, "wide"))?;
    std::thread::sleep(Duration::from_secs(8));
    let _ = app.wait("a settled view", |_| true);
    ensure!(
        app.settings.as_ref().is_some_and(|settings| settings.interface_font_size == 16),
        "sync ran while it was off"
    );
    println!("app sync: off means off");
    app.studio.shutdown();
    no_canary(&data)
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: t3_import_smoke <local-studio binary> <scratch dir>";
    let binary = PathBuf::from(args.next().context(usage)?);
    let scratch = PathBuf::from(args.next().context(usage)?);
    let fixture = build(&scratch)?;
    cli_stages(&fs::canonicalize(&binary)?, &fixture, &scratch)?;
    studio_stage(&fixture, &scratch)
}
