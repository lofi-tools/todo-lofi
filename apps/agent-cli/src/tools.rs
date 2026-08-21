//! Custom agent tools beyond the built-in cersei set.

use async_trait::async_trait;
use cersei::tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::Value;

/// Fetch up-to-date library/framework documentation via the Context7 API —
/// the same docs source the freebuff agent's `read_docs` tool uses.
pub struct ReadDocsTool;

#[async_trait]
impl Tool for ReadDocsTool {
    fn name(&self) -> &str {
        "ReadDocs"
    }

    fn description(&self) -> &str {
        "Fetch up-to-date documentation for libraries and frameworks using the Context7 API. \
         Use this to get current docs (APIs, options, examples) instead of guessing from memory."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "library_title": {
                    "type": "string",
                    "description": "The library or framework name (e.g., \"Next.js\", \"MongoDB\", \"React\"). Use the official name as it appears in documentation if possible. Only public libraries available in Context7's database are supported."
                },
                "topic": {
                    "type": "string",
                    "description": "Specific topic to focus on (e.g., \"routing\", \"hooks\", \"authentication\")"
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Maximum number of tokens to return. Defaults to 10000."
                }
            },
            "required": ["library_title"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Input {
            library_title: String,
            topic: Option<String>,
            max_tokens: Option<u32>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let max_tokens = input.max_tokens.unwrap_or(10_000);

        let client = reqwest::Client::new();

        // Resolve the library title to a Context7 library id (e.g. "/react/react").
        let search: Value = match client
            .get("https://context7.com/api/v1/search")
            .query(&[("query", input.library_title.as_str())])
            .send()
            .await
        {
            Ok(resp) => match resp.json().await {
                Ok(v) => v,
                Err(e) => return ToolResult::error(format!("Context7 search failed: {e}")),
            },
            Err(e) => return ToolResult::error(format!("Context7 search failed: {e}")),
        };
        let Some(id) = search["results"][0]["id"].as_str() else {
            return ToolResult::error(format!(
                "no library found matching '{}'",
                input.library_title
            ));
        };

        // Fetch the docs for that library, filtered to the requested topic.
        let mut docs_request = client.get(format!(
            "https://context7.com/api/v1/{}",
            id.trim_start_matches('/')
        ));
        if let Some(topic) = input.topic.filter(|t| !t.trim().is_empty()) {
            docs_request = docs_request.query(&[("topic", topic.as_str())]);
        }
        let docs_request = docs_request.query(&[("tokens", max_tokens.to_string().as_str())]);

        match docs_request.send().await {
            Ok(resp) => match resp.text().await {
                Ok(text) => {
                    if text.trim().is_empty() {
                        ToolResult::error(format!(
                            "no docs returned for '{}'",
                            input.library_title
                        ))
                    } else {
                        ToolResult::success(text)
                    }
                }
                Err(e) => ToolResult::error(format!("Context7 docs request failed: {e}")),
            },
            Err(e) => ToolResult::error(format!("Context7 docs request failed: {e}")),
        }
    }
}
