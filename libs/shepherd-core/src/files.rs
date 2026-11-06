use std::fs::{self, DirEntry, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

const MAX_DIRECTORY_ENTRIES: usize = 5_000;
const MAX_IGNORE_BYTES: u64 = 1024 * 1024;
const MAX_VIEW_BYTES: usize = 256 * 1024;
const MAX_VIEW_LINES: usize = 4_000;
const MAX_HIGHLIGHT_LINE_BYTES: usize = 4_096;
const MAX_SEARCH_ENTRIES: usize = 20_000;
const MAX_SEARCH_BYTES: usize = 32 * 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 200;
const MAX_SEARCH_DEPTH: usize = 128;
const MAX_QUERY_BYTES: usize = 512;

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
    #[error("The search query must contain 1 to {MAX_QUERY_BYTES} bytes.")]
    InvalidQuery,
    #[error("File search cancelled.")]
    Cancelled,
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
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    first_line: usize,
    lines: Vec<Vec<TextSpan>>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct TextSpan {
    text: String,
    color: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileSearchMode {
    Name,
    Contents,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchQuery {
    mode: FileSearchMode,
    query: String,
    show_dotfiles: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct FileSearchResult {
    matches: Vec<FileMatch>,
    incomplete: bool,
}

#[derive(Debug, Serialize)]
struct FileMatch {
    path: String,
    line: Option<usize>,
    excerpt: Option<String>,
}

type IgnoreRules = [Vec<Gitignore>; 2];

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
        load_rules(&root, &directory, &mut rules)?;
        let Some(name) = ancestors.next() else { break };
        directory.push(name);
        if (!show_dotfiles && is_dotfile(name)) || is_ignored(&rules, &directory, true) {
            return Err(FileError::HiddenDirectory);
        }
        check_directory(&directory)?;
    }

    let mut entries = Vec::new();
    let mut incomplete = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        let Some(is_directory) = visible_entry(&entry, &name, &path, &rules, show_dotfiles)? else {
            continue;
        };
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
            directory: is_directory,
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

pub fn read_file(
    root: &Path,
    relative: &Path,
    focus_line: Option<usize>,
) -> Result<TextFile, FileError> {
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
    Ok(highlight(&relative, content, focus_line))
}

pub fn search_files(
    root: &Path,
    query: FileSearchQuery,
    cancelled: &AtomicBool,
) -> Result<FileSearchResult, FileError> {
    let text = query.query.trim();
    if text.is_empty() || text.len() > MAX_QUERY_BYTES {
        return Err(FileError::InvalidQuery);
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(FileError::Cancelled);
    }
    check_directory(root)?;
    let root = root.canonicalize()?;
    let mut search = Search {
        root: &root,
        mode: query.mode,
        text: text.to_lowercase(),
        show_dotfiles: query.show_dotfiles,
        cancelled,
        visited: 0,
        remaining_bytes: MAX_SEARCH_BYTES,
        result: FileSearchResult::default(),
    };
    search.directory(&root, &mut Default::default(), 0)?;
    search.check_cancelled()?;
    search.result.matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    Ok(search.result)
}

struct Search<'a> {
    root: &'a Path,
    mode: FileSearchMode,
    text: String,
    show_dotfiles: bool,
    cancelled: &'a AtomicBool,
    visited: usize,
    remaining_bytes: usize,
    result: FileSearchResult,
}

impl Search<'_> {
    fn check_cancelled(&self) -> Result<(), FileError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(FileError::Cancelled);
        }
        Ok(())
    }

    fn at_limit(&self) -> bool {
        self.visited == MAX_SEARCH_ENTRIES
            || self.remaining_bytes == 0
            || self.result.matches.len() == MAX_SEARCH_MATCHES
    }

    fn directory(
        &mut self,
        directory: &Path,
        rules: &mut IgnoreRules,
        depth: usize,
    ) -> Result<(), FileError> {
        self.check_cancelled()?;
        if depth == MAX_SEARCH_DEPTH {
            self.result.incomplete = true;
            return Ok(());
        }
        check_directory(directory)?;
        let [shepherd, git] = rules.each_ref().map(Vec::len);
        let outcome = (|| {
            load_rules(self.root, directory, rules)?;
            for entry in fs::read_dir(directory)? {
                self.check_cancelled()?;
                if self.at_limit() {
                    self.result.incomplete = true;
                    break;
                }
                self.visited += 1;
                match entry {
                    Ok(entry) => match self.entry(&entry, rules, depth) {
                        Err(FileError::Cancelled) => return Err(FileError::Cancelled),
                        Err(_) => self.result.incomplete = true,
                        Ok(()) => {}
                    },
                    Err(_) => self.result.incomplete = true,
                }
            }
            Ok(())
        })();
        let [shepherd_rules, git_rules] = rules;
        shepherd_rules.truncate(shepherd);
        git_rules.truncate(git);
        outcome
    }

    fn entry(
        &mut self,
        entry: &DirEntry,
        rules: &mut IgnoreRules,
        depth: usize,
    ) -> Result<(), FileError> {
        let path = entry.path();
        let name = entry.file_name();
        match visible_entry(entry, &name, &path, rules, self.show_dotfiles)? {
            Some(true) => self.directory(&path, rules, depth + 1),
            Some(false) => {
                let relative = path
                    .strip_prefix(self.root)
                    .map_err(|_| FileError::InvalidPath)?;
                let Some((name, relative)) = name.to_str().zip(relative.to_str()) else {
                    self.result.incomplete = true;
                    return Ok(());
                };
                match self.mode {
                    FileSearchMode::Name if name.to_lowercase().contains(&self.text) => {
                        self.result.matches.push(FileMatch {
                            path: relative.into(),
                            line: None,
                            excerpt: None,
                        });
                    }
                    FileSearchMode::Contents => self.file(relative)?,
                    FileSearchMode::Name => {}
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn file(&mut self, relative: &str) -> Result<(), FileError> {
        let file = open_file(self.root, Path::new(relative))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Ok(());
        }
        let limit = MAX_VIEW_BYTES.min(self.remaining_bytes);
        if metadata.len() > limit as u64 {
            self.result.incomplete = true;
            return Ok(());
        }
        let mut bytes = Vec::new();
        file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
        self.remaining_bytes = self.remaining_bytes.saturating_sub(bytes.len());
        if bytes.len() > limit {
            self.result.incomplete = true;
            return Ok(());
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            return Ok(());
        };
        if content.contains('\0') {
            return Ok(());
        }
        for (index, line) in content.lines().enumerate() {
            self.check_cancelled()?;
            let lowercase = line.to_lowercase();
            let Some(offset) = lowercase.find(&self.text) else {
                continue;
            };
            if self.result.matches.len() == MAX_SEARCH_MATCHES {
                self.result.incomplete = true;
                break;
            }
            let start = lowercase[..offset].chars().count().saturating_sub(60);
            let excerpt = line.chars().skip(start).take(200).collect::<String>();
            self.result.matches.push(FileMatch {
                path: relative.into(),
                line: Some(index + 1),
                excerpt: Some(excerpt.trim().into()),
            });
        }
        Ok(())
    }
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

fn visible_entry(
    entry: &DirEntry,
    name: &std::ffi::OsStr,
    path: &Path,
    rules: &IgnoreRules,
    show_dotfiles: bool,
) -> io::Result<Option<bool>> {
    if is_git_metadata(name) || (!show_dotfiles && is_dotfile(name)) {
        return Ok(None);
    }
    let kind = entry.file_type()?;
    let directory = kind.is_dir();
    Ok(((directory || kind.is_file()) && !is_ignored(rules, path, directory)).then_some(directory))
}

fn load_rules(root: &Path, directory: &Path, rules: &mut IgnoreRules) -> Result<(), FileError> {
    for (name, matchers) in [".shepherdignore", ".gitignore"].into_iter().zip(rules) {
        if let Some(matcher) = read_ignore(root, directory, name)? {
            matchers.push(matcher);
        }
    }
    Ok(())
}

fn is_ignored(rules: &IgnoreRules, path: &Path, directory: bool) -> bool {
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

fn highlight(path: &Path, content: &str, focus_line: Option<usize>) -> TextFile {
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
    let first_line = focus_line
        .filter(|line| *line > MAX_VIEW_LINES)
        .map_or(1, |line| {
            line.min(content.lines().count()).saturating_sub(40).max(1)
        });
    let mut lines = content.split_inclusive('\n').skip(first_line - 1);
    let mut highlighter = syntax
        .zip(themes.themes.get("base16-ocean.dark"))
        .filter(|_| {
            lines
                .clone()
                .take(MAX_VIEW_LINES)
                .all(|line| line.len() <= MAX_HIGHLIGHT_LINE_BYTES)
        })
        .map(|(syntax, theme)| HighlightLines::new(syntax, theme));
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
        first_line,
        lines: visible,
        truncated: first_line > 1 || lines.next().is_some(),
    }
}

#[cfg(test)]
mod tests {
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
        let view = read_file(root, Path::new("main.rs"), None)?;
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
            read_file(root, Path::new("binary.bin"), None),
            Err(FileError::NonText)
        ));
        fs::write(root.join("large.txt"), vec![b'x'; MAX_VIEW_BYTES + 1])?;
        assert!(matches!(
            read_file(root, Path::new("large.txt"), None),
            Err(FileError::TooLarge)
        ));
        fs::write(root.join("lines.txt"), "\n".repeat(MAX_VIEW_LINES + 1))?;
        let view = read_file(root, Path::new("lines.txt"), None)?;
        assert!(view.truncated);
        assert_eq!(view.lines.len(), MAX_VIEW_LINES);
        Ok(())
    }

    #[test]
    fn searches_names_and_utf8_lines_with_the_same_workspace_rules()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("src/deep"))?;
        fs::create_dir(root.join(".git"))?;
        fs::write(root.join(".gitignore"), "src/ignored.txt\n")?;
        fs::write(root.join("src/.shepherdignore"), "Debug*\n")?;
        fs::write(root.join("src/deep/Needle.rs"), "header\nA Néedle result\n")?;
        fs::write(root.join("src/ignored.txt"), "Néedle ignored")?;
        fs::write(root.join("src/DebugNeedle.txt"), "Néedle ignored")?;
        fs::write(root.join(".hidden-Needle"), "Néedle hidden")?;
        fs::write(root.join(".git/Needle"), "Néedle metadata")?;
        fs::write(root.join("large.txt"), vec![b'x'; MAX_VIEW_BYTES + 1])?;
        fs::write(root.join("binary.bin"), "Néedle\0")?;
        let cancelled = AtomicBool::new(false);
        let search = |mode, query: &str, show_dotfiles| {
            search_files(
                root,
                FileSearchQuery {
                    mode,
                    query: query.into(),
                    show_dotfiles,
                },
                &cancelled,
            )
        };
        let names = search(FileSearchMode::Name, " needle ", false)?;
        assert_eq!(
            names
                .matches
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["src/deep/Needle.rs"]
        );
        assert_eq!(
            search(FileSearchMode::Name, "needle", true)?.matches.len(),
            2
        );
        let contents = search(FileSearchMode::Contents, "NÉEDLE", false)?;
        let hit = contents.matches.first().ok_or("expected a content match")?;
        assert_eq!(contents.matches.len(), 1);
        assert_eq!(hit.path, "src/deep/Needle.rs");
        assert_eq!(hit.line, Some(2));
        assert_eq!(hit.excerpt.as_deref(), Some("A Néedle result"));
        assert!(contents.incomplete);
        cancelled.store(true, Ordering::Relaxed);
        assert!(matches!(
            search(FileSearchMode::Contents, "Néedle", false),
            Err(FileError::Cancelled)
        ));
        Ok(())
    }

    #[test]
    fn limits_search_hits_and_can_preview_a_match_past_the_initial_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::write(
            root.join("long.txt"),
            format!(
                "{}{}",
                "\n".repeat(MAX_VIEW_LINES + 5),
                "match\n".repeat(MAX_SEARCH_MATCHES + 1)
            ),
        )?;
        let results = search_files(
            root,
            FileSearchQuery {
                mode: FileSearchMode::Contents,
                query: "match".into(),
                show_dotfiles: false,
            },
            &AtomicBool::new(false),
        )?;
        assert_eq!(results.matches.len(), MAX_SEARCH_MATCHES);
        assert!(results.incomplete);
        let line = results
            .matches
            .first()
            .and_then(|hit| hit.line)
            .ok_or("expected a line number")?;
        assert_eq!(line, MAX_VIEW_LINES + 6);
        let preview = read_file(root, Path::new("long.txt"), Some(line))?;
        assert!(preview.first_line > 1 && preview.truncated);
        let source = preview
            .lines
            .get(line - preview.first_line)
            .ok_or("match is inside the preview")?;
        assert_eq!(
            source
                .iter()
                .map(|part| part.text.as_str())
                .collect::<String>(),
            "match"
        );
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
            read_file(&linked_root, Path::new("visible.txt"), None),
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
            assert!(read_file(&root, path, None).is_err(), "{}", path.display());
        }
        Ok(())
    }
}
