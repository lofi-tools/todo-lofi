//! Finding Git repositories under a directory, and shaping them into a tree the
//! project picker can expand in place.
//!
//! The scan is best-effort: unreadable directories are skipped, and only
//! directories that lead to a repository appear in the result. Chains of
//! single-child directories collapse into one entry. Siblings stay at the same
//! level regardless of how many there are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Maximum directory depth below the scan root. Deeper trees than this are
/// rare, and the limit keeps a home-directory scan bounded.
const MAX_SCAN_DEPTH: usize = 5;

/// Directories never worth descending into. Toolchain caches and dependency
/// trees are noise; the macOS media folders are listed because reaching them
/// makes a bundled app prompt for TCC permissions.
const SKIPPED_DIRS: &[&str] = &[
    ".cargo",
    ".cache",
    ".git",
    ".local",
    ".npm",
    ".rustup",
    ".Trash",
    ".venv",
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "node_modules",
    "target",
    "venv",
];

/// One entry in the picker's tree. A node is either a repository the user can
/// add or a directory that leads to repositories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectNode {
    /// Display name. A collapsed chain joins its segments with `/`.
    pub name: String,
    /// Absolute path of the repository or directory.
    pub path: String,
    pub repository: bool,
    pub children: Vec<ProjectNode>,
}

/// The result of one scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScan {
    pub root: String,
    pub repositories: usize,
    pub tree: Vec<ProjectNode>,
}

/// Scan `root`, returning only the directories that lead to repositories.
pub fn scan(root: &Path) -> ProjectScan {
    let mut repositories = 0;
    let mut tree = Vec::new();
    if let Some((_, subdirectories)) = read_directory(root) {
        for subdirectory in subdirectories {
            if matches!(
                subdirectory.file_name().and_then(|name| name.to_str()),
                Some(".claude" | ".codex" | "ComfyUI-Installs")
            ) {
                continue;
            }
            if let Some(node) = walk(&subdirectory, 1, &mut repositories) {
                tree.push(collapse(node));
            }
        }
    }
    tree.sort_by(|left, right| left.name.cmp(&right.name));
    ProjectScan {
        root: root.display().to_string(),
        repositories,
        tree,
    }
}

struct RawNode {
    name: String,
    path: PathBuf,
    repository: bool,
    children: BTreeMap<String, RawNode>,
}

fn walk(dir: &Path, depth: usize, repositories: &mut usize) -> Option<RawNode> {
    if depth > MAX_SCAN_DEPTH {
        return None;
    }
    let (has_git, subdirectories) = read_directory(dir)?;
    let name = leaf_name(dir);

    // A repository stops the descent: repositories nested inside a checkout are
    // not reported separately.
    if has_git {
        *repositories += 1;
        return Some(RawNode {
            name,
            path: dir.to_path_buf(),
            repository: true,
            children: BTreeMap::new(),
        });
    }

    let mut children = BTreeMap::new();
    for subdirectory in subdirectories {
        let Some(child) = walk(&subdirectory, depth + 1, repositories) else {
            continue;
        };
        children.insert(child.name.clone(), child);
    }
    if children.is_empty() {
        return None;
    }

    Some(RawNode {
        name,
        path: dir.to_path_buf(),
        repository: false,
        children,
    })
}

fn read_directory(dir: &Path) -> Option<(bool, Vec<PathBuf>)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut has_git = false;
    let mut subdirectories = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if name == ".git" && (file_type.is_dir() || file_type.is_file()) {
            has_git = true;
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        if SKIPPED_DIRS.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        subdirectories.push(entry.path());
    }
    Some((has_git, subdirectories))
}

fn leaf_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// Fold chains of single-child directories into one entry, so
/// `src/me/project` shows as `src/me` containing `project` instead of three
/// nested clicks that each reveal one item.
fn collapse(mut node: RawNode) -> ProjectNode {
    let mut name = node.name.clone();
    loop {
        if node.repository || node.children.len() != 1 {
            break;
        }
        let Some(key) = node.children.keys().next().cloned() else {
            break;
        };
        let Some(child) = node.children.remove(&key) else {
            break;
        };
        name = format!("{name}/{key}");
        node = child;
    }

    ProjectNode {
        name,
        path: node.path.display().to_string(),
        repository: node.repository,
        children: node.children.into_values().map(collapse).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDirectory, TestRepository};

    #[test]
    fn finds_git_worktrees_with_a_git_file() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let worktree = root.join("checkout");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["worktree", "add", "--detach"])
            .arg(&worktree)
            .status()?;
        assert!(status.success());
        assert!(worktree.join(".git").is_file());
        std::fs::create_dir_all(worktree.join("nested/.git"))?;

        let result = scan(root.path());
        assert_eq!(result.repositories, 1);
        let checkout = result.tree.first().ok_or("missing worktree")?;
        assert_eq!(checkout.path, worktree.display().to_string());
        assert!(checkout.repository);
        assert!(checkout.children.is_empty());
        Ok(())
    }

    fn repository(root: &TestDirectory, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.join(".git")).expect("repo directory");
    }

    fn node<'a>(nodes: &'a [ProjectNode], name: &str) -> &'a ProjectNode {
        nodes
            .iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| panic!("no node named {name} in {nodes:?}"))
    }

    #[test]
    fn finds_repositories_and_stops_at_repository_roots() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = TestDirectory::new()?;
        repository(&root, "alpha");
        repository(&root, "alpha/nested");
        repository(&root, "src/beta");
        repository(&root, "node_modules/pkg");
        repository(&root, "Documents/private");

        let scan = scan(root.path());
        assert_eq!(scan.repositories, 2, "tree: {:?}", scan.tree);

        let alpha = node(&scan.tree, "alpha");
        assert!(alpha.repository);
        assert!(alpha.children.is_empty(), "a repo stops the descent");

        // `src` holds a single repository, so it collapses into one row.
        let beta = node(&scan.tree, "src/beta");
        assert!(beta.repository);
        assert_eq!(beta.path, root.join("src/beta").display().to_string());
        Ok(())
    }

    #[test]
    fn collapses_single_child_chains() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        repository(&root, "src/me/project");
        repository(&root, "src/me/other");

        let scan = scan(root.path());
        let collapsed = node(&scan.tree, "src/me");
        assert_eq!(collapsed.path, root.join("src/me").display().to_string());
        assert!(!collapsed.repository);
        assert_eq!(
            collapsed
                .children
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            vec!["other", "project"]
        );
        Ok(())
    }

    #[test]
    fn enumerates_all_siblings_under_the_same_parent() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        for index in 0..12 {
            repository(&root, &format!("many/repo-{index:02}"));
        }

        let scan = scan(root.path());
        let many = node(&scan.tree, "many");
        assert_eq!(scan.repositories, 12);
        let names: Vec<_> = many
            .children
            .iter()
            .map(|child| child.name.clone())
            .collect();
        assert_eq!(
            names,
            (0..12)
                .map(|index| format!("repo-{index:02}"))
                .collect::<Vec<_>>()
        );
        assert!(many.children.iter().all(|child| child.repository));
        Ok(())
    }

    #[test]
    fn skips_home_media_and_tool_directories() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        repository(&root, "Pictures/holiday");
        repository(&root, ".claude/cache/plugin");
        repository(&root, ".codex/worktrees/project");
        repository(&root, "ComfyUI-Installs/ComfyUI");
        repository(&root, "work/.codex/project");
        repository(&root, "work/real");

        let scan = scan(root.path());
        assert_eq!(scan.repositories, 2);
        assert_eq!(scan.tree.len(), 1);
        let work = node(&scan.tree, "work");
        assert!(node(&work.children, ".codex/project").repository);
        assert!(node(&work.children, "real").repository);
        Ok(())
    }
}
