use crate::domain::WorkflowDefinition;
use crate::error::SymphonyError::*;
use serde_yaml;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Load and parse a WORKFLOW.md file.
pub fn load_workflow<P: AsRef<Path>>(path: P) -> crate::error::Result<WorkflowDefinition> {
    let content = fs::read_to_string(&path).map_err(|_e| MissingWorkflowFile {
        path: path.as_ref().to_string_lossy().into_owned(),
    })?;

    parse_workflow(&content)
}

/// Parse workflow content from a string.
pub fn parse_workflow(content: &str) -> crate::error::Result<WorkflowDefinition> {
    // Check if content starts with YAML front matter
    if let Some(after_first) = content.strip_prefix("---") {
        let after_first_nl = after_first.strip_prefix('\n').unwrap_or(after_first);
        let offset = content.len() - after_first_nl.len();

        let mut yaml_lines = Vec::new();
        let mut found_end = false;
        let mut body_start = offset;

        for line in after_first_nl.lines() {
            if line.trim() == "---" {
                found_end = true;
                body_start += line.len() + 1; // skip past this line + newline
                break;
            }
            yaml_lines.push(line);
            body_start += line.len() + 1;
        }

        if !found_end {
            // No closing --- found, treat as no front matter
            return Err(WorkflowParseError {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "No closing YAML front matter delimiter",
                )),
            });
        }

        let yaml_content = yaml_lines.join("\n");
        let config: serde_yaml::Value =
            serde_yaml::from_str(&yaml_content).map_err(|e| WorkflowParseError {
                source: Box::new(e),
            })?;

        // Config must be a map/object
        if !config.is_mapping() {
            return Err(WorkflowFrontMatterNotAMap);
        }

        // The rest is the prompt template
        let prompt_template = if body_start < content.len() {
            content[body_start..].trim().to_string()
        } else {
            String::new()
        };

        Ok(WorkflowDefinition {
            config: config
                .as_mapping()
                .unwrap()
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|s| (s.to_string(), v.clone())))
                .collect(),
            prompt_template,
        })
    } else {
        // No front matter, entire content is prompt
        Ok(WorkflowDefinition {
            config: HashMap::new(),
            prompt_template: content.trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let workflow = parse_workflow(content).unwrap();

        // Check config was parsed
        assert_eq!(workflow.config.len(), 2);
        assert!(workflow.config.contains_key("tracker"));
        assert!(workflow.config.contains_key("polling"));

        // Check prompt template
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

        let workflow = parse_workflow(content).unwrap();

        // Check config is empty
        assert!(workflow.config.is_empty());

        // Check prompt template
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

        let result = parse_workflow(content);
        assert!(matches!(result, Err(WorkflowParseError { .. })));
    }

    #[test]
    fn test_parse_workflow_front_matter_not_map() {
        let content = r#"---
- item1
- item2
---

content
"#;

        let result = parse_workflow(content);
        assert!(matches!(result, Err(WorkflowFrontMatterNotAMap)));
    }
}
