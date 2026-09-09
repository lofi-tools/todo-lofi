//! Scans the home directory for local git repositories at startup, skipping
//! well-known junk directories, so they can be listed as projects in the nav
//! bar.

use std::path::{Path, PathBuf};

use gpui::{AppContext, Task};

/// Maximum directory depth below the home directory to descend. Depth 5 keeps
/// the scan bounded while still catching nested workspaces like ~/src/a/b.
const MAX_SCAN_DEPTH: usize = 5;

/// Directory names never worth traversing: toolchain caches, system dirs,
/// dependency trees. `.git` itself is excluded so repos nested inside other
/// checkouts don't duplicate entries.
const SKIPPED_DIRS: &[&str] = &[
    ".cargo",
    ".rustup",
    ".git",
    ".Trash",
    "Library",
    "Applications",
    "node_modules",
    "target",
    ".cache",
    ".npm",
    ".local",
    ".venv",
    "venv",
];

/// A local git repository found on disk.
#[derive(Clone, Debug)]
pub struct Project {
    /// Directory name (also used as the display label).
    pub name: String,
    /// Absolute path to the repository root (parent of `.git`).
    pub path: PathBuf,
}

impl Project {
    /// Read-only summary shown in the details pane: path, current branch and
    /// dirty-file count. Runs on the tokio runtime; failures degrade to
    /// "unknown" instead of erroring so one broken repo doesn't break the
    /// pane.
    pub fn describe(&self, cx: &impl AppContext) -> Task<anyhow::Result<String>> {
        let path = self.path.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let branch = git(&path, ["rev-parse", "--abbrev-ref", "HEAD"]).await;
            let status = git(&path, ["status", "--porcelain"]).await;
            let dirty = match status {
                Some(out) => out.lines().filter(|l| !l.trim().is_empty()).count(),
                None => 0,
            };
            Ok(format!(
                "{}\n\nBranch: {}\nDirty files: {}",
                path.display(),
                branch.unwrap_or_else(|| "unknown".into()),
                dirty
            ))
        })
    }
}

/// Run `git` in `dir` and return trimmed stdout, or `None` if the command
/// fails (no git installed, not a repo, etc.).
async fn git(dir: &Path, args: impl IntoIterator<Item = &str>) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Scan the home directory for git repos on the tokio runtime. Returns repos
/// sorted by name; the scan is best-effort and never fails the startup path.
pub fn scan(cx: &impl AppContext) -> Task<anyhow::Result<Vec<Project>>> {
    gpui_tokio::Tokio::spawn_result(cx, async move {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Ok(Vec::new());
        };
        let mut repos = Vec::new();
        walk(&home, 0, &mut repos);
        repos.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(repos)
    })
}

/// Recursive depth-first walk collecting the first repo found per subtree.
/// A directory containing `.git` is a repo root; nested repos inside it are
/// not reported separately.
fn walk(dir: &Path, depth: usize, repos: &mut Vec<Project>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    let mut has_git = false;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name == ".git" {
            has_git = true;
            continue;
        }
        if SKIPPED_DIRS.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        subdirs.push(entry.path());
    }
    if has_git {
        repos.push(Project {
            name: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.to_string_lossy().into_owned()),
            path: dir.to_path_buf(),
        });
        return;
    }
    for subdir in subdirs {
        walk(&subdir, depth + 1, repos);
    }
}
