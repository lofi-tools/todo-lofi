//! Custom agent tools beyond the built-in cersei set.

use async_trait::async_trait;
use cersei::tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::Value;
use std::path::Path;

use crate::subagents::AcpFsSink;

// ─── AskUser tool ──────────────────────────────────────────────────────────

/// `ask_user` is the first-class tool the interviewing agent calls to pose
/// clarifying questions during an `/interview` flow. Questions are always
/// answered with freeform text; `suggestions` are optional hints for the user.
/// The same schema works across TUI and ACP.
pub struct AskUserTool {
    /// Sender for piping questions to the TUI. When `None`, the tool operates
    /// in non-interactive mode (ACP/headless) and returns a structured result.
    ask_user_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::providers::AskUserRequest>>,
    /// Receiver for answers (wrapped in Tokio Mutex for async access, Arc so
    /// multiple concurrently-built agents can share one channel; answers are
    /// matched to the right caller by `request_id`).
    answer_rx: Option<
        std::sync::Arc<
            tokio::sync::Mutex<
                tokio::sync::mpsc::UnboundedReceiver<crate::providers::AskUserAnswer>,
            >,
        >,
    >,
    /// Monotonic counter for request ids.
    request_counter: std::sync::atomic::AtomicU64,
}

impl AskUserTool {
    pub fn new() -> Self {
        Self {
            ask_user_tx: None,
            answer_rx: None,
            request_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn with_channel(
        tx: tokio::sync::mpsc::UnboundedSender<crate::providers::AskUserRequest>,
        rx: std::sync::Arc<
            tokio::sync::Mutex<
                tokio::sync::mpsc::UnboundedReceiver<crate::providers::AskUserAnswer>,
            >,
        >,
    ) -> Self {
        Self {
            ask_user_tx: Some(tx),
            answer_rx: Some(rx),
            request_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user one or more clarifying questions. Answers are always entered \
         as freeform text. You may provide a suggestions array with example answers, \
         but the user can always enter something different. Use this tool when you \
         need clarification — never ask questions as plain text."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "One or more questions to ask the user.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "The question text." },
                            "header": { "type": "string", "description": "Short label, <= 12 chars (optional)." },
                            "suggestions": {
                                "type": "array",
                                "description": "Optional suggested answers. The user can always enter a different freeform answer.",
                                "items": { "type": "string" }
                            },
                            "validation": {
                                "type": "object",
                                "description": "Optional free-text validation constraints.",
                                "properties": {
                                    "maxLength": { "type": "integer" },
                                    "minLength": { "type": "integer" },
                                    "pattern": { "type": "string" },
                                    "patternError": { "type": "string" }
                                }
                            }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct Question {
            question: String,
            header: Option<String>,
            suggestions: Option<Vec<String>>,
            validation: Option<Validation>,
        }
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct Validation {
            #[serde(rename = "maxLength")]
            max_length: Option<u32>,
            #[serde(rename = "minLength")]
            min_length: Option<u32>,
            pattern: Option<String>,
            #[serde(rename = "patternError")]
            pattern_error: Option<String>,
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct ParsedInput {
            questions: Vec<Question>,
        }

        let parsed: ParsedInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        if parsed.questions.is_empty() {
            return ToolResult::error("ask_user requires at least one question".to_string());
        }

        let questions_json: Vec<serde_json::Value> = parsed
            .questions
            .iter()
            .map(|q| match serde_json::to_value(q) {
                Ok(value) => value,
                Err(error) => serde_json::json!({
                    "question": q.question,
                    "error": format!("failed to serialize question: {error}"),
                }),
            })
            .collect();

        let q_count = parsed.questions.len();

        if self.ask_user_tx.is_some() && self.answer_rx.is_some() {
            // Send the questions to the TUI and await an answer.
            let request_id = self
                .request_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let request = crate::providers::AskUserRequest {
                request_id,
                // The ACP elicitation path needs to know which session asked.
                session_id: Some(ctx.session_id.clone()),
                questions: questions_json.clone(),
            };

            if self.ask_user_tx.as_ref().unwrap().send(request).is_err() {
                return ToolResult::error(
                    "ask_user channel closed — cannot await user answer".to_string(),
                );
            }

            // Wait indefinitely for the user's answer. This is deliberately
            // not wrapped in a timeout: an unanswered question must keep the
            // conversation paused for as long as the user needs.
            let mut rx_guard = self.answer_rx.as_ref().unwrap().lock().await;
            loop {
                match rx_guard.recv().await {
                    Some(answer) if answer.request_id == request_id => {
                        let answer_text = format_ask_user_answer(&questions_json, &answer.answers);
                        return ToolResult::success(answer_text);
                    }
                    Some(_) => {
                        // Stale answer, keep waiting.
                        continue;
                    }
                    None => {
                        return ToolResult::error("ask_user answer channel closed".to_string());
                    }
                }
            }
        }

        // No channel: return the questions as a structured result for
        // non-interactive modes (headless runs, and ACP clients that did not
        // advertise elicitation form support).
        ToolResult::success(format!(
            "[ask_user: {q_count} question{qs}]\n{q_labels}",
            qs = if q_count == 1 { "" } else { "s" },
            q_labels = questions_json
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let question = q["question"].as_str().unwrap_or("?");
                    let suggestions = q["suggestions"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str())
                                .enumerate()
                                .map(|(index, suggestion)| format!("{}. {}", index + 1, suggestion))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .filter(|items| !items.is_empty())
                        .map(|items| format!("\n{items}"))
                        .unwrap_or_default();
                    format!("{})) {}{}", i + 1, question, suggestions)
                })
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// Format the questions and answers as a readable QA block.
fn format_ask_user_answer(
    questions_json: &[serde_json::Value],
    answers: &[Option<crate::providers::AskUserAnswerValue>],
) -> String {
    let mut out = String::new();
    out.push_str("## Clarifying Questions\n\n");
    for (i, (q, a)) in questions_json.iter().zip(answers.iter()).enumerate() {
        let question = q["question"].as_str().unwrap_or("?");
        out.push_str(&format!("### {}\n", i + 1));
        out.push_str(&format!("**Q:** {}\n", question));
        match a {
            Some(crate::providers::AskUserAnswerValue::OtherText(text)) => {
                out.push_str(&format!("**A:** {}\n", text));
            }
            None => {
                out.push_str("**A:** Skipped\n");
            }
        }
        out.push('\n');
    }
    out
}

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
            return grep_fallback(
                &input.pattern,
                &search_path,
                per_file,
                input.flags.as_deref(),
            )
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
                ));
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

// ─── Client-buffer-aware Read (replaces the built-in Read tool) ──────────────

/// `Read` tool that consults the ACP client's filesystem first, so unsaved
/// editor buffers are visible to the agent. When the run is backed by an ACP
/// client that advertised `fs.readTextFile`, an absolute `file_path` is read
/// via `fs/read_text_file` (which returns the client's live buffer, including
/// un-saved edits). Anything else (no client, capability off, relative path,
/// or a client error) falls back to cersei's built-in [`FileReadTool`], which
/// reads the on-disk file with the same `cat -n` formatting.
///
/// Registered under the `Read` name so it replaces the built-in Read tool,
/// matching the existing pattern used for `Grep` -> [`RgSearchTool`].
pub struct ClientReadTool {
    fs: AcpFsSink,
    fallback: cersei::tools::file_read::FileReadTool,
}

impl ClientReadTool {
    pub fn new(fs: AcpFsSink) -> Self {
        Self {
            fs,
            fallback: cersei::tools::file_read::FileReadTool,
        }
    }
}

#[async_trait]
impl Tool for ClientReadTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem."
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
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "offset": { "type": "integer", "description": "Line number to start reading from" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        // Only intercept absolute paths when a client fs bridge is configured
        // and has advertised the capability. Everything else — including any
        // client error — falls back to the on-disk read so the tool never
        // fails just because the editor doesn't have the file open.
        let intercept = self
            .fs
            .as_ref()
            .is_some_and(|r| r.supports_read_text_file())
            && input
                .get("file_path")
                .and_then(Value::as_str)
                .is_some_and(|p| Path::new(p).is_absolute());
        if !intercept {
            return self.fallback.execute(input, ctx).await;
        }

        #[derive(serde::Deserialize)]
        struct Input {
            file_path: String,
            offset: Option<usize>,
            limit: Option<usize>,
        }
        let parsed: Input = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            // Malformed input: defer to the built-in tool's own error message.
            Err(_) => return self.fallback.execute(input, ctx).await,
        };

        let Some(fs) = self.fs.as_ref() else {
            return self.fallback.execute(input, ctx).await;
        };
        // Fetch the full live buffer (no windowing) so the local `cat -n`
        // formatting matches the on-disk path's output exactly.
        match fs
            .read_text_file(&ctx.session_id, &parsed.file_path, None, None)
            .await
        {
            Ok(raw) => {
                let offset = parsed.offset.unwrap_or(0);
                ToolResult::success(format_with_line_numbers(&raw, offset, parsed.limit))
            }
            Err(_) => self.fallback.execute(input, ctx).await,
        }
    }
}

/// Format `raw` with 1-based line numbers (`cat -n` style), applying the
/// 0-based `offset` and optional `limit`, mirroring cersei's `read_file`
/// primitive so a client-buffer read looks identical to an on-disk read.
fn format_with_line_numbers(raw: &str, offset: usize, limit: Option<usize>) -> String {
    let all_lines: Vec<&str> = raw.lines().collect();
    let total = all_lines.len();
    let end = match limit {
        Some(n) if n > 0 => (offset + n).min(total),
        _ => total,
    };
    let selected = &all_lines[offset.min(total)..end];
    let mut out = String::new();
    for (i, line) in selected.iter().enumerate() {
        out.push_str(&format!("{:>6}\t{}\n", offset + i + 1, line));
    }
    out
}

// ─── Client-tracking Write/Edit (replace the built-ins) ────────────────────
//
// These wrap cersei's disk-writing tools so the ACP client can track edits
// made during a run. The on-disk write remains the source of truth (so Read,
// Bash, and git still see the repo state); after a successful write we mirror
// the resulting file content to the client via `fs/write_text_file`. A mirror
// failure is non-fatal — the disk write already succeeded — so it is surfaced
// as a trailing note on the tool result rather than an error. Mirroring only
// happens for absolute paths when the client advertised `fs.writeTextFile`;
// everything else behaves exactly like the built-in tool.

/// Mirror `content` at `path` to the ACP client (when supported) so the
/// editor reconciles its buffer with the agent's write. `path` must be the
/// same absolute path the underlying tool just wrote to. Returns a short
/// status string to append to the tool result (empty when nothing mirrored).
async fn mirror_to_client(fs: &AcpFsSink, session_id: &str, path: &str, content: &str) -> String {
    let Some(fs) = fs.as_ref() else {
        return String::new();
    };
    if !fs.supports_write_text_file() {
        return String::new();
    }
    if !Path::new(path).is_absolute() {
        return String::new();
    }
    match fs.write_text_file(session_id, path, content).await {
        Ok(()) => " [mirrored to client]".to_string(),
        Err(e) => format!(" [client mirror failed: {e}]"),
    }
}

/// `Write` tool that also pushes the written content to the ACP client.
pub struct ClientWriteTool {
    fs: AcpFsSink,
    fallback: cersei::tools::file_write::FileWriteTool,
}

impl ClientWriteTool {
    pub fn new(fs: AcpFsSink) -> Self {
        Self {
            fs,
            fallback: cersei::tools::file_write::FileWriteTool,
        }
    }
}

#[async_trait]
impl Tool for ClientWriteTool {
    fn name(&self) -> &str {
        "Write"
    }
    fn description(&self) -> &str {
        "Write content to a file, creating it if it doesn't exist."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let result = self.fallback.execute(input.clone(), ctx).await;
        if result.is_error {
            return result;
        }
        // The written content is in the input; re-extract it to mirror exactly
        // what was written (the built-in tool writes `content` verbatim).
        let (file_path, content) = match serde_json::from_value::<struct_write::Input>(input) {
            Ok(i) => (i.file_path, i.content),
            Err(_) => return result, // shouldn't happen — the write already succeeded
        };
        let note = mirror_to_client(&self.fs, &ctx.session_id, &file_path, &content).await;
        if note.is_empty() {
            result
        } else {
            ToolResult::success(format!("{}{}", result.content, note))
        }
    }
}

/// `Edit` tool that mirrors the edited file's final content to the ACP client.
pub struct ClientEditTool {
    fs: AcpFsSink,
    fallback: cersei::tools::file_edit::FileEditTool,
}

impl ClientEditTool {
    pub fn new(fs: AcpFsSink) -> Self {
        Self {
            fs,
            fallback: cersei::tools::file_edit::FileEditTool,
        }
    }
}

#[async_trait]
impl Tool for ClientEditTool {
    fn name(&self) -> &str {
        "Edit"
    }
    fn description(&self) -> &str {
        "Perform exact string replacements in files."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "old_string": { "type": "string", "description": "The text to replace" },
                "new_string": { "type": "string", "description": "The replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences", "default": false }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let parsed = serde_json::from_value::<struct_edit::Input>(input.clone());
        let result = self.fallback.execute(input, ctx).await;
        if result.is_error {
            return result;
        }
        let Ok(parsed) = parsed else { return result };
        // Re-read the post-edit content so the client gets the file as it now
        // is on disk (the edit may have shifted line numbers, etc.).
        let final_content = tokio::fs::read_to_string(&parsed.file_path)
            .await
            .unwrap_or_default();
        let note =
            mirror_to_client(&self.fs, &ctx.session_id, &parsed.file_path, &final_content).await;
        if note.is_empty() {
            result
        } else {
            ToolResult::success(format!("{}{}", result.content, note))
        }
    }
}

/// Input shapes for the wrapping write/edit tools (parsed to recover the
/// `file_path`/`content` for mirroring). Kept private to this module.
mod struct_write {
    use serde::Deserialize;
    #[derive(Deserialize)]
    pub struct Input {
        pub file_path: String,
        pub content: String,
    }
}

mod struct_edit {
    use serde::Deserialize;
    #[derive(Deserialize)]
    pub struct Input {
        pub file_path: String,
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
                        ToolResult::error(format!("no docs returned for '{}'", input.library_title))
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
    cmd.arg("-rn")
        .arg("--color=never")
        .arg(format!("-m{per_file}"));
    if let Some(flags) = flags {
        let mut iter = flags.split_whitespace();
        while let Some(flag) = iter.next() {
            if flag == "-g"
                && let Some(glob) = iter.next()
            {
                if let Some(rest) = glob.strip_prefix('!') {
                    cmd.arg("--exclude").arg(rest);
                } else {
                    cmd.arg("--include").arg(glob);
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

// ─── Web search ────────────────────────────────────────────────────────────

/// Environment variable for the Parallel Search MCP endpoint. The default
/// endpoint needs no API key (anonymous free tier).
const PARALLEL_MCP_URL_ENV: &str = "PARALLEL_SEARCH_MCP_URL";
/// Default Parallel Search MCP endpoint (Streamable HTTP, no API key).
const DEFAULT_PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";

/// Environment variable for the TinyFish Search API key.
const TINYFISH_API_KEY_ENV: &str = "TINYFISH_API_KEY";
/// Environment variable for the TinyFish Search API endpoint.
const TINYFISH_API_URL_ENV: &str = "TINYFISH_API_URL";
/// Default TinyFish Search endpoint.
const DEFAULT_TINYFISH_URL: &str = "https://api.search.tinyfish.ai";

/// Environment variable for the LangSearch API key (fallback).
const LANGSEARCH_API_KEY_ENV: &str = "LANGSEARCH_API_KEY";
/// Environment variable for the LangSearch API endpoint.
const LANGSEARCH_API_URL_ENV: &str = "LANGSEARCH_API_URL";
/// Default LangSearch endpoint (LangSearch Web Search API).
const DEFAULT_LANGSEARCH_URL: &str = "https://api.langsearch.com/v1/web-search";

/// Environment variable for the Exa Search API key (highest-priority backend).
const EXA_API_KEY_ENV: &str = "EXA_API_KEY";
/// Environment variable for the Exa Search API endpoint.
const EXA_API_URL_ENV: &str = "EXA_API_URL";
/// Default Exa Search endpoint.
const DEFAULT_EXA_URL: &str = "https://api.exa.ai/search";

/// One formatted web search result.
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Format results as "N. **title**\n url\n snippet\n".
fn format_search_results(results: &[SearchResult]) -> String {
    let mut output = String::new();
    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!(
            "{}. **{}**\n {}\n {}\n\n",
            i + 1,
            result.title,
            result.url,
            result.snippet
        ));
    }
    output
}

/// TinyFish Search: GET the endpoint with `X-API-Key` auth; the response
/// carries results in `results[]` with `title`/`url`/`snippet`.
async fn tinyfish_search(
    query: &str,
    num_results: usize,
    api_key: &str,
) -> anyhow::Result<Vec<SearchResult>> {
    let search_url =
        std::env::var(TINYFISH_API_URL_ENV).unwrap_or_else(|_| DEFAULT_TINYFISH_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client
        .get(&search_url)
        .header("X-API-Key", api_key)
        .query(&[("query", query)])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("TinyFish search API error ({status}): {body}");
    }
    let json: Value = response.json().await?;
    let mut results = Vec::new();
    if let Some(items) = json["results"].as_array() {
        for item in items.iter().take(num_results) {
            results.push(SearchResult {
                title: item["title"].as_str().unwrap_or("(no title)").to_string(),
                url: item["url"].as_str().unwrap_or("").to_string(),
                snippet: item["snippet"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(results)
}

/// LangSearch Web Search: POST the endpoint with Bearer auth; the
/// Bing-compatible response carries results in `webPages.value[]` with
/// `name`/`url`/`snippet`.
async fn langsearch_search(
    query: &str,
    num_results: usize,
    api_key: &str,
) -> anyhow::Result<Vec<SearchResult>> {
    let search_url = std::env::var(LANGSEARCH_API_URL_ENV)
        .unwrap_or_else(|_| DEFAULT_LANGSEARCH_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client
        .post(&search_url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "count": num_results,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("LangSearch search API error ({status}): {body}");
    }
    let json: Value = response.json().await?;
    let mut results = Vec::new();
    if let Some(items) = json["webPages"]["value"].as_array() {
        for item in items.iter().take(num_results) {
            results.push(SearchResult {
                title: item["name"].as_str().unwrap_or("(no title)").to_string(),
                url: item["url"].as_str().unwrap_or("").to_string(),
                snippet: item["snippet"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(results)
}

/// Exa Search: POST the `/search` endpoint with `x-api-key` auth; the
/// response carries results in `results[]` with `title`/`url` and, when
/// `contents.highlights` is requested, query-relevant `highlights[]`
/// excerpts.
async fn exa_search(
    query: &str,
    num_results: usize,
    api_key: &str,
) -> anyhow::Result<Vec<SearchResult>> {
    let search_url = std::env::var(EXA_API_URL_ENV).unwrap_or_else(|_| DEFAULT_EXA_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client
        .post(&search_url)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({
            "query": query,
            "numResults": num_results,
            "contents": { "highlights": true },
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Exa search API error ({status}): {body}");
    }
    let json: Value = response.json().await?;
    let mut results = Vec::new();
    if let Some(items) = json["results"].as_array() {
        for item in items.iter().take(num_results) {
            results.push(SearchResult {
                title: item["title"].as_str().unwrap_or("(no title)").to_string(),
                url: item["url"].as_str().unwrap_or("").to_string(),
                snippet: item["highlights"]
                    .as_array()
                    .map(|highlights| {
                        highlights
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            });
        }
    }
    Ok(results)
}

/// Send one MCP JSON-RPC request over Streamable HTTP, returning the parsed
/// response and any `Mcp-Session-Id` header the server issued. Handles both
/// plain JSON and SSE-framed responses (SSE is what most Streamable HTTP
/// servers pick when the client advertises `text/event-stream`).
async fn mcp_request(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    body: serde_json::Value,
) -> anyhow::Result<(Value, Option<String>)> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session_id {
        request = request.header("Mcp-Session-Id", session);
    }
    let response = request.json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("Parallel MCP request failed ({status}): {text}");
    }
    let new_session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = response.text().await?;
    let json = if content_type.contains("text/event-stream") {
        // SSE: each event is `event: message` / `data: {...}`; take the last
        // `data:` line, which carries the JSON-RPC payload.
        let mut data = None;
        for line in text.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                data = Some(payload.trim().to_string());
            }
        }
        match data {
            Some(payload) if !payload.is_empty() => serde_json::from_str(&payload)?,
            _ => anyhow::bail!("Parallel MCP returned an empty SSE response"),
        }
    } else {
        serde_json::from_str(&text)?
    };
    Ok((json, new_session))
}

/// Call the Parallel Search MCP `web_search` tool and parse its results.
/// Anonymous free tier — no API key required. The MCP handshake is
/// initialize → notifications/initialized → tools/call; the tool returns a
/// JSON payload (`results[]` with `title`/`url`/`excerpts`) inside the text
/// content, which we parse into the shared `SearchResult` shape.
async fn parallel_mcp_search(query: &str, num_results: usize) -> anyhow::Result<Vec<SearchResult>> {
    let url = std::env::var(PARALLEL_MCP_URL_ENV)
        .unwrap_or_else(|_| DEFAULT_PARALLEL_MCP_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let (init_result, session) = mcp_request(
        &client,
        &url,
        None,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "agent-cli",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        }),
    )
    .await?;
    if init_result.get("error").is_some() {
        anyhow::bail!("Parallel MCP initialize error: {}", init_result["error"]);
    }

    // Notification that initialization is complete (no request id).
    mcp_request(
        &client,
        &url,
        session.as_deref(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await?;

    let (call_result, _) = mcp_request(
        &client,
        &url,
        session.as_deref(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": {
                    "objective": query,
                    "search_queries": [query],
                }
            }
        }),
    )
    .await?;
    if call_result.get("error").is_some() {
        anyhow::bail!("Parallel MCP web_search error: {}", call_result["error"]);
    }
    let result = &call_result["result"];
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        anyhow::bail!("Parallel web_search failed: {}", result["content"]);
    }
    let mut output = String::new();
    if let Some(content) = result["content"].as_array() {
        for block in content {
            if let Some(text) = block["text"].as_str() {
                output.push_str(text);
            }
        }
    }
    let output = output.trim();
    if output.is_empty() {
        anyhow::bail!("Parallel web_search returned no content");
    }
    let json: Value = serde_json::from_str(output)?;
    let mut results = Vec::new();
    if let Some(items) = json["results"].as_array() {
        for item in items.iter().take(num_results) {
            results.push(SearchResult {
                title: item["title"].as_str().unwrap_or("(no title)").to_string(),
                url: item["url"].as_str().unwrap_or("").to_string(),
                snippet: item["excerpts"]
                    .as_array()
                    .map(|excerpts| {
                        excerpts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            });
        }
    }
    if results.is_empty() {
        anyhow::bail!("Parallel web_search returned no results");
    }
    Ok(results)
}

/// Web search registered under the `WebSearch` name so it replaces cersei's
/// built-in WebSearchTool (which reads the legacy `CERSEI_SEARCH_API_KEY`
/// env var). Provider precedence is handled in the background so the model
/// still only sees one tool: Exa (`EXA_API_KEY`) first, then Parallel Search
/// via MCP (anonymous, no key), then TinyFish (`TINYFISH_API_KEY`), then
/// LangSearch (`LANGSEARCH_API_KEY`). The keyed providers fall back to the
/// config `env` map when the vars aren't already set.
pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        "Search the web and return relevant results. Tries Exa (EXA_API_KEY), then Parallel Search (free, no key), then TinyFish (TINYFISH_API_KEY), then LangSearch (LANGSEARCH_API_KEY)."
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
                "query": { "type": "string", "description": "Search query" },
                "num_results": { "type": "integer", "description": "Number of results (default 8, max 10)" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Input {
            query: String,
            num_results: Option<usize>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let num_results = input.num_results.unwrap_or(8).min(10);
        let query = input.query;
        let mut failures = Vec::new();

        // Exa first — highest-priority backend, used when its key is set.
        match std::env::var(EXA_API_KEY_ENV) {
            Ok(key) if !key.is_empty() => match exa_search(&query, num_results, &key).await {
                Ok(results) if results.is_empty() => {
                    return ToolResult::success(format!("No results found for: {query}"));
                }
                Ok(results) => return ToolResult::success(format_search_results(&results)),
                Err(e) => failures.push(format!("Exa: {e}")),
            },
            _ => failures.push(format!("{} is not set", EXA_API_KEY_ENV)),
        }

        // Parallel via MCP next — anonymous, no API key.
        match parallel_mcp_search(&query, num_results).await {
            Ok(results) if results.is_empty() => {
                return ToolResult::success(format!("No results found for: {query}"));
            }
            Ok(results) => return ToolResult::success(format_search_results(&results)),
            Err(e) => failures.push(format!("Parallel: {e}")),
        }

        // TinyFish next.
        match std::env::var(TINYFISH_API_KEY_ENV) {
            Ok(key) if !key.is_empty() => match tinyfish_search(&query, num_results, &key).await {
                Ok(results) if results.is_empty() => {
                    return ToolResult::success(format!("No results found for: {query}"));
                }
                Ok(results) => return ToolResult::success(format_search_results(&results)),
                Err(e) => failures.push(format!("TinyFish: {e}")),
            },
            _ => failures.push(format!("{} is not set", TINYFISH_API_KEY_ENV)),
        }

        // LangSearch last.
        match std::env::var(LANGSEARCH_API_KEY_ENV) {
            Ok(key) if !key.is_empty() => {
                match langsearch_search(&query, num_results, &key).await {
                    Ok(results) if results.is_empty() => {
                        return ToolResult::success(format!("No results found for: {query}"));
                    }
                    Ok(results) => return ToolResult::success(format_search_results(&results)),
                    Err(e) => failures.push(format!("LangSearch: {e}")),
                }
            }
            _ => failures.push(format!("{} is not set", LANGSEARCH_API_KEY_ENV)),
        }

        ToolResult::error(format!(
            "Web search failed. EXA_API_KEY, Parallel Search (no key needed), TINYFISH_API_KEY, and LANGSEARCH_API_KEY were all unavailable.\n{}",
            failures.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cersei::tools::CostTracker;
    use cersei::tools::permissions::AllowAll;
    use parking_lot::Mutex;
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
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    async fn run_search(pattern: &str, flags: Option<&str>, dir: &std::path::Path) -> ToolResult {
        let input = serde_json::json!({
            "pattern": pattern,
            "flags": flags,
        });
        RgSearchTool
            .execute(input, &test_context(dir.to_path_buf()))
            .await
    }

    #[tokio::test]
    async fn web_search_reports_missing_keys() {
        // Unset in case a previous test set them. The Parallel MCP endpoint
        // is pointed at a closed local port so the fallback chain completes
        // without touching the network (connection refused fails fast); the
        // other three providers fail on their missing keys.
        unsafe { std::env::remove_var(EXA_API_KEY_ENV) };
        unsafe { std::env::remove_var(TINYFISH_API_KEY_ENV) };
        unsafe { std::env::remove_var(LANGSEARCH_API_KEY_ENV) };
        unsafe { std::env::set_var(PARALLEL_MCP_URL_ENV, "http://127.0.0.1:1/mcp") };
        let input = serde_json::json!({ "query": "rust async" });
        let result = WebSearchTool
            .execute(input, &test_context(std::env::temp_dir()))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains(EXA_API_KEY_ENV));
        assert!(result.content.contains("Parallel"));
        assert!(result.content.contains(TINYFISH_API_KEY_ENV));
        assert!(result.content.contains(LANGSEARCH_API_KEY_ENV));
        // Registers under the WebSearch name so it replaces the built-in.
        assert_eq!(WebSearchTool.name(), "WebSearch");
        assert!(WebSearchTool.input_schema()["properties"]["query"].is_object());
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
        assert!(
            result.content.contains("a.rs:1: fn foo()"),
            "got: {}",
            result.content
        );
        assert!(
            !result.content.contains("b.rs"),
            "case-sensitive match leaked: {}",
            result.content
        );
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
        assert!(
            !result.content.contains("c.txt"),
            "glob filter leaked: {}",
            result.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_relative_path_resolves_against_working_dir() {
        if !rg_available() {
            eprintln!("skipping: ripgrep not installed");
            return;
        }
        let dir = scratch_dir(&[(
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )]);
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
        let dir = scratch_dir(&[("a.rs", "fn foo() {}\n"), ("b.rs", "fn bar() {}\n")]);
        let result = grep_fallback("foo", &dir, DEFAULT_PER_FILE_RESULTS, None).await;
        assert!(!result.is_error, "fallback failed: {}", result.content);
        assert!(result.content.contains("a.rs:1"), "got: {}", result.content);
        assert!(!result.content.contains("b.rs"), "got: {}", result.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_fallback_translates_glob_flags() {
        let dir = scratch_dir(&[("a.rs", "fn foo() {}\n"), ("b.txt", "foo here\n")]);
        let result = grep_fallback("foo", &dir, DEFAULT_PER_FILE_RESULTS, Some("-g *.rs")).await;
        assert!(!result.is_error, "fallback failed: {}", result.content);
        assert!(result.content.contains("a.rs:1"), "got: {}", result.content);
        assert!(
            !result.content.contains("b.txt"),
            "glob filter leaked: {}",
            result.content
        );
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
        assert!(
            !result.is_error,
            "no-match must not error: {}",
            result.content
        );
        assert!(
            result.content.contains("No matches"),
            "got: {}",
            result.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── Client file tools ───────────────────────────────────────────────

    /// Captured read request: `(session_id, path)`.
    type ReadCapture = (String, String);
    /// Captured write request: `(session_id, (path, content))`.
    type WriteCapture = (String, (String, String));

    /// A stub `AcpFs` capturing the last read and the last write (path, content)
    /// so the wrappers can be exercised without a real ACP client.
    struct StubFs {
        read_content: Option<String>,
        read_supports: bool,
        write_supports: bool,
        last_read: Arc<Mutex<Option<ReadCapture>>>,
        last_write: Arc<Mutex<Option<WriteCapture>>>,
    }

    #[async_trait::async_trait]
    impl crate::subagents::AcpFs for StubFs {
        fn supports_read_text_file(&self) -> bool {
            self.read_supports
        }
        async fn read_text_file(
            &self,
            session_id: &str,
            path: &str,
            _line: Option<u32>,
            _limit: Option<u32>,
        ) -> anyhow::Result<String> {
            *self.last_read.lock() = Some((session_id.to_string(), path.to_string()));
            match &self.read_content {
                Some(c) => Ok(c.clone()),
                None => Err(anyhow::anyhow!("stub: file not open in editor")),
            }
        }
        fn supports_write_text_file(&self) -> bool {
            self.write_supports
        }
        async fn write_text_file(
            &self,
            session_id: &str,
            path: &str,
            content: &str,
        ) -> anyhow::Result<()> {
            *self.last_write.lock() = Some((
                session_id.to_string(),
                (path.to_string(), content.to_string()),
            ));
            Ok(())
        }
    }

    /// Build a stub `AcpFs` and return it both as a typed `Arc<StubFs>` (for
    /// inspecting captures) and as an `Arc<dyn AcpFs>` (to hand to a tool).
    fn fs_arc(
        read_content: Option<&str>,
        read_supports: bool,
        write_supports: bool,
    ) -> (Arc<StubFs>, Arc<dyn crate::subagents::AcpFs>) {
        let stub = Arc::new(StubFs {
            read_content: read_content.map(String::from),
            read_supports,
            write_supports,
            last_read: Arc::new(Mutex::new(None)),
            last_write: Arc::new(Mutex::new(None)),
        });
        let fs: Arc<dyn crate::subagents::AcpFs> = stub.clone();
        (stub, fs)
    }

    #[tokio::test]
    async fn read_uses_client_buffer_when_supported() {
        let dir = scratch_dir(&[("a.rs", "on disk\n")]);
        let disk_path = dir.join("a.rs");
        let (stub, fs) = fs_arc(Some("unsaved line 1\nunsaved line 2\n"), true, false);
        let tool = ClientReadTool::new(Some(fs));
        let input = serde_json::json!({ "file_path": disk_path });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        // The client buffer (not the on-disk content) must be returned.
        assert!(result.content.contains("unsaved line 1"));
        assert!(!result.content.contains("on disk"));
        // cat -n formatting applied.
        assert!(result.content.contains("     1\tunsaved line 1"));
        // The request reached the reader with the session id and absolute path.
        let (session_id, path) = stub.last_read.lock().clone().expect("request captured");
        assert_eq!(session_id, "test");
        assert_eq!(path, disk_path.to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_falls_back_to_disk_on_client_error() {
        let dir = scratch_dir(&[("a.rs", "on disk line\n")]);
        let disk_path = dir.join("a.rs");
        // Reader advertises support but errors (file not open in editor):
        // must transparently fall back to the on-disk read.
        let (_stub, fs) = fs_arc(None, true, false);
        let tool = ClientReadTool::new(Some(fs));
        let input = serde_json::json!({ "file_path": disk_path });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("on disk line"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_uses_disk_without_reader() {
        let dir = scratch_dir(&[("a.rs", "disk only\n")]);
        let disk_path = dir.join("a.rs");
        // No ACP client: pure on-disk read.
        let tool = ClientReadTool::new(None);
        let input = serde_json::json!({ "file_path": disk_path });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("disk only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_does_not_consult_client_for_relative_path() {
        let dir = scratch_dir(&[("rel.rs", "relative content\n")]);
        let (stub, fs) = fs_arc(Some("should not be used\n"), true, false);
        let tool = ClientReadTool::new(Some(fs));
        // Relative path: the spec requires absolute paths, so the client must
        // never be consulted. The built-in Read tool resolves relative paths
        // against the process cwd and errors when the file isn't found there.
        let input = serde_json::json!({ "file_path": "rel.rs" });
        let ctx = test_context(dir.clone());
        let result = tool.execute(input, &ctx).await;
        assert!(
            stub.last_read.lock().is_none(),
            "client must not be consulted for relative paths"
        );
        // Matches the built-in Read tool's behavior for a missing-from-cwd path.
        assert!(
            result.is_error,
            "expected built-in error for relative path: {}",
            result.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_with_line_numbers_matches_cat_n_style() {
        let raw = "alpha\nbeta\ngamma\n";
        let out = format_with_line_numbers(raw, 0, None);
        assert_eq!(out, "     1\talpha\n     2\tbeta\n     3\tgamma\n");
        // offset + limit windowing.
        let out = format_with_line_numbers(raw, 1, Some(1));
        assert_eq!(out, "     2\tbeta\n");
    }

    // ─── ClientWriteTool / ClientEditTool ──────────────────────────────

    #[tokio::test]
    async fn write_mirrors_content_to_client_when_supported() {
        let dir = scratch_dir(&[]);
        let target = dir.join("new.txt");
        let (stub, fs) = fs_arc(None, false, true);
        let tool = ClientWriteTool::new(Some(fs));
        let input = serde_json::json!({
            "file_path": target,
            "content": "fresh content\n"
        });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("mirrored to client"));
        // Disk got the content.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fresh content\n");
        // Client was mirrored with the same path + content.
        let (session_id, (path, content)) = stub.last_write.lock().clone().expect("write captured");
        assert_eq!(session_id, "test");
        assert_eq!(path, target.to_string_lossy());
        assert_eq!(content, "fresh content\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_skips_mirror_without_client() {
        let dir = scratch_dir(&[]);
        let target = dir.join("plain.txt");
        // No ACP client: pure on-disk write, no mirror attempt.
        let tool = ClientWriteTool::new(None);
        let input = serde_json::json!({
            "file_path": target,
            "content": "just disk\n"
        });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(!result.content.contains("mirrored"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "just disk\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_skips_mirror_when_capability_off() {
        let dir = scratch_dir(&[]);
        let target = dir.join("capoff.txt");
        // Client present but did NOT advertise fs.writeTextFile.
        let (stub, fs) = fs_arc(None, true, false);
        let tool = ClientWriteTool::new(Some(fs));
        let input = serde_json::json!({
            "file_path": target,
            "content": "x\n"
        });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(!result.content.contains("mirrored"));
        assert!(
            stub.last_write.lock().is_none(),
            "must not mirror without capability"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_mirrors_final_content_to_client() {
        let dir = scratch_dir(&[("e.rs", "alpha\nbeta\ngamma\n")]);
        let target = dir.join("e.rs");
        let (stub, fs) = fs_arc(None, false, true);
        let tool = ClientEditTool::new(Some(fs));
        let input = serde_json::json!({
            "file_path": target,
            "old_string": "beta",
            "new_string": "BETA"
        });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("mirrored to client"));
        // Disk reflects the edit.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        // Client was mirrored with the final (post-edit) content.
        let (_session_id, (path, content)) =
            stub.last_write.lock().clone().expect("write captured");
        assert_eq!(path, target.to_string_lossy());
        assert_eq!(content, "alpha\nBETA\ngamma\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_skips_mirror_on_not_found_error() {
        let dir = scratch_dir(&[("e.rs", "alpha\n")]);
        let target = dir.join("e.rs");
        let (stub, fs) = fs_arc(None, false, true);
        let tool = ClientEditTool::new(Some(fs));
        // old_string absent: the built-in tool errors; no mirror must happen.
        let input = serde_json::json!({
            "file_path": target,
            "old_string": "missing",
            "new_string": "x"
        });
        let result = tool.execute(input, &test_context(dir.clone())).await;
        assert!(result.is_error, "expected edit error: {}", result.content);
        assert!(
            stub.last_write.lock().is_none(),
            "must not mirror a failed edit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
