//! Custom agent tools beyond the built-in cersei set.

use async_trait::async_trait;
use cersei::tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::Value;

// ─── Ripgrep code search (replaces the built-in Grep tool) ────────────────

/// Matches (per file) and total (global) caps for search results.
const DEFAULT_PER_FILE_RESULTS: usize = 15;
const GLOBAL_RESULT_CAP: usize = 250;
/// Long matching lines are truncated so results stay compact.
const MAX_LINE_LENGTH: usize = 500;

/// Ripgrep-backed search with raw `rg` flag passthrough, per-file and global
/// result caps — the same contract as the freebuff agent's `code_search`
/// tool (`pattern` + raw rg `flags` + `maxResults` per file, global cap of
/// 250). Registered under the `Grep` name so it replaces cersei's built-in
/// Grep tool.
pub struct RgSearchTool;

#[async_trait]
impl Tool for RgSearchTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search for string patterns in the project's files. This tool uses ripgrep (rg), a fast \
         line-oriented search tool. Use it to find where a symbol, string, or pattern appears. \
         Pass raw ripgrep flags in `flags` to customize the search (e.g. \"-i\" for \
         case-insensitive, \"-g *.rs\" to only search Rust files, \"-g !*.test.rs\" to exclude \
         test files, \"-A 3\" / \"-B 2\" for context lines). Results respect .gitignore."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "The regex pattern to search for." },
                "flags": { "type": "string", "description": "Optional raw ripgrep flags to customize the search (e.g. \"-i\", \"-g *.ts -g *.js\", \"-g !*.test.ts\", \"-A 3\", \"-B 2\")." },
                "path": { "type": "string", "description": "Optional file or directory to search in. Defaults to the project root." },
                "max_results": { "type": "integer", "description": "Maximum number of results to return per file. Defaults to 15." }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Input {
            pattern: String,
            flags: Option<String>,
            path: Option<String>,
            max_results: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        // Resolve the search path against the agent's working directory — the
        // rg subprocess runs with the process cwd, not ctx.working_dir.
        let search_path = match input.path {
            Some(path) => {
                let path = std::path::PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    ctx.working_dir.join(path)
                }
            }
            None => ctx.working_dir.clone(),
        };
        let per_file = input.max_results.unwrap_or(DEFAULT_PER_FILE_RESULTS).max(1);

        // Best-effort fallback to system `grep` when ripgrep isn't installed.
        if !rg_available() {
            return grep_fallback(&input.pattern, &search_path, per_file, input.flags.as_deref())
                .await;
        }

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--json")
            .arg("--color=never")
            .arg(format!("--max-count={per_file}"));
        if let Some(flags) = input.flags.filter(|f| !f.trim().is_empty()) {
            cmd.args(flags.split_whitespace());
        }
        cmd.arg("--").arg(&input.pattern).arg(&search_path);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!(
                    "failed to run ripgrep (rg): {e} — is ripgrep installed?"
                ))
            }
        };
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let Some(mut stderr) = stderr else {
                return String::new();
            };
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf).await;
            buf
        });

        // `lines()` must be called as the async trait method — the sync
        // std::io::BufRead::lines also matches here.
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut lines = tokio::io::AsyncBufReadExt::lines(&mut reader);
        let mut matches: Vec<String> = Vec::new();
        let mut capped = false;
        while let Some(line) = lines.next_line().await.unwrap_or(None) {
            if matches.len() >= GLOBAL_RESULT_CAP {
                capped = true;
                let _ = child.kill().await;
                break;
            }
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if event["type"] != "match" {
                continue;
            }
            let file = event["data"]["path"]["text"].as_str().unwrap_or("?");
            let Some(line_number) = event["data"]["line_number"].as_u64() else {
                continue;
            };
            let text = event["data"]["lines"]["text"].as_str().unwrap_or("");
            let text = text.trim_end_matches('\n');
            let truncated: String = if text.chars().count() > MAX_LINE_LENGTH {
                format!(
                    "{}...",
                    text.chars().take(MAX_LINE_LENGTH).collect::<String>()
                )
            } else {
                text.to_string()
            };
            matches.push(format!("{file}:{line_number}: {truncated}"));
        }

        let status = child.wait().await;
        let stderr = stderr_task.await.unwrap_or_default();
        if let Ok(status) = status {
            // rg exits 1 when there are no matches — not an error.
            if !status.success() && status.code() != Some(1) {
                let msg = if stderr.trim().is_empty() {
                    format!("ripgrep exited with {status}")
                } else {
                    stderr.trim().to_string()
                };
                return ToolResult::error(msg);
            }
        }

        if matches.is_empty() {
            return ToolResult::success("No matches found".to_string());
        }

        let mut output = matches.join("\n");
        if capped {
            output.push_str(&format!(
                "\n\n[{GLOBAL_RESULT_CAP} matches limit reached. Use more specific flags or pattern, or read files directly.]"
            ));
        }
        ToolResult::success(output)
    }
}


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

/// Whether ripgrep is on the PATH. If not, searches fall back to system grep.
fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Best-effort `grep -rn` fallback used when ripgrep is missing. Only the
/// `-g <glob>` search flag can be translated (`--include`/`--exclude`); the
/// raw ripgrep flags in `flags` are ignored, so searches still work without
/// rg, just with less power.
async fn grep_fallback(
    pattern: &str,
    search_path: &std::path::Path,
    per_file: usize,
    flags: Option<&str>,
) -> ToolResult {
    let mut cmd = tokio::process::Command::new("grep");
    cmd.arg("-rn").arg("--color=never").arg(format!("-m{per_file}"));
    if let Some(flags) = flags {
        let mut iter = flags.split_whitespace();
        while let Some(flag) = iter.next() {
            if flag == "-g" {
                if let Some(glob) = iter.next() {
                    if let Some(rest) = glob.strip_prefix('!') {
                        cmd.arg("--exclude").arg(rest);
                    } else {
                        cmd.arg("--include").arg(glob);
                    }
                }
            }
        }
    }
    cmd.arg("--").arg(pattern).arg(search_path);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            return ToolResult::error(format!("failed to run grep: {e}"));
        }
    };
    // grep exits 1 when there are no matches — not an error.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ToolResult::error(if stderr.trim().is_empty() {
            format!("grep exited with {}", output.status)
        } else {
            stderr.trim().to_string()
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut matches: Vec<String> = Vec::new();
    let mut capped = false;
    for line in stdout.lines() {
        if matches.len() >= GLOBAL_RESULT_CAP {
            capped = true;
            break;
        }
        // Format: path:line:content
        let mut parts = line.splitn(3, ':');
        let (Some(file), Some(line_number), Some(content)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if file.trim().is_empty() || line_number.parse::<u64>().is_err() {
            continue;
        }
        let truncated: String = if content.chars().count() > MAX_LINE_LENGTH {
            format!(
                "{}...",
                content.chars().take(MAX_LINE_LENGTH).collect::<String>()
            )
        } else {
            content.to_string()
        };
        matches.push(format!("{file}:{line_number}: {truncated}"));
    }

    if matches.is_empty() {
        return ToolResult::success("No matches found".to_string());
    }

    let mut output_text = matches.join("\n");
    if capped {
        output_text.push_str(&format!(
            "\n\n[{GLOBAL_RESULT_CAP} matches limit reached. Use more specific flags or pattern, or read files directly.]"
        ));
    }
    ToolResult::success(output_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cersei::tools::permissions::AllowAll;
    use cersei::tools::CostTracker;
    use std::sync::Arc;

    /// A ToolContext rooted at `working_dir` with an allow-all permission
    /// policy (tools run locally in tests, nothing is executed remotely).
    fn test_context(working_dir: std::path::PathBuf) -> ToolContext {
        ToolContext {
            working_dir,
            session_id: "test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Default::default(),
        }
    }

    fn scratch_dir(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rg-tool-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    async fn run_search(pattern: &str, flags: Option<&str>, dir: &std::path::Path) -> ToolResult {
        let input = serde_json::json!({
            "pattern": pattern,
            "flags": flags,
        });
        RgSearchTool.execute(input, &test_context(dir.to_path_buf())).await
    }

    #[tokio::test]
    async fn grep_returns_matches_with_line_numbers() {
        if !rg_available() {
            eprintln!("skipping: ripgrep not installed");
            return;
        }
        let dir = scratch_dir(&[
            ("a.rs", "fn foo() {}\nfn bar() {}\n"),
            ("b.rs", "fn FOO() {}\n"),
        ]);
        let result = run_search("foo", None, &dir).await;
        assert!(!result.is_error, "search failed: {}", result.content);
        assert!(result.content.contains("a.rs:1: fn foo()"), "got: {}", result.content);
        assert!(!result.content.contains("b.rs"), "case-sensitive match leaked: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_passes_raw_ripgrep_flags() {
        if !rg_available() {
            eprintln!("skipping: ripgrep not installed");
            return;
        }
        let dir = scratch_dir(&[
            ("a.rs", "fn foo() {}\n"),
            ("b.rs", "fn FOO() {}\n"),
            ("c.txt", "foo here\n"),
        ]);
        // Case-insensitive + only Rust files (raw flag passthrough).
        let result = run_search("foo", Some("-i -g *.rs"), &dir).await;
        assert!(!result.is_error, "search failed: {}", result.content);
        assert!(result.content.contains("a.rs:1"), "got: {}", result.content);
        assert!(result.content.contains("b.rs:1"), "got: {}", result.content);
        assert!(!result.content.contains("c.txt"), "glob filter leaked: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_relative_path_resolves_against_working_dir() {
        if !rg_available() {
            eprintln!("skipping: ripgrep not installed");
            return;
        }
        let dir = scratch_dir(&[("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }\n")]);
        // A relative path must resolve against the tool's working dir, not the
        // process cwd (the rg subprocess inherits the process cwd).
        let input = serde_json::json!({ "pattern": "add", "path": "src" });
        let result = RgSearchTool
            .execute(input, &test_context(dir.clone()))
            .await;
        assert!(!result.is_error, "search failed: {}", result.content);
        assert!(
            result.content.contains("lib.rs:1"),
            "relative path not resolved against working dir: {}",
            result.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_fallback_uses_system_grep() {
        let dir = scratch_dir(&[
            ("a.rs", "fn foo() {}\n"),
            ("b.rs", "fn bar() {}\n"),
        ]);
        let result = grep_fallback("foo", &dir, DEFAULT_PER_FILE_RESULTS, None).await;
        assert!(!result.is_error, "fallback failed: {}", result.content);
        assert!(result.content.contains("a.rs:1"), "got: {}", result.content);
        assert!(!result.content.contains("b.rs"), "got: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_fallback_translates_glob_flags() {
        let dir = scratch_dir(&[
            ("a.rs", "fn foo() {}\n"),
            ("b.txt", "foo here\n"),
        ]);
        let result = grep_fallback("foo", &dir, DEFAULT_PER_FILE_RESULTS, Some("-g *.rs")).await;
        assert!(!result.is_error, "fallback failed: {}", result.content);
        assert!(result.content.contains("a.rs:1"), "got: {}", result.content);
        assert!(!result.content.contains("b.txt"), "glob filter leaked: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_no_matches_is_not_an_error() {
        if !rg_available() {
            eprintln!("skipping: ripgrep not installed");
            return;
        }
        let dir = scratch_dir(&[("a.rs", "fn foo() {}\n")]);
        let result = run_search("nonexistent-symbol", None, &dir).await;
        assert!(!result.is_error, "no-match must not error: {}", result.content);
        assert!(result.content.contains("No matches"), "got: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
