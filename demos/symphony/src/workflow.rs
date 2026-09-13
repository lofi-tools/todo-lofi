use crate::domain::WorkflowDefinition;
use crate::error::SymphonyError::*;
use crate::error::Result;
use crate::prompt::Template;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Default prompt used when the workflow body is empty (Section 5.4).
pub const DEFAULT_PROMPT: &str = "You are working on an issue from Linear.";

/// Default workflow file name resolved from the process working directory.
pub const DEFAULT_WORKFLOW_FILE: &str = "WORKFLOW.md";

/// Select the workflow file path: an explicit runtime path wins, otherwise the
/// current working directory default (Section 5.1).
pub fn select_workflow_path(explicit: Option<&Path>) -> PathBuf {
    match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(DEFAULT_WORKFLOW_FILE),
    }
}

/// Load and parse a WORKFLOW.md file.
pub fn load_workflow<P: AsRef<Path>>(path: P) -> Result<WorkflowDefinition> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).map_err(|_error| MissingWorkflowFile {
        path: path.to_string_lossy().into_owned(),
    })?;

    parse_workflow(&content)
}

/// Parse the workflow markdown body into a strict prompt template.
pub fn parse_prompt_template(body: &str) -> Result<Template> {
    let body = if body.trim().is_empty() {
        DEFAULT_PROMPT
    } else {
        body
    };
    crate::prompt::parse(body)
}

/// Parse workflow content from a string.
pub fn parse_workflow(content: &str) -> Result<WorkflowDefinition> {
    if let Some(after_first) = content.strip_prefix("---") {
        // Only treat `---` as a front-matter delimiter when it starts a line on its own.
        let after_first = after_first
            .strip_prefix("\r\n")
            .or_else(|| after_first.strip_prefix('\n'));
        let Some(after_first) = after_first else {
            return Ok(WorkflowDefinition {
                config: HashMap::new(),
                prompt_template: content.trim().to_string(),
            });
        };

        let mut front_matter = Vec::new();
        let mut body_lines: Vec<&str> = Vec::new();
        let mut in_front_matter = true;

        for line in after_first.lines() {
            if in_front_matter && line.trim_end() == "---" {
                in_front_matter = false;
                continue;
            }
            if in_front_matter {
                front_matter.push(line);
            } else {
                body_lines.push(line);
            }
        }

        if in_front_matter {
            return Err(WorkflowParseError {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "No closing YAML front matter delimiter",
                )),
            });
        }

        let yaml_content = front_matter.join("\n");
        let config: serde_yaml::Value =
            serde_yaml::from_str(&yaml_content).map_err(|error| WorkflowParseError {
                source: Box::new(error),
            })?;

        // YAML front matter MUST decode to a map/object.
        let Some(mapping) = config.as_mapping() else {
            return Err(WorkflowFrontMatterNotAMap);
        };

        let config = mapping
            .iter()
            .filter_map(|(key, value)| key.as_str().map(|key| (key.to_string(), value.clone())))
            .collect();

        Ok(WorkflowDefinition {
            config,
            prompt_template: body_lines.join("\n").trim().to_string(),
        })
    } else {
        Ok(WorkflowDefinition {
            config: HashMap::new(),
            prompt_template: content.trim().to_string(),
        })
    }
}

/// Detects `WORKFLOW.md` modifications so the service can reload without restart
/// (Section 6.2).
#[derive(Debug, Clone)]
pub struct WorkflowWatcher {
    path: PathBuf,
    last_seen: Option<FileStamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    length: u64,
}

impl FileStamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            length: metadata.len(),
        })
    }
}

impl WorkflowWatcher {
    /// Create a watcher for a workflow path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            last_seen: FileStamp::read(&path),
            path,
        }
    }

    /// The watched path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record the current file state without reporting a change.
    ///
    /// Used after a successful load so the first poll does not re-report the
    /// contents that were just applied.
    pub fn mark_current(&mut self) {
        self.last_seen = FileStamp::read(&self.path);
    }

    /// Return the new workflow contents when the file changed since the last poll.
    ///
    /// The stamp is advanced even when the reload fails, so a broken file is
    /// reported once instead of on every tick; the caller keeps operating on the
    /// last known good configuration.
    pub fn poll(&mut self) -> Option<Result<WorkflowDefinition>> {
        let current = FileStamp::read(&self.path);
        if current == self.last_seen {
            return None;
        }
        self.last_seen = current;

        match current {
            // The file disappeared: report it as a missing workflow file.
            None => Some(Err(MissingWorkflowFile {
                path: self.path.to_string_lossy().into_owned(),
            })),
            Some(_) => Some(load_workflow(&self.path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SymphonyError;
    use tempfile::TempDir;

    #[test]
    fn test_parse_workflow_with_front_matter() {
        let content = r#"---
tracker:
  kind: linear
  project_slug: test-project
polling:
  interval_ms: 30000
---

# Test Prompt

Working on issue: {{ issue.identifier }}
"#;

        let workflow = parse_workflow(content).expect("workflow parses");

        assert_eq!(workflow.config.len(), 2);
        assert!(workflow.config.contains_key("tracker"));
        assert!(workflow.config.contains_key("polling"));

        let expected = r#"# Test Prompt

Working on issue: {{ issue.identifier }}"#;
        assert_eq!(workflow.prompt_template, expected);
    }

    #[test]
    fn test_parse_workflow_without_front_matter() {
        let content = r#"

# Test Prompt

Working on issue: {{ issue.identifier }}

"#;

        let workflow = parse_workflow(content).expect("workflow parses");

        assert!(workflow.config.is_empty());
        let expected = r#"# Test Prompt

Working on issue: {{ issue.identifier }}"#;
        assert_eq!(workflow.prompt_template, expected);
    }

    #[test]
    fn test_parse_workflow_invalid_yaml() {
        let content = r#"---
tracker: [invalid, yaml: here
---

content
"#;

        assert!(matches!(
            parse_workflow(content),
            Err(WorkflowParseError { .. })
        ));
    }

    #[test]
    fn test_parse_workflow_front_matter_not_map() {
        let content = r#"---
- item1
- item2
---

content
"#;

        assert!(matches!(
            parse_workflow(content),
            Err(WorkflowFrontMatterNotAMap)
        ));
    }

    #[test]
    fn test_parse_workflow_missing_closing_delimiter() {
        let content = "---\ntracker:\n  kind: linear\n";
        assert!(matches!(
            parse_workflow(content),
            Err(WorkflowParseError { .. })
        ));
    }

    #[test]
    fn test_missing_workflow_file_is_typed_error() {
        let directory = TempDir::new().expect("temp dir");
        let missing = directory.path().join("nope.md");
        assert!(matches!(
            load_workflow(&missing),
            Err(SymphonyError::MissingWorkflowFile { .. })
        ));
    }

    #[test]
    fn test_empty_prompt_body_uses_default_prompt() {
        let template = parse_prompt_template("").expect("default prompt parses");
        let rendered = crate::prompt::render(&template, &serde_json::json!({}))
            .expect("default prompt renders");
        assert_eq!(rendered, DEFAULT_PROMPT);
    }

    #[test]
    fn test_watcher_detects_change() {
        let directory = TempDir::new().expect("temp dir");
        let path = directory.path().join("WORKFLOW.md");
        fs::write(&path, "# first\n").expect("write");

        let mut watcher = WorkflowWatcher::new(path.clone());
        assert!(watcher.poll().is_none());

        fs::write(&path, "# second longer body\n").expect("write");
        let changed = watcher.poll().expect("change detected");
        let workflow = changed.expect("workflow parses");
        assert_eq!(workflow.prompt_template, "# second longer body");

        // The reload is reported once.
        assert!(watcher.poll().is_none());
    }

    #[test]
    fn test_watcher_reports_invalid_reload_once() {
        let directory = TempDir::new().expect("temp dir");
        let path = directory.path().join("WORKFLOW.md");
        fs::write(&path, "# first\n").expect("write");

        let mut watcher = WorkflowWatcher::new(path.clone());
        assert!(watcher.poll().is_none());

        fs::write(&path, "---\n- not a map\n---\nbody\n").expect("write");
        let changed = watcher.poll().expect("change detected");
        assert!(matches!(
            changed,
            Err(SymphonyError::WorkflowFrontMatterNotAMap)
        ));
        assert!(watcher.poll().is_none());
    }
}
