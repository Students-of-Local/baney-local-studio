use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use super::{
    install, load, mcp_servers, sync_mcp_servers, tree_hash, Capability, Catalog, CatalogEntry, Consent, Grants,
    Source,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ls-plugins-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create {}: {error}", dir.display()));
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| panic!("mkdir {}: {error}", parent.display()));
    }
    std::fs::write(path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn executable(path: &Path) {
    write(path, "#!/bin/sh\n");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

fn plugin(root: &Path, id: &str, version: &str, extra: &str) -> PathBuf {
    let dir = root.join(id);
    write(
        &dir.join("plugin.toml"),
        &format!("id = \"{id}\"\nversion = \"{version}\"\ndescription = \"A test plugin\"\n{extra}"),
    );
    dir
}

const SERVER: &str = "capabilities = [\"mcp-server\"]\n[mcp]\ncommand = \"bin/server\"\nargs = [\"mcp\"]\n";

fn served(root: &Path, id: &str, version: &str, capabilities: &str) -> PathBuf {
    let dir = plugin(
        root,
        id,
        version,
        &format!("capabilities = [{capabilities}]\n[mcp]\ncommand = \"bin/server\"\nargs = [\"mcp\"]\n"),
    );
    executable(&dir.join("bin/server"));
    dir
}

fn errors(dir: &Path) -> Vec<String> {
    match load(dir) {
        Ok(plugin) => panic!("{} loaded: {plugin:?}", dir.display()),
        Err(errors) => errors,
    }
}

#[test]
fn every_known_capability_loads_with_a_description() {
    let temp = Temp::new();
    let names: Vec<String> =
        Capability::ALL.iter().filter(|c| **c != Capability::McpServer).map(|c| format!("\"{}\"", c.name())).collect();
    let dir = plugin(&temp.0, "known", "1.0.0", &format!("capabilities = [{}]\n", names.join(", ")));
    let loaded = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(loaded.capabilities.len(), 6);
    assert!(loaded.mcp.is_none());
    for capability in Capability::ALL {
        assert_eq!(Capability::parse(capability.name()), Some(capability));
        assert!(!capability.description().is_empty());
    }
    let dir = served(&temp.0, "served", "1.0.0", "\"mcp-server\", \"ask-user\"");
    let loaded = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(loaded.capabilities, vec![Capability::McpServer, Capability::AskUser]);
    let mcp = loaded.mcp.unwrap_or_else(|| panic!("no mcp"));
    assert_eq!(mcp.command, dir.join("bin/server"));
    assert_eq!(mcp.args, vec!["mcp".to_string()]);
}

#[test]
fn an_unknown_capability_fails_the_load() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "unknown", "1.0.0", "capabilities = [\"ask-user\", \"network\"]\n");
    assert_eq!(errors(&dir), vec!["unknown capability: network".to_string()]);
    let dir = plugin(&temp.0, "twice", "1.0.0", "capabilities = [\"ask-user\", \"ask-user\"]\n");
    assert_eq!(errors(&dir), vec!["duplicate capability: ask-user".to_string()]);
}

#[test]
fn mcp_needs_the_capability_and_a_command_inside() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "bare", "1.0.0", "[mcp]\ncommand = \"bin/server\"\n");
    executable(&dir.join("bin/server"));
    assert_eq!(errors(&dir), vec!["[mcp] needs the mcp-server capability".to_string()]);
    let dir = plugin(&temp.0, "tableless", "1.0.0", "capabilities = [\"mcp-server\"]\n");
    assert_eq!(errors(&dir), vec!["the mcp-server capability needs an [mcp] table".to_string()]);
    let dir = plugin(&temp.0, "escape", "1.0.0", &SERVER.replace("bin/server", "../bin/server"));
    assert!(errors(&dir)[0].contains("path must stay inside the plugin"));
    let dir = plugin(&temp.0, "envy", "1.0.0", &format!("{SERVER}env = {{ KEY = \"ANTHROPIC_API_KEY\" }}\n"));
    executable(&dir.join("bin/server"));
    assert!(errors(&dir)[0].contains("must start with ENVY_"));
}

#[test]
fn grants_gate_the_mcp_server_entry() {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    let dir = served(&root, "tool-box", "1.0.0", "\"mcp-server\"");
    let loaded = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    let mut grants = Grants::default();
    assert_eq!(grants.consent(&loaded), Consent::NeedsConsent { missing: vec![Capability::McpServer] });
    assert!(mcp_servers(std::slice::from_ref(&loaded), &grants).is_empty());

    let agent = temp.0.join("agent");
    write(
        &agent.join("settings.json"),
        r#"{"defaultModel": "m", "mcpServers": {"mine": {"type": "stdio", "command": "x"}, "plugin-gone": {"type": "stdio", "command": "y"}}}"#,
    );
    assert!(sync_mcp_servers(&agent, std::slice::from_ref(&loaded), &grants).unwrap_or_else(|error| panic!("{error}")));
    let settings = read_json(&agent.join("settings.json"));
    assert_eq!(settings["mcpServers"], json!({"mine": {"type": "stdio", "command": "x"}}));
    assert_eq!(settings["defaultModel"], json!("m"));

    grants.grant(&loaded, &[Capability::McpServer]).unwrap_or_else(|error| panic!("{error}"));
    assert!(grants.grant(&loaded, &[Capability::AskUser]).is_err());
    grants.save(&root).unwrap_or_else(|error| panic!("{error}"));
    let grants = Grants::load(&root).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(grants.consent(&loaded), Consent::Ready);
    assert!(sync_mcp_servers(&agent, std::slice::from_ref(&loaded), &grants).unwrap_or_else(|error| panic!("{error}")));
    assert!(!sync_mcp_servers(&agent, std::slice::from_ref(&loaded), &grants).unwrap_or_else(|error| panic!("{error}")));
    let entry = &read_json(&agent.join("settings.json"))["mcpServers"]["plugin-tool-box"];
    let command = dir.join("bin/server").canonicalize().unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(entry["type"], json!("stdio"));
    assert_eq!(entry["command"], json!(command.to_string_lossy()));
    assert_eq!(entry["args"], json!(["mcp"]));

    let mut grants = grants;
    assert!(grants.revoke("tool-box", &[]));
    assert!(sync_mcp_servers(&agent, std::slice::from_ref(&loaded), &grants).unwrap_or_else(|error| panic!("{error}")));
    let settings = read_json(&agent.join("settings.json"));
    assert_eq!(settings["mcpServers"], json!({"mine": {"type": "stdio", "command": "x"}}));
}

#[test]
fn a_version_bump_that_adds_a_capability_asks_again() {
    let temp = Temp::new();
    let dir = served(&temp.0, "bumped", "0.1.0", "\"mcp-server\"");
    let first = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    let mut grants = Grants::default();
    grants.grant(&first, &[Capability::McpServer]).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(grants.granted("bumped"), Some(("0.1.0", vec!["mcp-server"])));

    served(&temp.0, "bumped", "0.2.0", "\"mcp-server\"");
    let same = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(grants.consent(&same), Consent::Ready);

    served(&temp.0, "bumped", "0.3.0", "\"mcp-server\", \"screen-capture\"");
    let more = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(grants.consent(&more), Consent::NeedsConsent { missing: vec![Capability::ScreenCapture] });
    assert!(mcp_servers(std::slice::from_ref(&more), &grants).is_empty());
    grants.grant(&more, &[Capability::ScreenCapture]).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(grants.granted("bumped"), Some(("0.3.0", vec!["mcp-server", "screen-capture"])));
    assert_eq!(mcp_servers(std::slice::from_ref(&more), &grants).len(), 1);
}

#[test]
fn a_plugin_that_needs_consent_still_lists_its_commands() {
    let temp = Temp::new();
    let dir = served(&temp.0, "waiting", "1.0.0", "\"mcp-server\"");
    write(&dir.join("prompts/hello.md"), "Say hello\n");
    write(
        &dir.join("plugin.toml"),
        &format!(
            "id = \"waiting\"\nversion = \"1.0.0\"\ndescription = \"x\"\nprompts = \"prompts\"\n{SERVER}[[commands]]\nname = \"hello\"\ndescription = \"Say hello\"\nprompt = \"prompts/hello.md\"\n"
        ),
    );
    let loaded = load(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert!(matches!(Grants::default().consent(&loaded), Consent::NeedsConsent { .. }));
    assert_eq!(super::commands(std::slice::from_ref(&loaded)).len(), 1);
    assert_eq!(loaded.prompts.len(), 1);
}

fn refused(source: &Path, needle: &str) {
    let temp = Temp::new();
    let root = temp.0.join("plugins");
    match install(&root, &Source::Dir(source.to_path_buf()), &Catalog::default()) {
        Ok(installed) => panic!("installed {:?}", installed.plugin.id),
        Err(errors) => assert!(errors.iter().any(|error| error.contains(needle)), "{needle} not in {errors:?}"),
    }
    let left: Vec<_> = std::fs::read_dir(&root).map(|dir| dir.filter_map(Result::ok).collect()).unwrap_or_default();
    assert!(left.is_empty(), "the refused install left {left:?}");
}

#[test]
fn install_refuses_a_symlink_pointing_outside() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "linky", "1.0.0", "");
    std::os::unix::fs::symlink("../../outside", dir.join("escape")).unwrap_or_else(|error| panic!("{error}"));
    refused(&dir, "symlink points outside the plugin: escape");
    let dir = plugin(&temp.0, "rooted", "1.0.0", "");
    std::os::unix::fs::symlink("/etc/passwd", dir.join("abs")).unwrap_or_else(|error| panic!("{error}"));
    refused(&dir, "symlink points outside the plugin: abs");
}

#[test]
fn install_refuses_a_file_over_ten_megabytes() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "heavy", "1.0.0", "");
    let file = std::fs::File::create(dir.join("weights.bin")).unwrap_or_else(|error| panic!("{error}"));
    file.set_len(super::MAX_FILE + 1).unwrap_or_else(|error| panic!("{error}"));
    refused(&dir, "file over 10 MB: weights.bin");
}

#[test]
fn install_refuses_an_executable_outside_bin() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "scripty", "1.0.0", "");
    executable(&dir.join("scripts/run.sh"));
    refused(&dir, "executable outside bin/: scripts/run.sh");
}

#[test]
fn install_refuses_a_command_path_outside_the_dir() {
    let temp = Temp::new();
    let dir = plugin(
        &temp.0,
        "wander",
        "1.0.0",
        "[[commands]]\nname = \"go\"\ndescription = \"x\"\nprompt = \"../secret.md\"\n",
    );
    refused(&dir, "path must stay inside the plugin: ../secret.md");
    let dir = plugin(&temp.0, "server-out", "1.0.0", &SERVER.replace("bin/server", "/usr/bin/env"));
    refused(&dir, "path must stay inside the plugin: /usr/bin/env");
}

#[test]
fn install_refuses_a_manifest_that_fails_validation() {
    let temp = Temp::new();
    let dir = plugin(&temp.0, "loose", "one", "");
    refused(&dir, "bad version");
}

#[test]
fn install_copies_a_clean_plugin_and_reports_consent() {
    let temp = Temp::new();
    let source = served(&temp.0.join("src"), "tidy", "1.0.0", "\"mcp-server\"");
    write(&source.join("skills/tidy/SKILL.md"), "---\nname: tidy\ndescription: Tidy up\n---\nBody\n");
    std::os::unix::fs::symlink("skills/tidy/SKILL.md", source.join("SKILL.md")).unwrap_or_else(|error| panic!("{error}"));
    let root = temp.0.join("plugins");
    let installed = install(&root, &Source::Dir(source.clone()), &Catalog::default())
        .unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(installed.plugin.dir, root.join("tidy"));
    assert_eq!(installed.consent, Consent::NeedsConsent { missing: vec![Capability::McpServer] });
    assert!(installed.replaced.is_none());
    let mode = std::fs::metadata(root.join("tidy/bin/server")).map(|meta| meta.permissions().mode()).unwrap_or(0);
    assert_ne!(mode & 0o111, 0);
    assert_eq!(installed.tree, tree_hash(&source, &[]).unwrap_or_default());
    let again = install(&root, &Source::Dir(source), &Catalog::default()).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(again.replaced.as_deref(), Some("1.0.0"));
}

#[test]
fn git_sources_must_be_pinned_to_a_full_sha() {
    for arg in ["https://github.com/a/b", "https://github.com/a/b@main", "https://github.com/a/b@5cdd089", "git@github.com:a/b.git"] {
        let error = Source::parse(arg).err().unwrap_or_else(|| panic!("{arg} parsed"));
        assert!(error.contains("full commit SHA"), "{arg}: {error}");
    }
    let sha = "5cdd0899732ccf87a892bd602b8fda45bc09489e";
    assert_eq!(
        Source::parse(&format!("https://github.com/a/b@{sha}")),
        Ok(Source::Git { url: "https://github.com/a/b".to_string(), rev: sha.to_string() })
    );
    assert_eq!(Source::parse("gantrel"), Ok(Source::Catalog("gantrel".to_string())));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git: {error}"));
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_catalog_tree_mismatch_refuses_the_install() {
    let temp = Temp::new();
    let repo = temp.0.join("repo");
    let source = plugin(&repo.join("plugins"), "pinned", "1.0.0", "");
    executable(&repo.join("tools/dev.sh"));
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);
    let tree = tree_hash(&source, &[]).unwrap_or_else(|error| panic!("{error}"));
    let entry = |tree: &str| CatalogEntry {
        id: "pinned".to_string(),
        git: repo.to_string_lossy().into_owned(),
        rev: rev.clone(),
        subdir: Some("plugins/pinned".to_string()),
        tree: tree.to_string(),
        exclude: Vec::new(),
    };
    let root = temp.0.join("plugins");
    let wrong = format!("sha256:{}", "0".repeat(64));
    let catalog = Catalog { plugins: vec![entry(&wrong)] };
    let errors = install(&root, &Source::Catalog("pinned".to_string()), &catalog).err().unwrap_or_default();
    assert!(errors.iter().any(|error| error.contains("tree hash mismatch")), "{errors:?}");
    assert!(!root.join("pinned").exists());

    let catalog = Catalog { plugins: vec![entry(&tree)] };
    let installed =
        install(&root, &Source::Catalog("pinned".to_string()), &catalog).unwrap_or_else(|errors| panic!("{errors:?}"));
    assert_eq!(installed.tree, tree);
    assert_eq!(installed.plugin.dir, root.join("pinned"));

    let whole = Catalog {
        plugins: vec![CatalogEntry {
            id: "pinned".to_string(),
            git: repo.to_string_lossy().into_owned(),
            rev: rev.clone(),
            subdir: None,
            tree: wrong.clone(),
            exclude: vec!["tools".to_string()],
        }],
    };
    let errors = install(&root, &Source::Catalog("pinned".to_string()), &whole).err().unwrap_or_default();
    assert!(errors.iter().any(|error| error.contains("tree hash mismatch")), "{errors:?}");
    let missing = Catalog::default();
    assert!(install(&root, &Source::Catalog("pinned".to_string()), &missing).is_err());
}

#[test]
fn the_bundled_catalog_pins_full_shas() {
    let catalog = Catalog::bundled().unwrap_or_else(|error| panic!("{error}"));
    assert!(catalog.entry("smoothstone").is_some());
    assert!(catalog.entry("gantrel").is_some());
    let bad = "[[plugin]]\nid = \"x\"\ngit = \"u\"\nrev = \"main\"\ntree = \"sha256:00\"\n";
    assert!(Catalog::parse(bad).is_err());
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_default()).unwrap_or(Value::Null)
}
