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

use crate::store::Store;

/// A local git repository found on disk.
#[derive(Clone, Debug)]
pub struct Project {
    /// Directory name (also used as the display label).
    pub name: String,
    /// Absolute path to the repository root (parent of `.git`).
    pub path: PathBuf,
}

impl Project {
    /// Get-or-create the tag backing this project folder: a unique name
    /// derived from the canonical absolute path plus the directory name as
    /// display name. Runs on the tokio runtime.
    pub fn tag(
        &self,
        store: &Store,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Tag>> {
        let store = store.clone();
        let path = self.path.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.0.lock().await;
            s.get_or_create_project_tag(&path).await.map_err(Into::into)
        })
    }
}

/// Scan for git repos on the tokio runtime. Returns repos sorted by name; the
/// scan is best-effort and never fails the startup path.
///
/// `TODO_LOFI_SCAN_ROOT` scans that directory instead of the default
/// `~/src`. The scan stays out of the home root itself so a bundled macOS
/// app never walks TCC-protected folders (Documents, Music, Photos).
pub fn scan(cx: &impl AppContext) -> Task<anyhow::Result<Vec<Project>>> {
    gpui_tokio::Tokio::spawn_result(cx, async move {
        let root = scan_root();
        let mut repos = Vec::new();
        walk(&root, 0, &mut repos);
        repos.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(repos)
    })
}

/// Where the repo scan starts: `TODO_LOFI_SCAN_ROOT`, defaulting to `~/src`.
fn scan_root() -> PathBuf {
    if let Ok(root) = std::env::var("TODO_LOFI_SCAN_ROOT")
        && !root.is_empty()
    {
        return PathBuf::from(root);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("src"))
        .unwrap_or_else(|| PathBuf::from("src"))
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

#[cfg(test)]
mod tests {
    use super::*;

    static SCAN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_scan_root_follows_the_env() {
        let Ok(_guard) = SCAN_ENV_LOCK.lock() else {
            unreachable!("scan env lock poisoned");
        };
        unsafe {
            std::env::remove_var("TODO_LOFI_SCAN_ROOT");
        }
        assert_eq!(
            scan_root(),
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join("src"))
                .unwrap_or_else(|| PathBuf::from("src"))
        );
        unsafe {
            std::env::set_var("TODO_LOFI_SCAN_ROOT", "/tmp/somewhere");
        }
        assert_eq!(scan_root(), PathBuf::from("/tmp/somewhere"));
        unsafe {
            std::env::remove_var("TODO_LOFI_SCAN_ROOT");
        }
    }
}
