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
use cersei::events::AgentEvent;
use cersei::tools::permissions::AllowAll;
use cersei::tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use cersei::tools::{web_fetch::WebFetchTool, web_search::WebSearchTool};
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;

/// Shared handle to the live parent agent. `build_agent` fills this after
/// constructing the agent; sub-agents with `include_message_history` read it
/// at spawn time to inherit the conversation (including tool results).
pub type ParentHandle = Arc<Mutex<Option<Weak<Agent>>>>;

/// Shared sink where `suggest_followups` stores its suggestions. The TUI and
/// single-shot runner drain it after a run completes.
pub type FollowupSink = Arc<Mutex<Vec<Followup>>>;

/// Optional broadcast sender for sub-agent activity. The TUI subscribes to
/// this channel to render each spawned agent's tool calls nested under the
/// parent `spawn_agents` call. In headless (`-p`) and ACP modes there is no
/// receiver and the sends are dropped.
pub type SubAgentEventSink = Option<broadcast::Sender<SubAgentActivity>>;

/// Bridges the agent's filesystem tools to the ACP client's environment.
/// `fs/read_text_file` returns unsaved editor buffers; `fs/write_text_file`
/// lets the client track edits the agent made during a run. Implemented by
/// the ACP server; the TUI/headless paths supply `None` (no editor to consult).
#[async_trait::async_trait]
pub trait AcpFs: Send + Sync {
    /// `true` if the client advertised `fs.readTextFile`. Lets the wrapping
    /// Read tool skip the round-trip entirely when the client can't help.
    fn supports_read_text_file(&self) -> bool;

    /// Read `path` from the client. `line` is 1-based, `limit` a line cap, per
    /// the ACP `fs/read_text_file` schema. Errors propagate to the caller so
    /// it can fall back to the local filesystem.
    async fn read_text_file(
        &self,
        session_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String>;

    /// `true` if the client advertised `fs.writeTextFile`. Lets the wrapping
    /// write/edit tools skip the mirror round-trip when the client can't track.
    fn supports_write_text_file(&self) -> bool;

    /// Write `content` to `path` in the client's environment via
    /// `fs/write_text_file`. The client creates the file if it doesn't exist.
    /// Errors propagate to the caller so the wrapping tool can treat a failed
    /// mirror as non-fatal (the disk write already succeeded).
    async fn write_text_file(
        &self,
        session_id: &str,
        path: &str,
        content: &str,
    ) -> anyhow::Result<()>;
}

/// Shared handle to an optional ACP client-filesystem bridge. `None` means
/// the run isn't backed by an ACP client (TUI/`-p`), so the wrapping file
/// tools read/write the local filesystem directly with no client mirroring.
pub type AcpFsSink = Option<Arc<dyn AcpFs>>;

/// A single piece of sub-agent activity, tagged with the run id of the
/// sub-agent that produced it. The TUI uses `run_id` to attach events to the
/// right `spawn_agents` parent call even when they arrive out of order.
#[derive(Debug, Clone)]
pub enum SubAgentActivity {
    /// The sub-agent started: creates the nested header under `spawn_agents`.
    Started {
        run_id: u64,
        agent_type: String,
        display_name: String,
        prompt: String,
    },
    /// A tool call inside the sub-agent started.
    ToolStart {
        run_id: u64,
        name: String,
        input_summary: String,
    },
    /// A tool call inside the sub-agent finished.
    ToolEnd {
        run_id: u64,
        name: String,
        is_error: bool,
        output_preview: String,
        duration_ms: u64,
    },
    /// The sub-agent finished; `text` is its final output.
    Finished {
        run_id: u64,
        text: String,
    },
}

impl SubAgentActivity {
    pub fn run_id(&self) -> u64 {
        match self {
            SubAgentActivity::Started { run_id, .. }
            | SubAgentActivity::ToolStart { run_id, .. }
            | SubAgentActivity::ToolEnd { run_id, .. }
            | SubAgentActivity::Finished { run_id, .. } => *run_id,
        }
    }
}

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
    /// Strip `<think>...</think>` blocks from the sub-agent's output before
    /// returning it (freebuff's thinker hides its raw thinking from the
    /// parent).
    pub strip_think_tags: bool,
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
            strip_think_tags: false,
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
            strip_think_tags: false,
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
            strip_think_tags: false,
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
            strip_think_tags: false,
        },
        SubAgentDef {
            id: "thinker",
            display_name: "Thinker",
            spawner_prompt: "Does deep thinking given the current conversation history and a specific prompt to focus on. Use this to help you solve a specific problem, especially hard reasoning questions. You must gather any relevant context before spawning this agent because the thinker agent has no access to tools. You can keep the prompt very short, because the thinker agent can see the entire conversation history for context.",
            system_prompt: "You are the thinker agent. Use the <think> tag to think deeply about the user request. When satisfied, write out a very concise response that captures the most important points. DO NOT be verbose — say the absolute minimum needed to answer the user's question correctly. The parent agent will see your response. DO NOT call any tools. Just do the thinking work now.",
            tools: || vec![],
            max_turns: 1,
            include_message_history: true,
            strip_think_tags: true,
        },
    ]
}

/// The parent system prompt section that tells the model which sub-agents it
/// can spawn and walks it through freebuff's phase-based workflow
/// (explore → write_todos → implement → reviewer loop → validate →
/// suggest_followups). freebuff encodes this in its base2/base-deep
/// instructions prompts; the workflow is prompt-encoded, not a state machine.
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
         ## Phase Workflow\n\
         Act as a helpful assistant and freely respond to the user's request however would be most helpful to the user. \
         Use your judgement to orchestrate the completion of the user's request using your specialized sub-agents and tools as needed. \
         Take your time and be comprehensive. Don't surprise the user — for example, don't modify files if the user has not asked you to do so at least implicitly.\n\
         Follow this phase workflow for implementation tasks. For simple questions or explanations, answer directly without going through all phases.\n\
         **Phase 1 — Explore:** Before asking questions or writing any code, gather broad context about the relevant parts of the codebase and any external knowledge needed. \
         Spawn code-searcher, researcher-web, and researcher-docs agents IN PARALLEL to find all files relevant to the user's request and research any libraries, APIs, or technologies involved. \
         Cast a wide net — spawn multiple code-searcher queries with different angles, and researchers for any external docs or web resources that could inform the implementation. \
         Read the relevant files returned by these agents using the Read tool. This context will help you avoid building the wrong thing.\n\
         **Phase 2 — write_todos:** For any task requiring 3+ steps, use the TodoWrite tool to write out your step-by-step implementation plan. \
         Include ALL of the applicable tasks in the list. You should include a step to review the changes after you have implemented them, and at least one step to validate/test your changes (be specific about whether to typecheck, run tests, run lints, etc.). \
         Update the todo list as you complete each step during implementation. Skip write_todos for simple tasks like quick edits or answering questions.\n\
         For hard reasoning questions — a tricky algorithm, a subtle bug, or a complex design decision — spawn the thinker sub-agent before implementing: it has no tools but sees the entire conversation, so give it a short prompt describing the specific problem and let its answer inform your plan.\n\
         **Phase 3 — Implement:** Fully implement the plan using direct file editing tools. Prefer Edit/ApplyPatch for existing-file edits; use Write only for creating or replacing entire files when that is simpler. \
         Implement ALL requirements — do not leave anything partially done. Narrate what you are doing as you go.\n\
         **Phase 4 — Review Loop:** Iteratively review until the code is clean. \
         Spawn the code-reviewer sub-agent to review all changes. If the reviewer finds ANY issues, fix them. \
         After fixing, you MUST spawn code-reviewer again to re-review. Repeat until the reviewer finds no new issues. Do NOT skip the re-review — every fix must be verified.\n\
         **Phase 5 — Validate:** Thoroughly validate the changes. \
         Run the project's relevant validation commands (typechecks, tests, lints) via the Bash tool, in parallel when possible. \
         Write and run additional tests for new functionality. Fix any failures and re-validate.\n\
         **Phase 6 — Follow-ups:** End your response by calling the `suggest_followups` tool with exactly 3 followups the user is likely to want next — natural next questions, deeper dives, or related directions that build on what you just said. \
         For each followup give a short `label` (2–5 words, the card title) and a `prompt` (the message sent verbatim when the user clicks it, phrased in the user's first-person voice, e.g. \"Show me how to…\"). \
         Keep the prompt short and goal-oriented — usually one sentence naming what the user wants to know. \
         Call it last, after your written answer (and after any tool/subagent calls). Skip it only when there is no sensible next step (e.g. the user said goodbye).\n\
         Give a very short summary of what you accomplished at the end of your turn.\n\
         ## Follow-up Requests\n\
         If the full phase workflow has already been completed in this conversation and the user is asking for a followup change (e.g. \"also add X\" or \"tweak Y\"), you do NOT need to repeat the entire workflow. \
         Use your judgement to run only the phases that are relevant — for example, directly make the requested changes, do a light review, and run validation. Skip the explore and todos phases if the request is a straightforward extension of the work already done."
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
    /// Broadcast channel for sub-agent activity, consumed by the TUI.
    events: SubAgentEventSink,
    /// Unique ids handed out per spawned sub-agent, so the UI can tell the
    /// sub-agents of one `spawn_agents` call apart (and across calls).
    run_counter: AtomicU64,
}

impl SpawnAgentsTool {
    pub fn new(resolved: Resolved, parent: ParentHandle, events: SubAgentEventSink) -> Self {
        let defs = sub_agent_defs();
        let run_counter = AtomicU64::new(0);
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
            events,
            run_counter,
        }
    }

    /// Forward one activity event to the TUI (if any receiver is subscribed).
    /// Best-effort telemetry: in headless mode there is no receiver and the
    /// send fails — that is fine and expected.
    fn emit(&self, activity: SubAgentActivity) {
        if let Some(sender) = &self.events {
            let _ignored = sender.send(activity);
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
            let run_id = self.run_counter.fetch_add(1, Ordering::Relaxed);
            let display_name = def.display_name;
            if def.id == "code-searcher" {
                // Deterministic path: run the queries directly (freebuff's
                // code-searcher is a handleSteps loop, not an LLM agent).
                self.emit(SubAgentActivity::Started {
                    run_id,
                    agent_type: def.id.to_string(),
                    display_name: display_name.to_string(),
                    prompt: prompt.clone(),
                });
                let result = run_code_searcher(agent.params.as_ref(), ctx).await;
                let text = match &result {
                    Ok(t) => t.clone(),
                    Err(e) => e.clone(),
                };
                self.emit(SubAgentActivity::Finished {
                    run_id,
                    text: truncate_preview(&text, 400),
                });
                results[*i] = Some(result);
                continue;
            }
            let i = *i;
            let resolved = self.resolved.clone();
            let parent = self.parent.clone();
            let def = (**def).clone();
            let working_dir = ctx.working_dir.clone();
            let events = self.events.clone();
            set.spawn(async move {
                (
                    i,
                    run_sub_agent(
                        def, resolved, parent, &prompt, working_dir, events, run_id,
                    )
                    .await,
                )
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

/// Truncate a string to at most `max` characters, char-boundary safe (the
/// preview cap used for forwarded activity events).
fn truncate_preview(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Remove `<think>...</think>` blocks from a sub-agent's output. freebuff's
/// thinker agent thinks inside `<think>` tags and its handleSteps strips them
/// before handing the answer to the parent — ported here as a post-processing
/// step on the sub-agent's final text.
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        result.push_str(&rest[..start]);
        let after = &rest[start + "<think>".len()..];
        match after.find("</think>") {
            Some(end) => rest = &after[end + "</think>".len()..],
            // Unclosed tag: drop the remainder of the output.
            None => {
                rest = "";
                break;
            }
        }
    }
    result.push_str(rest);
    result.trim().to_string()
}

/// Build and run one LLM sub-agent with a fresh provider, the def's system
/// prompt and tool set, and (optionally) the parent's conversation history.
/// Every activity event is forwarded to `events` (if a receiver is
/// subscribed) so the TUI can render the sub-agent's tool calls nested under
/// the parent `spawn_agents` call.
async fn run_sub_agent(
    def: SubAgentDef,
    resolved: Resolved,
    parent: ParentHandle,
    prompt: &str,
    working_dir: PathBuf,
    events: SubAgentEventSink,
    run_id: u64,
) -> Result<String, String> {
    let provider = OpenAi::builder()
        .base_url(&resolved.base_url)
        .api_key(&resolved.api_key)
        .model(&resolved.model)
        .build()
        .map_err(|e| format!("failed to build sub-agent provider: {e}"))?;

    let events = events.clone();
    // The event-forwarding closure owns its own clone; `events` stays behind
    // for the Started/Finished bookends.
    let forward_events = events.clone();
    let agent_type = def.id.to_string();
    let display_name = def.display_name.to_string();
    let mut builder = Agent::builder()
        .provider(provider)
        .model(&resolved.model)
        .tools((def.tools)())
        .system_prompt(def.system_prompt)
        .max_turns(def.max_turns)
        .working_dir(working_dir)
        .permission_policy(AllowAll)
        .on_event(move |event: &AgentEvent| {
            // Forward tool lifecycle events so the TUI can render the
            // sub-agent's activity. Text deltas are skipped; the final answer
            // arrives with the `Finished` event.
            let activity = match event {
                AgentEvent::ToolStart { name, input, .. } => Some(SubAgentActivity::ToolStart {
                    run_id,
                    name: name.clone(),
                    input_summary: crate::tui::event_loop::tool_input_summary(name, input),
                }),
                AgentEvent::ToolEnd {
                    name,
                    result,
                    is_error,
                    duration,
                    ..
                } => Some(SubAgentActivity::ToolEnd {
                    run_id,
                    name: name.clone(),
                    is_error: *is_error,
                    output_preview: truncate_preview(result, 200),
                    duration_ms: duration.as_millis() as u64,
                }),
                _ => None,
            };
            if let Some(activity) = activity
                && let Some(sender) = &forward_events
            {
                let _ignored = sender.send(activity);
            }
        });

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
    if let Some(sender) = &events {
        let _ignored = sender.send(SubAgentActivity::Started {
            run_id,
            agent_type: agent_type.clone(),
            display_name: display_name.clone(),
            prompt: prompt.to_string(),
        });
    }
    // Wall-clock cap so a stuck sub-agent can't hang the parent run forever.
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(SUB_AGENT_TIMEOUT_SECS),
        agent.run(prompt),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            let msg = format!("sub-agent failed: {e}");
            if let Some(sender) = &events {
                let _ignored = sender.send(SubAgentActivity::Finished {
                    run_id,
                    text: truncate_preview(&msg, 400),
                });
            }
            return Err(msg);
        }
        Err(_) => {
            let msg = format!("sub-agent timed out after {SUB_AGENT_TIMEOUT_SECS}s");
            if let Some(sender) = &events {
                let _ignored = sender.send(SubAgentActivity::Finished {
                    run_id,
                    text: truncate_preview(&msg, 400),
                });
            }
            return Err(msg);
        }
    };
    let text = output.text().to_string();
    // The thinker's raw `<think>` blocks are stripped before the answer is
    // shown to the parent (and to the user via the TUI preview).
    let text = if def.strip_think_tags {
        strip_think_tags(&text)
    } else {
        text
    };
    if let Some(sender) = &events {
        let _ignored = sender.send(SubAgentActivity::Finished {
            run_id,
            text: truncate_preview(&text, 400),
        });
    }
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
            vec![
                "researcher-web",
                "researcher-docs",
                "code-searcher",
                "code-reviewer",
                "thinker"
            ]
        );
        // History-inheriting agents: code-reviewer (reviews the changes) and
        // thinker (reasons about the conversation); the others start fresh.
        assert!(defs.iter().any(|d| d.id == "code-reviewer" && d.include_message_history));
        assert!(defs.iter().any(|d| d.id == "thinker" && d.include_message_history));
        // The thinker is tool-free and strips its <think> blocks from the
        // answer the parent sees.
        let thinker = defs.iter().find(|d| d.id == "thinker").unwrap();
        assert_eq!((thinker.tools)().len(), 0);
        assert!(thinker.strip_think_tags);
        // Every def has a spawner prompt so the parent knows when to spawn it.
        for def in &defs {
            assert!(!def.spawner_prompt.trim().is_empty(), "{}", def.id);
        }
    }

    #[test]
    fn strip_think_tags_removes_blocks() {
        assert_eq!(
            strip_think_tags("<think>let me reason</think>The answer is 42."),
            "The answer is 42."
        );
        // Multiple blocks, including one mid-answer.
        assert_eq!(
            strip_think_tags("Start. <think>a</think>middle<think>b</think> end."),
            "Start. middle end."
        );
        // Unclosed tag drops the remainder.
        assert_eq!(strip_think_tags("kept <think>never closed"), "kept");
        // No tags: unchanged.
        assert_eq!(strip_think_tags("plain answer"), "plain answer");
    }

    #[test]
    fn spawner_system_prompt_lists_agents() {
        let prompt = spawner_system_prompt();
        for id in [
            "researcher-web",
            "researcher-docs",
            "code-searcher",
            "code-reviewer",
            "thinker",
        ] {
            assert!(prompt.contains(id), "spawner prompt missing {id}");
        }
        assert!(prompt.contains("suggest_followups"));
        assert!(prompt.contains("spawn_agents"));
        assert!(prompt.contains("TodoWrite"));
    }

    #[test]
    fn spawner_system_prompt_has_phase_workflow() {
        let prompt = spawner_system_prompt();
        // The freebuff-style phase workflow: explore → todos → implement →
        // reviewer loop → validate → followups, plus followup-request escape.
        for phase in [
            "Phase 1 — Explore",
            "Phase 2 — write_todos",
            "Phase 3 — Implement",
            "Phase 4 — Review Loop",
            "Phase 5 — Validate",
            "Phase 6 — Follow-ups",
        ] {
            assert!(prompt.contains(phase), "workflow missing {phase}");
        }
        assert!(prompt.contains("re-review"), "reviewer loop must require re-review");
        assert!(prompt.contains("Follow-up Requests"));
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
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)), None);
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
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)), None);
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
        let (tx, mut rx) = broadcast::channel(64);
        let tool = SpawnAgentsTool::new(resolved, parent.clone(), Some(tx));
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
        // Drop the tool so its broadcast sender is gone and the channel
        // closes once the sub-agent task is done.
        drop(tool);
        // The sub-agent run must have forwarded activity events: Started and
        // Finished (the mock replies with plain text, so no tool calls).
        let mut saw_started = false;
        let mut saw_finished = false;
        while let Ok(activity) = rx.recv().await {
            match activity {
                SubAgentActivity::Started { agent_type, .. } => {
                    assert_eq!(agent_type, "researcher-web");
                    saw_started = true;
                }
                SubAgentActivity::Finished { .. } => saw_finished = true,
                _ => {}
            }
        }
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("[researcher-web]"), "{}", result.content);
        assert!(result.content.contains("The answer is 42"), "{}", result.content);
        assert!(saw_started, "sub-agent never forwarded a Started event");
        assert!(saw_finished, "sub-agent never forwarded a Finished event");
    }

    #[tokio::test]
    async fn spawn_thinker_strips_think_tags() {
        // The thinker replies with <think> reasoning then a concise answer;
        // the tags must be stripped from what the parent sees (and from the
        // forwarded Finished preview).
        let base_url =
            text_mock_server("<think>let me reason carefully</think>The answer is 42.").await;
        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: base_url.clone(),
            api_key: "test-key".into(),
        };
        let parent: ParentHandle = Arc::new(Mutex::new(None));
        let (tx, mut rx) = broadcast::channel(64);
        let tool = SpawnAgentsTool::new(resolved, parent.clone(), Some(tx));
        let ctx = test_context(std::env::temp_dir());
        let result = tool
            .execute(
                json!({
                    "agents": [{
                        "agent_type": "thinker",
                        "prompt": "Why does this code deadlock?"
                    }]
                }),
                &ctx,
            )
            .await;
        drop(tool);

        // The parent sees the stripped answer, never the raw thinking.
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("[thinker]"), "{}", result.content);
        assert!(result.content.contains("The answer is 42"), "{}", result.content);
        assert!(
            !result.content.contains("<think>"),
            "think tags leaked into the parent: {}",
            result.content
        );
        // The TUI preview also gets the clean text.
        while let Ok(activity) = rx.recv().await {
            if let SubAgentActivity::Finished { text, .. } = activity {
                assert!(!text.contains("<think>"), "think tags leaked into preview: {text}");
                assert!(text.contains("The answer is 42"), "preview: {text}");
            }
        }
    }

    #[tokio::test]
    async fn spawn_code_searcher_requires_queries() {
        let resolved = Resolved {
            provider: "mock".into(),
            model: "mock/model".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "key".into(),
        };
        let tool = SpawnAgentsTool::new(resolved, Arc::new(Mutex::new(None)), None);
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
