//! Freebuff-style sub-agents: a `spawn_agents` tool that delegates focused
//! sub-tasks (web research, docs research, code search, code review) to
//! specialized sub-agents with their own system prompts and tool sets, plus
//! the `suggest_followups` tool that surfaces clickable next-step suggestions.
//!
//! The catalog mirrors freebuff's `SecretAgentDefinition`s: each sub-agent
//! has an id, a display name, a spawner prompt (when the parent should spawn
//! it), its own system prompt, its own tool set, and optionally inherits the
//! parent's conversation history (`include_message_history`, used by the
//! code-reviewer so it can see the changes it reviews). The parent's system
//! prompt (see [`spawner_system_prompt`]) lists the catalog and tells the
//! model when to spawn each — the workflow is prompt-encoded, exactly like
//! freebuff's base2 instructions, not a state machine.

use crate::providers::Resolved;
use crate::tools::{ReadDocsTool, RgSearchTool};
use async_trait::async_trait;
use cersei::tools::permissions::AllowAll;
use cersei::tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use cersei::tools::{web_fetch::WebFetchTool, web_search::WebSearchTool};
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Weak};

/// Shared handle to the live parent agent. `build_agent` fills this after
/// constructing the agent; sub-agents with `include_message_history` read it
/// at spawn time to inherit the conversation (including tool results).
pub type ParentHandle = Arc<Mutex<Option<Weak<Agent>>>>;

/// Shared sink where `suggest_followups` stores its suggestions. The TUI and
/// single-shot runner drain it after a run completes.
pub type FollowupSink = Arc<Mutex<Vec<Followup>>>;

/// A suggested followup prompt, rendered after the agent's reply.
#[derive(Debug, Clone)]
pub struct Followup {
    pub prompt: String,
    pub label: String,
}

/// A spawnable sub-agent definition (freebuff's `SecretAgentDefinition`,
/// reduced to what matters at runtime).
#[derive(Clone)]
pub struct SubAgentDef {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Shown to the parent model so it knows what this agent is for and when
    /// to spawn it.
    pub spawner_prompt: &'static str,
    /// Full system prompt for the sub-agent (systemPrompt + instructions).
    pub system_prompt: &'static str,
    /// Builds a fresh tool set for each spawned instance (`Box<dyn Tool>` is
    /// not cloneable, so we rebuild from constructors).
    pub tools: fn() -> Vec<Box<dyn Tool>>,
    pub max_turns: u32,
    /// Seed the sub-agent with the parent's conversation (including tool
    /// results) instead of starting fresh.
    pub include_message_history: bool,
}

/// The catalog of spawnable sub-agents.
pub fn sub_agent_defs() -> Vec<SubAgentDef> {
    vec![
        SubAgentDef {
            id: "researcher-web",
            display_name: "Web Researcher",
            spawner_prompt: "Browses the web to find relevant information. Spawn whenever the answer depends on current or recent information, or whenever you are not confident in your knowledge.",
            system_prompt: "You are an expert researcher who can search the web to find relevant information. Your goal is to answer the user's question from current search results and useful source pages. Use WebSearch to get search results. Use WebFetch to fetch and extract readable text from pages that would help answer the user's question. Search snippets and answer boxes are NOT evidence and are often stale — you must read source pages with WebFetch before answering.\n\n\
             Research iteratively, in multiple rounds:\n\
             1. Start with 1-2 WebSearch calls. Inspect the titles, links, snippets, answer boxes, and related results.\n\
             2. Call WebFetch on the most promising results, especially official or primary sources. Call WebFetch on several pages at once, in parallel.\n\
             3. After reading, check what is still missing, uncertain, or worth verifying. Run follow-up searches with refined queries (using new terms you learned from the pages) and read more pages until the question is well covered from multiple sources.\n\
             If WebFetch cannot handle a source, choose a different result or explain the limitation.\n\
             HARD RULE: You may not write your final answer until you have successfully fetched at least 3 pages with WebFetch — for multi-part or comparative questions, fetch 5 or more. Search results alone are never sufficient, no matter how complete they look. If you are about to answer and have fewer than 3 WebFetch fetches, call WebFetch instead.\n\
             Then, write up a concise answer that includes key findings for the user's prompt and cites source URLs when useful.\n\
             Do not stop after a tool call — always continue with either more tool calls or your final written answer.",
            tools: || {
                vec![
                    Box::new(WebSearchTool) as Box<dyn Tool>,
                    Box::new(WebFetchTool) as Box<dyn Tool>,
                ]
            },
            max_turns: 12,
            include_message_history: false,
        },
        SubAgentDef {
            id: "researcher-docs",
            display_name: "Doc",
            spawner_prompt: "Expert at reading technical documentation of major public libraries and frameworks to find relevant information. (e.g. React, MongoDB, Postgres, etc.)",
            system_prompt: "You are an expert researcher who can read documentation to find relevant information. Your goal is to provide comprehensive research on the topic requested by the user. Use ReadDocs to get detailed documentation.\n\n\
             Instructions:\n\
             1. Use the ReadDocs tool once to get detailed documentation relevant to the user's question.\n\
             2. Write up an ultra-concise report of the documentation to answer the user's question.",
            tools: || vec![Box::new(ReadDocsTool) as Box<dyn Tool>],
            max_turns: 6,
            include_message_history: false,
        },
        SubAgentDef {
            id: "code-searcher",
            display_name: "Code Searcher",
            spawner_prompt: "Mechanically runs multiple code search queries (using ripgrep line-oriented search) and returns up to 250 results across all source files, showing each line that matches the search pattern. Excludes git-ignored files. You MUST pass searchQueries in params. Example input: { \"params\": { \"searchQueries\": [{ \"pattern\": \"createUser\", \"flags\": \"-g *.ts\" }, { \"pattern\": \"deleteUser\" }] } }",
            // code-searcher is deterministic (freebuff implements it with a
            // handleSteps loop, not an LLM): the spawn tool runs the queries
            // directly, so no agent or tools are needed.
            system_prompt: "You are a mechanical code searcher.",
            tools: || vec![],
            max_turns: 1,
            include_message_history: false,
        },
        SubAgentDef {
            id: "code-reviewer",
            display_name: "Nit Pick Nick",
            spawner_prompt: "Reviews file changes and responds with critical feedback. Use this after making any significant change to the codebase; otherwise, no need to use this agent for minor changes since it takes a second.",
            system_prompt: "You are a subagent that reviews code changes and gives helpful critical feedback. Do not use any tools.\n\
             # Task\n\
             Your task is to provide helpful critical feedback on the last file changes made by the assistant. You should find ways to improve the code changes made recently in the conversation you were given.\n\
             Be brief: If you don't have much critical feedback, simply say it looks good in one sentence. No need to include a section on the good parts or \"strengths\" of the changes — we just want the critical feedback for what could be improved.\n\
             NOTE: You cannot make any changes directly! DO NOT CALL ANY TOOLS! You can only suggest changes.\n\
             # Guidelines\n\
             - Focus on giving feedback that will help the assistant get to a complete and correct solution as the top priority.\n\
             - Make sure all the requirements in the user's message are addressed. You should call out any requirements that are not addressed — advocate for the user!\n\
             - Try to keep any changes to the codebase as minimal as possible.\n\
             - Simplify any logic that can be simplified.\n\
             - Where a function can be reused, reuse it and do not create a new one.\n\
             - Make sure that no new dead code is introduced.\n\
             - Make sure there are no missing imports.\n\
             - Make sure no sections were deleted that weren't supposed to be deleted.\n\
             - Make sure the new code matches the style of the existing code.\n\
             Be extremely concise.",
            tools: || vec![],
            max_turns: 4,
            include_message_history: true,
        },
    ]
}

/// The parent system prompt section that tells the model which sub-agents it
/// can spawn, when to spawn them, and to end with `suggest_followups` —
/// freebuff encodes this workflow in its base2 instructions prompt.
pub fn spawner_system_prompt() -> String {
    let mut agents = String::new();
    for def in sub_agent_defs() {
        agents.push_str(&format!("- {} ({}): {}\n", def.id, def.display_name, def.spawner_prompt));
    }
    format!(
        "## Sub-agents\n\
         You can delegate focused sub-tasks to specialized sub-agents instead of doing them yourself. \
         Call the `spawn_agents` tool with a JSON object: {{\"agents\": [{{\"agent_type\": \"<id>\", \"prompt\": \"self-contained task\", \"params\": {{...}}}}]}}. \
         Sub-agents run in parallel, so give each a fully self-contained prompt; if you need sequential work, spawn one at a time.\n\n\
         Available sub-agents:\n{agents}\n\
         ## Follow-ups\n\
         End every response by calling the `suggest_followups` tool with exactly 3 followups the user is likely to want next — natural next questions, deeper dives, or related directions that build on what you just said. \
         For each followup give a short `label` (2–5 words, the card title) and a `prompt` (the message sent verbatim when the user clicks it, phrased in the user's first-person voice, e.g. \"Show me how to…\"). \
         Keep the prompt short and goal-oriented — usually one sentence naming what the user wants to know. \
         Call it last, after your written answer (and after any tool/subagent calls). Skip it only when there is no sensible next step (e.g. the user said goodbye)."
    )
}

// ─── suggest_followups tool ─────────────────────────────────────────────────

pub struct SuggestFollowupsTool {
    sink: FollowupSink,
}

impl SuggestFollowupsTool {
    pub fn new(sink: FollowupSink) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl Tool for SuggestFollowupsTool {
    fn name(&self) -> &str {
        "suggest_followups"
    }

    fn description(&self) -> &str {
        "Suggest clickable followup prompts to the user. Call this at the end of your response with followups the user is likely to want next."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "followups": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "prompt": { "type": "string", "description": "The prompt text to send as a user message when clicked." },
                            "label": { "type": "string", "description": "Short display label (defaults to the prompt if not provided)." }
                        },
                        "required": ["prompt"]
                    }
                }
            },
            "required": ["followups"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Input {
            followups: Vec<FollowupInput>,
        }
        #[derive(serde::Deserialize)]
        struct FollowupInput {
            prompt: String,
            label: Option<String>,
        }
        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let followups: Vec<Followup> = input
            .followups
            .into_iter()
            .map(|f| Followup {
                label: f.label.unwrap_or_else(|| f.prompt.clone()),
                prompt: f.prompt,
            })
            .collect();
        let count = followups.len();
        *self.sink.lock() = followups;
        ToolResult::success(format!("Recorded {count} followup suggestion(s)."))
    }
}

// ─── spawn_agents tool ──────────────────────────────────────────────────────

/// Spawns sub-agents from the catalog, each with its own provider, system
/// prompt, and tool set. Sub-agents run in parallel (freebuff: "These agents
/// will run in parallel") and their outputs are returned to the parent.
pub struct SpawnAgentsTool {
    resolved: Resolved,
    parent: ParentHandle,
    defs: Vec<SubAgentDef>,
    description: String,
}

impl SpawnAgentsTool {
    pub fn new(resolved: Resolved, parent: ParentHandle) -> Self {
        let defs = sub_agent_defs();
        let mut description = String::from(
            "Spawn specialized sub-agents to handle focused sub-tasks in parallel. \
             Input: {\"agents\": [{\"agent_type\": \"<id>\", \"prompt\": \"self-contained task\", \"params\": {...}}]}. \
             Sub-agents run in parallel — give each a fully self-contained prompt. Available sub-agents:\n",
        );
        for def in &defs {
            description.push_str(&format!(
                "- {} ({}): {}\n",
                def.id, def.display_name, def.spawner_prompt
            ));
        }
        Self {
            resolved,
            parent,
            defs,
            description,
        }
    }
}

#[derive(serde::Deserialize)]
struct SpawnRequest {
    agents: Vec<SpawnRequestAgent>,
}

#[derive(serde::Deserialize)]
struct SpawnRequestAgent {
    agent_type: String,
    prompt: Option<String>,
    params: Option<Value>,
}

#[async_trait]
impl Tool for SpawnAgentsTool {
    fn name(&self) -> &str {
        "spawn_agents"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agents": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent_type": { "type": "string", "description": "The sub-agent to spawn. See the tool description for the available agent types." },
                            "prompt": { "type": "string", "description": "The prompt to send to the agent." },
                            "params": { "type": "object", "description": "Optional parameters for the agent (code-searcher requires params.searchQueries)." }
                        },
                        "required": ["agent_type"]
                    }
                }
            },
            "required": ["agents"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let request: SpawnRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        if request.agents.is_empty() {
            return ToolResult::error("spawn_agents requires at least one agent");
        }

        // Resolve each requested agent against the catalog, failing fast on
        // unknown types.
        let mut resolved: Vec<(usize, &SubAgentDef, SpawnRequestAgent)> = Vec::new();
        for (i, agent) in request.agents.into_iter().enumerate() {
            let Some(def) = self.defs.iter().find(|d| d.id == agent.agent_type) else {
                let known: Vec<&str> = self.defs.iter().map(|d| d.id).collect();
                return ToolResult::error(format!(
                    "unknown agent_type '{}'; available: {}",
                    agent.agent_type,
                    known.join(", ")
                ));
            };
            resolved.push((i, def, agent));
        }

        let mut results: Vec<Option<Result<String, String>>> = vec![None; resolved.len()];
        let mut set = tokio::task::JoinSet::new();

        for (i, def, agent) in &resolved {
            let prompt = agent.prompt.clone().unwrap_or_default();
            if prompt.trim().is_empty() && def.id != "code-searcher" {
                return ToolResult::error(format!(
                    "agent '{}' requires a non-empty prompt",
                    def.id
                ));
            }
            if def.id == "code-searcher" {
                // Deterministic path: run the queries directly (freebuff's
                // code-searcher is a handleSteps loop, not an LLM agent).
                results[*i] = Some(run_code_searcher(agent.params.as_ref(), ctx).await);
                continue;
            }
            let i = *i;
            let resolved = self.resolved.clone();
            let parent = self.parent.clone();
            let def = (**def).clone();
            let working_dir = ctx.working_dir.clone();
            set.spawn(async move {
                (i, run_sub_agent(def, resolved, parent, &prompt, working_dir).await)
            });
        }

        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((i, result)) => results[i] = Some(result),
                Err(e) => {
                    // Task panicked — best-effort, like freebuff: a child
                    // failure doesn't abort the batch.
                    if let Some(i) = results.iter().position(|r| r.is_none()) {
                        results[i] = Some(Err(format!("sub-agent task failed: {e}")));
                    }
                }
            }
        }

        // Deterministic order, matching the input order.
        let mut parts: Vec<String> = Vec::new();
        for (i, def, _) in &resolved {
            let label = format!("[{}]", def.id);
            match results[*i].as_ref() {
                Some(Ok(text)) => parts.push(format!("{label}\n{text}")),
                Some(Err(e)) => parts.push(format!("{label} ERROR: {e}")),
                None => parts.push(format!("{label} (no result)")),
            }
        }

        ToolResult::success(parts.join("\n\n---\n\n"))
    }
}

/// Run one code-searcher request: execute each query in
/// `params.searchQueries` via the ripgrep search tool and return the combined
/// results as JSON. Mirrors freebuff's code-searcher handleSteps.
async fn run_code_searcher(params: Option<&Value>, ctx: &ToolContext) -> Result<String, String> {
    let Some(queries) = params
        .and_then(|p| p.get("searchQueries"))
        .and_then(|q| q.as_array())
    else {
        return Err(
            "code-searcher requires params.searchQueries: [{pattern, flags?, path?, max_results?}]"
                .to_string(),
        );
    };
    let mut results = Vec::new();
    for query in queries {
        let input = json!({
            "pattern": query.get("pattern").and_then(|v| v.as_str()).unwrap_or(""),
            "flags": query.get("flags").and_then(|v| v.as_str()),
            "path": query.get("path").and_then(|v| v.as_str()).or_else(|| query.get("cwd").and_then(|v| v.as_str())),
            "max_results": query.get("max_results").and_then(|v| v.as_u64()).or_else(|| query.get("maxResults").and_then(|v| v.as_u64())),
        });
        let result = RgSearchTool.execute(input, ctx).await;
        results.push(json!({
            "query": query,
            "is_error": result.is_error,
            "output": result.content,
        }));
    }
    serde_json::to_string_pretty(&json!({ "results": results }))
        .map_err(|e| format!("failed to serialize search results: {e}"))
}

/// Wall-clock limit for one LLM sub-agent run (seconds). Prevents a stuck
/// sub-agent from hanging the parent run indefinitely.
const SUB_AGENT_TIMEOUT_SECS: u64 = 120;

/// Build and run one LLM sub-agent with a fresh provider, the def's system
/// prompt and tool set, and (optionally) the parent's conversation history.
async fn run_sub_agent(
    def: SubAgentDef,
    resolved: Resolved,
    parent: ParentHandle,
    prompt: &str,
    working_dir: PathBuf,
) -> Result<String, String> {
    let provider = OpenAi::builder()
        .base_url(&resolved.base_url)
        .api_key(&resolved.api_key)
        .model(&resolved.model)
        .build()
        .map_err(|e| format!("failed to build sub-agent provider: {e}"))?;

    let mut builder = Agent::builder()
        .provider(provider)
        .model(&resolved.model)
        .tools((def.tools)())
        .system_prompt(def.system_prompt)
        .max_turns(def.max_turns)
        .working_dir(working_dir)
        .permission_policy(AllowAll);

    if def.include_message_history {
        let messages = parent
            .lock()
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .map(|agent| agent.messages());
        if let Some(messages) = messages {
            builder = builder.with_messages(messages);
        }
    }

    let agent = builder.build().map_err(|e| format!("failed to build sub-agent: {e}"))?;
    // Wall-clock cap so a stuck sub-agent can't hang the parent run forever.
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(SUB_AGENT_TIMEOUT_SECS),
        agent.run(prompt),
    )
    .await
    .map_err(|_| format!("sub-agent timed out after {SUB_AGENT_TIMEOUT_SECS}s"))?
    .map_err(|e| format!("sub-agent failed: {e}"))?;
    let text = output.text().to_string();
    if text.trim().is_empty() {
        Err("sub-agent returned no output".to_string())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cersei::tools::permissions::AllowAll;
    use cersei::tools::CostTracker;

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

    #[test]
    fn catalog_has_expected_sub_agents() {
        let defs = sub_agent_defs();
        let ids: Vec<&str> = defs.iter().map(|d| d.id).collect();
        assert_eq!(
            ids,
            vec!["researcher-web", "researcher-docs", "code-searcher", "code-reviewer"]
        );
        // code-reviewer must inherit the parent's history; the others start fresh.
        assert!(defs.iter().any(|d| d.id == "code-reviewer" && d.include_message_history));
        // Every def has a spawner prompt so the parent knows when to spawn it.
        for def in &defs {
            assert!(!def.spawner_prompt.trim().is_empty(), "{}", def.id);
        }
    }

    #[test]
    fn spawner_system_prompt_lists_agents() {
        let prompt = spawner_system_prompt();
        for id in ["researcher-web", "researcher-docs", "code-searcher", "code-reviewer"] {
            assert!(prompt.contains(id), "spawner prompt missing {id}");
        }
        assert!(prompt.contains("suggest_followups"));
        assert!(prompt.contains("spawn_agents"));
    }

    #[tokio::test]
    async fn suggest_followups_stores_sink() {
        let sink: FollowupSink = Arc::new(Mutex::new(Vec::new()));
        let tool = SuggestFollowupsTool::new(sink.clone());
        let ctx = test_context(std::env::temp_dir());
        let result = tool
            .execute(
                json!({
                    "followups": [
                        {"prompt": "Show me how to cache this", "label": "Add caching"},
                        {"prompt": "Explain the refactor"}
                    ]
                }),
                &ctx,
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        let stored = sink.lock().clone();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].label, "Add caching");
        assert_eq!(stored[1].label, "Explain the refactor"); // label defaults to prompt
    }

    #[tokio::test]
    async fn spawn_agents_unknown_type_errors() {
        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "key".into(),
        };
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)));
        let ctx = test_context(std::env::temp_dir());
        let result = tool
            .execute(json!({ "agents": [{ "agent_type": "nope" }] }), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown agent_type"), "{}", result.content);
        assert!(result.content.contains("researcher-web"), "{}", result.content);
    }

    #[tokio::test]
    async fn spawn_code_searcher_runs_queries() {
        let dir = std::env::temp_dir().join(format!("subagent-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn create_user() {}\npub fn delete_user() {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "create_user docs here\n").unwrap();

        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "key".into(),
        };
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)));
        let ctx = test_context(dir.clone());
        let result = tool
            .execute(
                json!({
                    "agents": [{
                        "agent_type": "code-searcher",
                        "params": {
                            "searchQueries": [
                                { "pattern": "create_user", "flags": "-g *.rs" },
                                { "pattern": "delete_user" }
                            ]
                        }
                    }]
                }),
                &ctx,
            )
            .await;
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("[code-searcher]"), "{}", result.content);
        assert!(result.content.contains("create_user"), "{}", result.content);
        assert!(result.content.contains("delete_user"), "{}", result.content);
        // The -g *.rs flag must be honored by the search.
        assert!(!result.content.contains("README.md"), "{}", result.content);
    }

    /// A fake OpenAI-compatible SSE server that replies to every request with
    /// the given plain text — drives an LLM sub-agent through a single-turn
    /// run with no tool calls. Returns the base URL to point a provider at.
    async fn text_mock_server(reply: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let reply = reply.to_string();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let reply = reply.clone();
                tokio::spawn(async move {
                    // Drain the request so the client can read the response.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..end]);
                        let content_length = head.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        });
                        match content_length {
                            Some(len) if buf.len() >= end + 4 + len => break,
                            None if buf.windows(5).any(|w| w == b"0\r\n\r\n") => break,
                            _ => continue,
                        }
                    }
                    let body = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({
                            "id": "chatcmpl-sub",
                            "object": "chat.completion.chunk",
                            "choices": [{
                                "index": 0,
                                "delta": { "content": reply },
                                "finish_reason": null
                            }]
                        }),
                        serde_json::json!({
                            "id": "chatcmpl-sub",
                            "object": "chat.completion.chunk",
                            "choices": [{
                                "index": 0,
                                "delta": {},
                                "finish_reason": "stop"
                            }]
                        }),
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{addr}")
    }

    #[tokio::test]
    async fn spawn_llm_sub_agent_runs_against_provider() {
        // Point the sub-agent at a mock provider that returns plain text: the
        // spawned researcher-web should run its own agentic loop and return
        // the reply to the parent.
        let base_url = text_mock_server("The answer is 42.").await;
        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: base_url.clone(),
            api_key: "test-key".into(),
        };
        let parent: ParentHandle = Arc::new(Mutex::new(None));
        let tool = SpawnAgentsTool::new(resolved, parent.clone());
        let ctx = test_context(std::env::temp_dir());
        let result = tool
            .execute(
                json!({
                    "agents": [{
                        "agent_type": "researcher-web",
                        "prompt": "What is the answer?"
                    }]
                }),
                &ctx,
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("[researcher-web]"), "{}", result.content);
        assert!(result.content.contains("The answer is 42"), "{}", result.content);
    }

    #[tokio::test]
    async fn spawn_code_searcher_requires_queries() {
        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "key".into(),
        };
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)));
        let ctx = test_context(std::env::temp_dir());
        let result = tool
            .execute(
                json!({
                    "agents": [{ "agent_type": "code-searcher", "prompt": "find it" }]
                }),
                &ctx,
            )
            .await;
        // The missing-params failure is reported inside the agent's result
        // section (batch semantics — one bad agent doesn't fail the batch).
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("[code-searcher]"), "{}", result.content);
        assert!(result.content.contains("searchQueries"), "{}", result.content);
    }
}
