//! Finding Git repositories under a directory, and shaping them into a tree the
//! project picker can expand in place.
//!
//! The scan is best-effort: unreadable directories are skipped, and only
//! directories that lead to a repository appear in the result. Chains of
//! single-child directories collapse into one entry. Siblings stay at the same
//! level regardless of how many there are.

use std::collections::BTreeMap;
use std::io::Read;
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
pub fn scan(root: &Path) -> std::io::Result<ProjectScan> {
    let mut repositories = 0;
    let mut tree = root_children(root, &mut repositories).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("could not scan {}: {error}", root.display()),
        )
    })?;
    tree.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ProjectScan {
        root: root.display().to_string(),
        repositories,
        tree,
    })
}

#[cfg(unix)]
fn root_children(root: &Path, repositories: &mut usize) -> std::io::Result<Vec<ProjectNode>> {
    use rustix::fs::{Mode, OFlags, open};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::DIRECTORY;
    let directory = open(root, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let (_, children) = read_directory_at(&directory)?;
    Ok(children
        .into_iter()
        .filter(|name| {
            !matches!(
                name.to_str(),
                Some(".claude" | ".codex" | "ComfyUI-Installs")
            )
        })
        .filter_map(|name| walk_at(&directory, &name, &root.join(&name), 1, repositories))
        .map(collapse)
        .collect())
}

#[cfg(not(unix))]
fn root_children(root: &Path, repositories: &mut usize) -> std::io::Result<Vec<ProjectNode>> {
    let (_, children) = read_directory(root)?;
    Ok(children
        .into_iter()
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".claude" | ".codex" | "ComfyUI-Installs")
            )
        })
        .filter_map(|path| walk(&path, 1, repositories))
        .map(collapse)
        .collect())
}

struct RawNode {
    name: String,
    path: PathBuf,
    repository: bool,
    children: BTreeMap<String, RawNode>,
}

#[cfg(unix)]
fn walk_at(
    parent: &impl std::os::fd::AsFd,
    name: &std::ffi::OsStr,
    path: &Path,
    depth: usize,
    repositories: &mut usize,
) -> Option<RawNode> {
    use rustix::fs::{Mode, OFlags, openat};
    if depth > MAX_SCAN_DEPTH {
        return None;
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::DIRECTORY;
    let directory = openat(parent, name, flags, Mode::empty()).ok()?;
    let (has_git, subdirectories) = read_directory_at(&directory).ok()?;
    let name = leaf_name(path);
    if has_git {
        *repositories += 1;
        return Some(RawNode {
            name,
            path: path.to_path_buf(),
            repository: true,
            children: BTreeMap::new(),
        });
    }
    let mut children = BTreeMap::new();
    for child in subdirectories {
        if let Some(node) = walk_at(
            &directory,
            &child,
            &path.join(&child),
            depth + 1,
            repositories,
        ) {
            children.insert(node.name.clone(), node);
        }
    }
    if children.is_empty() {
        return None;
    }
    Some(RawNode {
        name,
        path: path.to_path_buf(),
        repository: false,
        children,
    })
}

#[cfg(unix)]
fn read_directory_at(
    directory: &impl std::os::fd::AsFd,
) -> std::io::Result<(bool, Vec<std::ffi::OsString>)> {
    use rustix::fs::{AtFlags, Dir, FileType, statat};
    use std::os::unix::ffi::OsStringExt;

    let entries = Dir::read_from(directory).map_err(std::io::Error::from)?;
    let mut has_git = false;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let Ok(metadata) = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        let file_type = FileType::from_raw_mode(metadata.st_mode);
        let name = std::ffi::OsString::from_vec(name.to_bytes().to_vec());
        if name == ".git" {
            has_git = file_type == FileType::Directory
                || (file_type == FileType::RegularFile && is_gitdir_file_at(directory, &name));
        } else if file_type == FileType::Directory
            && !SKIPPED_DIRS.contains(&name.to_string_lossy().as_ref())
        {
            children.push(name);
        }
    }
    Ok((has_git, children))
}

#[cfg(not(unix))]
fn walk(dir: &Path, depth: usize, repositories: &mut usize) -> Option<RawNode> {
    if depth > MAX_SCAN_DEPTH {
        return None;
    }
    let (has_git, subdirectories) = read_directory(dir).ok()?;
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

#[cfg(not(unix))]
fn read_directory(dir: &Path) -> std::io::Result<(bool, Vec<PathBuf>)> {
    let entries = std::fs::read_dir(dir)?;
    let mut has_git = false;
    let mut subdirectories = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if name == ".git" {
            let marker_file = file_type.is_file() && is_gitdir_file(&entry.path());
            has_git = file_type.is_dir() || marker_file;
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
    Ok((has_git, subdirectories))
}

#[cfg(unix)]
fn is_gitdir_file_at(directory: &impl std::os::fd::AsFd, name: &std::ffi::OsStr) -> bool {
    use rustix::fs::{Mode, OFlags, openat};
    let file = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from);
    is_gitdir_file_contents(file)
}

#[cfg(not(unix))]
fn is_gitdir_file(path: &Path) -> bool {
    is_gitdir_file_contents(std::fs::File::open(path))
}

fn is_gitdir_file_contents(file: std::io::Result<std::fs::File>) -> bool {
    let mut prefix = [0; 8];
    file.and_then(|mut file| {
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("Git marker is not a regular file"));
        }
        file.read_exact(&mut prefix)
    })
    .is_ok()
        && &prefix == b"gitdir: "
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

    struct TestDirectory {
        directory: tempfile::TempDir,
    }

    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            Ok(Self {
                directory: tempfile::tempdir()?,
            })
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.path().join(path)
        }
    }

    struct TestRepository(TestDirectory);

    impl TestRepository {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let directory = TestDirectory::new()?;
            let git = |arguments: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(directory.path())
                    .args(arguments)
                    .status()?;
                if !status.success() {
                    return Err(format!("git {arguments:?} failed").into());
                }
                Ok(())
            };
            git(&["init", "-b", "main"])?;
            std::fs::write(directory.join("README.md"), "initial\n")?;
            git(&[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Shepherd Test",
                "-c",
                "user.email=shepherd@example.com",
                "add",
                "README.md",
            ])?;
            git(&[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Shepherd Test",
                "-c",
                "user.email=shepherd@example.com",
                "commit",
                "-m",
                "initial",
            ])?;
            Ok(Self(directory))
        }

        fn path(&self) -> &Path {
            self.0.path()
        }
    }

    #[test]
    fn scan_reports_an_unavailable_root() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        let missing = root.join("missing");
        let error = scan(&missing).err().ok_or("missing root was accepted")?;
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains(&missing.display().to_string()));
        let file = root.join("file");
        std::fs::write(&file, "not a directory")?;
        assert!(scan(&file).is_err());
        Ok(())
    }

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

        let result = scan(root.path())?;
        assert_eq!(result.repositories, 1);
        let checkout = result.tree.first().ok_or("missing worktree")?;
        assert_eq!(checkout.path, worktree.display().to_string());
        assert!(checkout.repository);
        assert!(checkout.children.is_empty());
        Ok(())
    }

    #[test]
    fn ignores_non_git_marker_files_and_keeps_scanning() -> Result<(), Box<dyn std::error::Error>> {
        let root = TestDirectory::new()?;
        std::fs::create_dir_all(root.join("projects/child/.git"))?;
        std::fs::write(root.join("projects/.git"), "not a gitdir")?;
        std::fs::create_dir_all(root.join("unrelated"))?;
        std::fs::write(root.join("unrelated/.git"), "git")?;

        let result = scan(root.path())?;
        assert_eq!(result.repositories, 1);
        let child = result.tree.first().ok_or("missing repository")?;
        assert_eq!(
            child.path,
            root.join("projects/child").display().to_string()
        );
        assert!(child.repository);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn gitdir_marker_reader_rejects_symlinks_and_named_pipes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new()?;
        let directory = std::fs::File::open(root.path())?;
        std::fs::write(root.join("marker"), "gitdir: /fixture\n")?;
        symlink(root.join("marker"), root.join("link"))?;
        assert!(!is_gitdir_file_at(&directory, "link".as_ref()));
        let status = std::process::Command::new("mkfifo")
            .arg(root.join("pipe"))
            .status()?;
        assert!(status.success());
        assert!(!is_gitdir_file_at(&directory, "pipe".as_ref()));
        assert!(is_gitdir_file_at(&directory, "marker".as_ref()));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn gitdir_marker_reader_keeps_the_open_parent_after_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new()?;
        let original = root.join("projects");
        let outside = root.join("outside");
        std::fs::create_dir(&original)?;
        std::fs::create_dir(&outside)?;
        std::fs::write(original.join(".git"), "gitdir: /fixture\n")?;
        std::fs::write(outside.join(".git"), "not a gitdir")?;
        let directory = std::fs::File::open(&original)?;
        std::fs::rename(&original, root.join("moved"))?;
        symlink(&outside, &original)?;
        assert!(is_gitdir_file_at(&directory, ".git".as_ref()));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scan_descent_uses_open_parent_and_rejects_replaced_children()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new()?;
        let parent = root.join("projects");
        let outside = root.join("outside");
        std::fs::create_dir_all(parent.join("repo/.git"))?;
        std::fs::create_dir_all(outside.join("repo/.git"))?;
        std::fs::create_dir_all(outside.join("external/.git"))?;
        let directory = std::fs::File::open(&parent)?;
        std::fs::rename(&parent, root.join("moved"))?;
        symlink(&outside, &parent)?;
        let (_, names) = read_directory_at(&directory)?;
        assert_eq!(names, vec![std::ffi::OsString::from("repo")]);
        let mut count = 0;
        assert!(
            walk_at(
                &directory,
                "repo".as_ref(),
                &parent.join("repo"),
                1,
                &mut count
            )
            .is_some()
        );
        assert_eq!(count, 1);

        std::fs::rename(root.join("moved/repo"), root.join("saved"))?;
        symlink(outside.join("repo"), root.join("moved/repo"))?;
        assert!(
            walk_at(
                &directory,
                "repo".as_ref(),
                &parent.join("repo"),
                1,
                &mut count
            )
            .is_none()
        );
        assert_eq!(count, 1);
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

        let scan = scan(root.path())?;
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

        let scan = scan(root.path())?;
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

        let scan = scan(root.path())?;
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

        let scan = scan(root.path())?;
        assert_eq!(scan.repositories, 2);
        assert_eq!(scan.tree.len(), 1);
        let work = node(&scan.tree, "work");
        assert!(node(&work.children, ".codex/project").repository);
        assert!(node(&work.children, "real").repository);
        Ok(())
    }
}
