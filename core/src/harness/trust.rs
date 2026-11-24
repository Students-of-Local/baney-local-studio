use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::Harness;
use crate::store::home_dir;
use crate::threads::git_dir;

const CLAUDE: &[&str] = &[".claude", ".mcp.json", "CLAUDE.md", "CLAUDE.local.md"];
const CODEX: &[&str] = &[".codex"];

fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn home() -> Option<PathBuf> {
    home_dir().map(|home| real(&home))
}

pub(crate) fn own(cwd: &str) -> bool {
    home() == Some(real(Path::new(cwd)))
}

pub(crate) fn root(cwd: &str) -> String {
    let cwd = real(Path::new(cwd));
    let root = match git_dir(&cwd) {
        Some((dot_git, root)) if Some(&root) != home().as_ref() => main_checkout(&dot_git).unwrap_or(root),
        _ => cwd,
    };
    root.to_string_lossy().into_owned()
}

fn main_checkout(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_symlink() {
        return None;
    }
    let text = std::fs::read_to_string(dot_git).ok()?;
    let target = dot_git.parent()?.join(text.trim().strip_prefix("gitdir:")?.trim()).canonicalize().ok()?;
    let back = std::fs::read_to_string(target.join("gitdir")).ok()?;
    let worktrees = target.parent()?;
    let common = worktrees.parent()?;
    let linked = worktrees.file_name()? == "worktrees"
        && common.file_name()? == ".git"
        && target.join(back.trim()).canonicalize().ok()? == dot_git.canonicalize().ok()?;
    linked.then(|| common.parent().map(Path::to_path_buf)).flatten()
}

pub(crate) fn folders(cwd: &str) -> BTreeSet<String> {
    let here = real(Path::new(cwd));
    let top = git_dir(&here).map_or_else(|| here.clone(), |(_, root)| root);
    let paths = [here, top].into_iter().map(|path| path.to_string_lossy().into_owned());
    paths.chain([cwd.to_string(), root(cwd)]).collect()
}

pub(crate) fn has_config(harness: Harness, cwd: &str) -> bool {
    let names = if harness == Harness::Codex { CODEX } else { CLAUDE };
    let (cwd, home) = (real(Path::new(cwd)), home());
    cwd.ancestors()
        .take_while(|dir| home.as_deref() != Some(*dir))
        .any(|dir| names.iter().any(|name| dir.join(name).exists()))
}
