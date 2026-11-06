use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

const MAX_DIRECTORY_ENTRIES: usize = 5_000;
const MAX_IGNORE_BYTES: u64 = 1024 * 1024;
const MAX_VIEW_BYTES: usize = 256 * 1024;
const MAX_VIEW_LINES: usize = 4_000;
const MAX_HIGHLIGHT_LINE_BYTES: usize = 4_096;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("Choose a relative path inside the workspace.")]
    InvalidPath,
    #[error("Symbolic links are unavailable in the Files tab.")]
    SymbolicLink,
    #[error("The selected path is not a folder.")]
    NotDirectory,
    #[error("The selected folder is hidden or ignored.")]
    HiddenDirectory,
    #[error("The selected path is not a regular file.")]
    NotFile,
    #[error("The viewer supports UTF-8 text files.")]
    NonText,
    #[error("This file exceeds the {MAX_VIEW_BYTES}-byte viewer limit.")]
    TooLarge,
    #[error("Could not read workspace files: {0}")]
    Io(#[from] io::Error),
    #[error("Could not read a workspace ignore file: {0}")]
    Ignore(String),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    entries: Vec<FileEntry>,
    incomplete: bool,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    name: String,
    path: String,
    directory: bool,
}

#[derive(Debug, Serialize)]
pub struct TextFile {
    lines: Vec<Vec<TextSpan>>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct TextSpan {
    text: String,
    color: Option<String>,
}

pub fn list_directory(
    root: &Path,
    relative: &Path,
    show_dotfiles: bool,
) -> Result<DirectoryListing, FileError> {
    let relative = checked_path(relative)?;
    check_directory(root)?;
    let root = root.canonicalize()?;
    let mut directory = root.clone();
    let mut ancestors = relative.iter();
    let mut rules = [Vec::new(), Vec::new()];
    loop {
        for (name, matchers) in [".shepherdignore", ".gitignore"]
            .into_iter()
            .zip(&mut rules)
        {
            if let Some(matcher) = read_ignore(&root, &directory, name)? {
                matchers.push(matcher);
            }
        }
        let Some(name) = ancestors.next() else { break };
        directory.push(name);
        if (!show_dotfiles && is_dotfile(name)) || is_ignored(&rules, &directory, true) {
            return Err(FileError::HiddenDirectory);
        }
        check_directory(&directory)?;
    }

    let mut entries = Vec::new();
    let mut incomplete = false;
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_git_metadata(&name) || (!show_dotfiles && is_dotfile(&name)) {
            continue;
        }
        let kind = entry.file_type()?;
        let path = entry.path();
        if !(kind.is_dir() || kind.is_file()) || is_ignored(&rules, &path, kind.is_dir()) {
            continue;
        }
        let Some((name, path)) = name.to_str().zip(
            path.strip_prefix(&root)
                .map_err(|_| FileError::InvalidPath)?
                .to_str(),
        ) else {
            incomplete = true;
            continue;
        };
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            incomplete = true;
            break;
        }
        entries.push(FileEntry {
            name: name.into(),
            path: path.into(),
            directory: kind.is_dir(),
        });
    }
    entries.sort_by(|left, right| {
        right
            .directory
            .cmp(&left.directory)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(DirectoryListing {
        entries,
        incomplete,
    })
}

pub fn read_file(root: &Path, relative: &Path) -> Result<TextFile, FileError> {
    let relative = checked_path(relative)?;
    check_directory(root)?;
    let root = root.canonicalize()?;
    let file = open_file(&root, &relative)?;
    if !file.metadata()?.is_file() {
        return Err(FileError::NotFile);
    }
    let mut bytes = Vec::new();
    file.take((MAX_VIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_VIEW_BYTES {
        return Err(FileError::TooLarge);
    }
    let content = std::str::from_utf8(&bytes).map_err(|_| FileError::NonText)?;
    if content.contains('\0') {
        return Err(FileError::NonText);
    }
    Ok(highlight(&relative, content))
}

fn checked_path(path: &Path) -> Result<PathBuf, FileError> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !is_git_metadata(name) => relative.push(name),
            Component::CurDir => {}
            _ => return Err(FileError::InvalidPath),
        }
    }
    Ok(relative)
}

fn is_git_metadata(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
}

fn is_dotfile(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
}

fn check_directory(path: &Path) -> Result<(), FileError> {
    let kind = path.symlink_metadata()?.file_type();
    if kind.is_symlink() {
        return Err(FileError::SymbolicLink);
    }
    if !kind.is_dir() {
        return Err(FileError::NotDirectory);
    }
    Ok(())
}

fn is_ignored(rules: &[Vec<Gitignore>; 2], path: &Path, directory: bool) -> bool {
    rules
        .iter()
        .flat_map(|group| group.iter().rev())
        .map(|matcher| matcher.matched(path, directory))
        .find(|result| !result.is_none())
        .is_some_and(|result| result.is_ignore())
}

fn read_ignore(root: &Path, directory: &Path, name: &str) -> Result<Option<Gitignore>, FileError> {
    let path = directory.join(name);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| FileError::InvalidPath)?;
    let file = open_file(root, relative)?;
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut content = String::new();
    file.take(MAX_IGNORE_BYTES + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > MAX_IGNORE_BYTES {
        return Err(FileError::Ignore(format!(
            "{} exceeds the {MAX_IGNORE_BYTES}-byte ignore file limit",
            relative.display()
        )));
    }
    let mut builder = GitignoreBuilder::new(directory);
    for line in content.trim_start_matches('\u{feff}').lines() {
        builder
            .add_line(Some(path.clone()), line)
            .map_err(|error| FileError::Ignore(error.to_string()))?;
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| FileError::Ignore(error.to_string()))
}

#[cfg(unix)]
fn open_file(root: &Path, relative: &Path) -> Result<File, FileError> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let parent = relative.parent().ok_or(FileError::NotFile)?;
    let name = relative.file_name().ok_or(FileError::NotFile)?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory =
        open(root, flags | OFlags::DIRECTORY, Mode::empty()).map_err(io::Error::from)?;
    // Walk open directory descriptors so a symlink substituted after a check
    // cannot redirect a preview outside the root.
    for name in parent {
        directory = openat(&directory, name, flags | OFlags::DIRECTORY, Mode::empty())
            .map_err(io::Error::from)?;
    }
    let file = openat(&directory, name, flags | OFlags::NONBLOCK, Mode::empty())
        .map_err(io::Error::from)?;
    Ok(File::from(file))
}

#[cfg(not(unix))]
fn open_file(root: &Path, relative: &Path) -> Result<File, FileError> {
    let parent = relative.parent().ok_or(FileError::NotFile)?;
    let name = relative.file_name().ok_or(FileError::NotFile)?;
    let mut path = root.to_path_buf();
    for component in parent {
        path.push(component);
        check_directory(&path)?;
    }
    path.push(name);
    if !path.symlink_metadata()?.is_file() {
        return Err(FileError::NotFile);
    }
    Ok(File::open(path)?)
}

fn highlight(path: &Path, content: &str) -> TextFile {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();

    let plain = |line: &str| {
        vec![TextSpan {
            text: line.trim_end_matches(['\r', '\n']).into(),
            color: None,
        }]
    };
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let syntax = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| syntaxes.find_syntax_by_extension(name))
        .or_else(|| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| syntaxes.find_syntax_by_extension(extension))
        });
    let mut highlighter = syntax
        .zip(themes.themes.get("base16-ocean.dark"))
        .filter(|_| {
            content
                .lines()
                .take(MAX_VIEW_LINES)
                .all(|line| line.len() <= MAX_HIGHLIGHT_LINE_BYTES)
        })
        .map(|(syntax, theme)| HighlightLines::new(syntax, theme));
    let mut lines = content.split_inclusive('\n');
    let visible = lines
        .by_ref()
        .take(MAX_VIEW_LINES)
        .map(|line| {
            let Some(highlighter) = highlighter.as_mut() else {
                return plain(line);
            };
            match highlighter.highlight_line(line, syntaxes) {
                Ok(parts) => parts
                    .into_iter()
                    .map(|(style, text)| {
                        let color = style.foreground;
                        TextSpan {
                            text: text.trim_end_matches(['\r', '\n']).into(),
                            color: Some(format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)),
                        }
                    })
                    .collect(),
                Err(error) => {
                    tracing::debug!(%error, "could not highlight a workspace file line");
                    plain(line)
                }
            }
        })
        .collect();
    TextFile {
        lines: visible,
        truncated: lines.next().is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn lists_one_level_and_honors_ancestor_and_local_ignore_rules()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("src/nested"))?;
        fs::create_dir(root.join(".git"))?;
        fs::write(root.join(".gitignore"), "src/build.log\n")?;
        fs::write(root.join(".shepherdignore"), "root-secret.txt\n")?;
        fs::write(root.join("src/.shepherdignore"), "debug.txt\n")?;
        for file in [
            "README.md",
            ".hidden",
            "root-secret.txt",
            "src/main.rs",
            "src/build.log",
            "src/debug.txt",
            "src/nested/deep.rs",
        ] {
            fs::write(root.join(file), "fixture")?;
        }
        let paths = |path: &str, show_dotfiles| -> Result<Vec<_>, FileError> {
            Ok(list_directory(root, Path::new(path), show_dotfiles)?
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect())
        };
        assert_eq!(paths("", false)?, ["src", "README.md"]);
        assert_eq!(paths("src", false)?, ["src/nested", "src/main.rs"]);
        assert_eq!(paths("src/nested", false)?, ["src/nested/deep.rs"]);
        assert_eq!(
            paths("", true)?,
            [
                "src",
                ".gitignore",
                ".hidden",
                ".shepherdignore",
                "README.md"
            ]
        );
        assert!(matches!(
            list_directory(root, Path::new(".GIT"), true),
            Err(FileError::InvalidPath)
        ));
        Ok(())
    }

    #[test]
    fn previews_text_with_highlighting_and_rejects_binary_and_oversized_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::write(
            root.join("main.rs"),
            "fn main() {\r\n    println!(\"héllo\");\r\n}\r\n",
        )?;
        let view = read_file(root, Path::new("main.rs"))?;
        assert_eq!(view.lines.len(), 3);
        assert!(view.lines.iter().flatten().any(|span| span.color.is_some()));
        assert_eq!(
            view.lines
                .first()
                .into_iter()
                .flatten()
                .map(|part| part.text.as_str())
                .collect::<String>(),
            "fn main() {"
        );
        fs::write(root.join("binary.bin"), [0, 255])?;
        assert!(matches!(
            read_file(root, Path::new("binary.bin")),
            Err(FileError::NonText)
        ));
        fs::write(root.join("large.txt"), vec![b'x'; MAX_VIEW_BYTES + 1])?;
        assert!(matches!(
            read_file(root, Path::new("large.txt")),
            Err(FileError::TooLarge)
        ));
        fs::write(root.join("lines.txt"), "\n".repeat(MAX_VIEW_LINES + 1))?;
        let view = read_file(root, Path::new("lines.txt"))?;
        assert!(view.truncated);
        assert_eq!(view.lines.len(), MAX_VIEW_LINES);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn rejects_traversal_and_symlinks_in_any_component() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("workspace");
        fs::create_dir(&root)?;
        let private = directory.path().join("private.txt");
        fs::write(&private, "visible.txt\n")?;
        fs::write(root.join("visible.txt"), "inside the workspace")?;
        std::os::unix::fs::symlink(&private, root.join(".gitignore"))?;
        std::os::unix::fs::symlink(&private, root.join("link.txt"))?;
        std::os::unix::fs::symlink(directory.path(), root.join("parent"))?;
        let listing = list_directory(&root, Path::new(""), true)?;
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["visible.txt"]
        );
        let linked_root = directory.path().join("linked-root");
        std::os::unix::fs::symlink(&root, &linked_root)?;
        assert!(matches!(
            read_file(&linked_root, Path::new("visible.txt")),
            Err(FileError::SymbolicLink)
        ));
        assert!(matches!(
            list_directory(&root, Path::new("parent"), true),
            Err(FileError::SymbolicLink)
        ));
        for path in [
            Path::new("../private.txt"),
            private.as_path(),
            Path::new("link.txt"),
            Path::new("parent/private.txt"),
        ] {
            assert!(read_file(&root, path).is_err(), "{}", path.display());
        }
        Ok(())
    }
}
