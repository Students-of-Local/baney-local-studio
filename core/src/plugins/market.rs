use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use super::install::{full_sha, remote, Scratch};
use super::{inside, resolve_channel, Source, MANIFEST};

pub const CLAUDE_MARKET: &str = ".claude-plugin/marketplace.json";
pub const CODEX_MARKET: &str = ".agents/plugins/marketplace.json";
const PLUGIN_FILES: [&str; 4] = [MANIFEST, ".claude-plugin/plugin.json", ".codex-plugin/plugin.json", "plugin.json"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub description: String,
    pub from: String,
}

enum Reader {
    Dir(PathBuf),
    Git { scratch: Scratch, rev: String },
}

impl Reader {
    fn open(source: &Source) -> Result<Self, String> {
        match source {
            Source::Dir(dir) => Ok(Reader::Dir(dir.clone())),
            Source::Git { url, rev } => {
                let scratch =
                    Scratch::new(&std::env::temp_dir(), "ls-plugin-market").map_err(|errors| errors.join("; "))?;
                let reader = Reader::Git { scratch, rev: rev.clone() };
                reader.git(&["init", "-q"])?;
                reader.git(&["remote", "add", "origin", url])?;
                reader.git(&["config", "remote.origin.promisor", "true"])?;
                reader.git(&["config", "remote.origin.partialclonefilter", "blob:none"])?;
                if reader.git(&["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", rev]).is_err() {
                    reader.git(&[
                        "fetch",
                        "-q",
                        "--filter=blob:none",
                        "origin",
                        "+refs/heads/*:refs/remotes/origin/*",
                        "+refs/tags/*:refs/tags/*",
                    ])?;
                }
                Ok(reader)
            }
            _ => Err("a marketplace must be a folder or a git URL pinned to a commit".to_string()),
        }
    }

    fn git(&self, args: &[&str]) -> Result<String, String> {
        let Reader::Git { scratch, .. } = self else { return Err("not a git marketplace".to_string()) };
        let output = Command::new("git")
            .arg("-C")
            .arg(scratch.path())
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| format!("cannot run git: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
        }
    }

    fn read(&self, relative: &str) -> Option<String> {
        match self {
            Reader::Dir(dir) => {
                let path = dir.join(relative);
                let meta = std::fs::symlink_metadata(&path).ok()?;
                (meta.is_file() && meta.len() <= super::MAX_FILE).then(|| std::fs::read_to_string(&path).ok())?
            }
            Reader::Git { rev, .. } => self.git(&["show", &format!("{rev}:{relative}")]).ok(),
        }
    }

    fn exists(&self, relative: &str) -> bool {
        match self {
            Reader::Dir(dir) => std::fs::symlink_metadata(dir.join(relative)).is_ok(),
            Reader::Git { rev, .. } => self.git(&["cat-file", "-e", &format!("{rev}:{relative}")]).is_ok(),
        }
    }
}

fn markets(reader: &Reader) -> Vec<(&'static str, Map<String, Value>)> {
    [CLAUDE_MARKET, CODEX_MARKET]
        .into_iter()
        .filter_map(|file| {
            let text = reader.read(file)?;
            match serde_json::from_str::<Value>(&text).ok()? {
                Value::Object(object) => Some((file, object)),
                _ => None,
            }
        })
        .collect()
}

fn plugins(market: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    market.get("plugins").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object).collect()
}

fn from(entry: &Map<String, Value>) -> String {
    match entry.get("source") {
        Some(Value::String(path)) => path.clone(),
        Some(Value::Object(source)) => {
            let kind = source.get("source").and_then(Value::as_str).unwrap_or("?");
            match kind {
                "npm" | "archive" | "command" => format!("{kind}, which Local Studio does not install"),
                "local" => source.get("path").and_then(Value::as_str).unwrap_or("?").to_string(),
                _ => source
                    .get("url")
                    .or_else(|| source.get("repo"))
                    .and_then(Value::as_str)
                    .map_or_else(|| kind.to_string(), str::to_string),
            }
        }
        _ => "?".to_string(),
    }
}

pub(crate) fn listing(dir: &Path) -> Option<Vec<String>> {
    let reader = Reader::Dir(dir.to_path_buf());
    let names: Vec<String> = markets(&reader)
        .iter()
        .flat_map(|(_, market)| plugins(market))
        .filter_map(|entry| entry.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    (!names.is_empty()).then_some(names)
}

/// The plugins a marketplace lists, or `None` when the source is a plugin itself or no marketplace.
pub fn entries(source: &Source) -> Result<Option<Vec<Entry>>, String> {
    if !matches!(source, Source::Dir(_) | Source::Git { .. }) {
        return Ok(None);
    }
    let reader = Reader::open(source)?;
    if PLUGIN_FILES.iter().any(|file| reader.exists(file)) || reader.exists("skills") {
        return Ok(None);
    }
    let found: Vec<Entry> = markets(&reader)
        .iter()
        .flat_map(|(_, market)| plugins(market))
        .filter_map(|entry| {
            Some(Entry {
                name: entry.get("name").and_then(Value::as_str)?.to_string(),
                description: entry.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                from: from(entry),
            })
        })
        .collect();
    Ok((!found.is_empty()).then_some(found))
}

/// Where one plugin of a marketplace comes from, as a source `install` takes.
pub fn resolve(source: &Source, name: &str) -> Result<Source, String> {
    let reader = Reader::open(source)?;
    let found = markets(&reader).into_iter().find_map(|(file, market)| {
        let root = market
            .get("metadata")
            .and_then(|metadata| metadata.get("pluginRoot"))
            .and_then(Value::as_str)
            .map(str::to_string);
        plugins(&market)
            .into_iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
            .map(|entry| (file, root, entry.get("source").cloned().unwrap_or(Value::Null)))
    });
    let Some((file, root, spec)) = found else {
        return Err(format!("no plugin named {name} in {CLAUDE_MARKET} or {CODEX_MARKET}"));
    };
    let local = |path: &str| -> Result<Source, String> {
        let relative = relative(path, root.as_deref()).map_err(|error| format!("{file} {name}: {error}"))?;
        match source {
            Source::Dir(dir) => {
                let target = dir.join(&relative);
                let base = dir.canonicalize().map_err(|error| format!("{}: {error}", dir.display()))?;
                let real =
                    target.canonicalize().map_err(|error| format!("{file} {name}: {} ({error})", target.display()))?;
                if !real.starts_with(&base) {
                    return Err(format!("{file} {name}: {path} leaves the marketplace"));
                }
                Ok(Source::Dir(if relative == "." { dir.clone() } else { target }))
            }
            Source::Git { url, rev } => Ok(git(url, rev, Some(relative))),
            _ => Err("a marketplace must be a folder or a git URL pinned to a commit".to_string()),
        }
    };
    match &spec {
        Value::String(path) => local(path),
        Value::Object(fields) => {
            let field = |key: &str| fields.get(key).and_then(Value::as_str);
            match field("source").unwrap_or_default() {
                "local" => local(field("path").ok_or_else(|| format!("{file} {name}: a local source needs a path"))?),
                "github" => {
                    let repo = field("repo").ok_or_else(|| format!("{file} {name}: a github source needs a repo"))?;
                    let url = format!("https://github.com/{}.git", repo.trim_end_matches(".git"));
                    let rev = pick(&url, field("ref"), field("sha"))?;
                    Ok(git(&url, &rev, None))
                }
                "url" => {
                    let url = field("url").ok_or_else(|| format!("{file} {name}: a url source needs a url"))?;
                    let rev = pick(url, field("ref"), field("sha"))?;
                    Ok(git(url, &rev, None))
                }
                "git-subdir" => {
                    let url = field("url").ok_or_else(|| format!("{file} {name}: a git-subdir source needs a url"))?;
                    let shorthand = !url.contains(':')
                        && !url.starts_with(['/', '.'])
                        && url.matches('/').count() == 1;
                    let url = if shorthand { format!("https://github.com/{url}.git") } else { url.to_string() };
                    let path = field("path").ok_or_else(|| format!("{file} {name}: a git-subdir source needs a path"))?;
                    let subdir = relative(path, None).map_err(|error| format!("{file} {name}: {error}"))?;
                    let rev = pick(&url, field("ref"), field("sha"))?;
                    Ok(git(&url, &rev, Some(subdir)))
                }
                kind @ ("npm" | "archive" | "command") => Err(format!(
                    "{name} comes from a {kind} source, which Local Studio refuses: it can fetch and pin only git and folders"
                )),
                other => Err(format!("{file} {name}: unknown source type {other:?}")),
            }
        }
        _ => Err(format!("{file} {name}: no source")),
    }
}

fn git(url: &str, rev: &str, subdir: Option<String>) -> Source {
    match subdir.filter(|subdir| subdir != ".") {
        None => Source::Git { url: url.to_string(), rev: rev.to_string() },
        Some(subdir) => Source::Follow {
            origin: format!("git {url}"),
            url: url.to_string(),
            rev: rev.to_string(),
            subdir: Some(subdir),
            exclude: Vec::new(),
        },
    }
}

fn relative(path: &str, root: Option<&str>) -> Result<String, String> {
    if path.contains('\\') {
        return Err(format!("{path} holds a backslash"));
    }
    let joined = match (path.strip_prefix("./"), root) {
        (None, Some(root)) if path != "." && !path.contains('/') => {
            format!("{}/{path}", root.trim_start_matches("./").trim_end_matches('/'))
        }
        (Some(rest), _) => rest.to_string(),
        (None, _) => path.to_string(),
    };
    let joined = joined.trim_end_matches('/');
    let joined = if joined.is_empty() { "." } else { joined };
    inside(Path::new(""), joined)?;
    Ok(joined.trim_start_matches("./").to_string())
}

fn pick(url: &str, reference: Option<&str>, sha: Option<&str>) -> Result<String, String> {
    if let Some(sha) = sha {
        return if full_sha(sha) { Ok(sha.to_string()) } else { Err(format!("{url}: sha {sha} is not a full commit")) };
    }
    let Some(reference) = reference else { return resolve_channel(url, crate::CHANNEL).map(|(_, rev)| rev) };
    let refs = remote(url, &["--heads", "--tags"])?;
    [format!("refs/tags/{reference}^{{}}"), format!("refs/tags/{reference}"), format!("refs/heads/{reference}")]
        .iter()
        .find_map(|wanted| refs.iter().find(|(name, _)| name == wanted).map(|(_, sha)| sha.clone()))
        .ok_or_else(|| format!("{url} has no branch or tag {reference}"))
}
