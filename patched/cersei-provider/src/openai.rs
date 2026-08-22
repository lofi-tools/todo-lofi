//! OpenAI-compatible provider (works with OpenAI, Azure, Ollama, etc.)

use crate::*;
use cersei_types::*;
use futures::StreamExt;
use tokio::sync::mpsc;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

/// Which delta field carries a model's thinking, resolved per model family by
/// the caller (agent-cli maps its `model_families` config onto this). The
/// reader turns it into `ThinkingDelta` events so reasoning streams as a
/// separate block instead of being glued onto (or dropped from) the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningField {
    /// No family configured: auto-detect among the known shapes (`reasoning`,
    /// `reasoning_content`, `reasoning_details` reasoning.text parts).
    Auto,
    /// Read thinking from exactly this delta field (e.g. "reasoning" or
    /// "reasoning_content").
    Field(&'static str),
    /// Treat everything as answer text (the "plain" family).
    Off,
}

pub struct OpenAi {
    auth: Auth,
    base_url: String,
    default_model: String,
    /// F-09: emit `options.num_ctx` on the wire. Ollama-only — OpenAI proper
    /// rejects unknown top-level fields with a 400, so the router sets this
    /// only for local providers whose server-side default window (often 4k)
    /// would otherwise silently truncate the prompt front.
    send_num_ctx: bool,
    /// B2: which schema dialect tools serialize into. `OpenAiLoose` unless
    /// the router's quirks opt a model into `OpenAiStrict`.
    dialect: crate::adapt::SchemaDialect,
    /// How the SSE reader treats reasoning deltas (see [`ReasoningField`]).
    reasoning_field: ReasoningField,
    client: reqwest::Client,
}

impl OpenAi {
    pub fn new(auth: Auth) -> Self {
        let base_url = std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| OPENAI_API_BASE.to_string());
        Self {
            auth,
            base_url,
            default_model: "gpt-4o".to_string(),
            send_num_ctx: false,
            dialect: crate::adapt::SchemaDialect::OpenAiLoose,
            reasoning_field: ReasoningField::Auto,
            client: reqwest::Client::new(),
        }
    }

    /// The configured reasoning field (for tests / diagnostics).
    pub fn reasoning_field(&self) -> ReasoningField {
        self.reasoning_field
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| CerseiError::Auth("OPENAI_API_KEY not set".into()))?;
        Ok(Self::new(Auth::ApiKey(key)))
    }

    pub fn builder() -> OpenAiBuilder {
        OpenAiBuilder::default()
    }
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &str {
        "openai"
    }

    fn context_window(&self, model: &str) -> u64 {
        match model {
            m if m.contains("gpt-5") => 1_000_000,
            m if m.starts_with("o1") || m.starts_with("o3") => 200_000,
            m if m.contains("gpt-4o") => 128_000,
            m if m.contains("gpt-4-turbo") => 128_000,
            m if m.contains("gpt-4") => 8_192,
            m if m.contains("gpt-3.5") => 16_385,
            _ => 128_000,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        // Build OpenAI-format messages
        let mut api_messages: Vec<serde_json::Value> = Vec::new();

        if let Some(system) = &request.system {
            // The boundary marker is a client-side cache hint; OpenAI-compat
            // caching is automatic, so just keep the literal string off the
            // wire instead of letting the model read it.
            let content = system.replace(SYSTEM_PROMPT_DYNAMIC_BOUNDARY, "");
            api_messages.push(serde_json::json!({
                "role": "system",
                "content": content,
            }));
        }

        for msg in &request.messages {
            match msg.role {
                Role::User => {
                    // Check if this is a tool result message
                    if let MessageContent::Blocks(blocks) = &msg.content {
                        for block in blocks {
                            // `is_error` is deliberately discarded: OpenAI's
                            // `role:"tool"` message has no error field on the
                            // wire. The failure signal still reaches the model
                            // because the runner appends its error note into
                            // the result content itself (see §2.1 of the
                            // tool-calling audit).
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error: _,
                            } = block
                            {
                                api_messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                }));
                            }
                        }
                        // Collect text + multimodal (image/PDF) parts into a
                        // single user message. OpenAI takes content as an array
                        // of typed parts when any non-text media is present.
                        let mut parts: Vec<serde_json::Value> = Vec::new();
                        for block in blocks {
                            match block {
                                ContentBlock::Text { text } => {
                                    parts.push(serde_json::json!({
                                        "type": "text",
                                        "text": text,
                                    }));
                                }
                                ContentBlock::Image { source } => {
                                    if let Some(url) = openai_image_url(source) {
                                        parts.push(serde_json::json!({
                                            "type": "image_url",
                                            "image_url": { "url": url },
                                        }));
                                    }
                                }
                                ContentBlock::Document { source, .. } => {
                                    if let Some(part) = openai_file_part(source) {
                                        parts.push(part);
                                    }
                                }
                                _ => {}
                            }
                        }
                        match parts.as_slice() {
                            [] => {}
                            // A single text part collapses to a plain string for
                            // backward compatibility with text-only callers.
                            [only] if only["type"] == "text" => {
                                api_messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": only["text"].clone(),
                                }));
                            }
                            _ => {
                                api_messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": parts,
                                }));
                            }
                        }
                    } else {
                        api_messages.push(serde_json::json!({
                            "role": "user",
                            "content": msg.get_all_text(),
                        }));
                    }
                }
                Role::Assistant => {
                    // Check for tool_use blocks — serialize as tool_calls
                    if let MessageContent::Blocks(blocks) = &msg.content {
                        let tool_uses: Vec<&ContentBlock> = blocks
                            .iter()
                            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                            .collect();
                        if !tool_uses.is_empty() {
                            let tool_calls: Vec<serde_json::Value> = tool_uses
                                .iter()
                                .map(|b| {
                                    if let ContentBlock::ToolUse { id, name, input } = b {
                                        serde_json::json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": input.to_string(),
                                            }
                                        })
                                    } else {
                                        serde_json::json!({})
                                    }
                                })
                                .collect();

                            let text_content: String = blocks
                                .iter()
                                .filter_map(|b| {
                                    if let ContentBlock::Text { text } = b {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("");

                            let mut asst_msg = serde_json::json!({
                                "role": "assistant",
                                "tool_calls": tool_calls,
                            });
                            if !text_content.is_empty() {
                                asst_msg["content"] = serde_json::json!(text_content);
                            }
                            api_messages.push(asst_msg);
                        } else {
                            api_messages.push(serde_json::json!({
                                "role": "assistant",
                                "content": msg.get_all_text(),
                            }));
                        }
                    } else {
                        api_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.get_all_text(),
                        }));
                    }
                }
                Role::System => {
                    api_messages.push(serde_json::json!({
                        "role": "system",
                        "content": msg.get_all_text(),
                    }));
                }
            }
        }

        // GPT-5+ and o-series use max_completion_tokens; older models use max_tokens
        let use_new_param =
            model.starts_with("gpt-5") || model.starts_with("o1") || model.starts_with("o3");

        let mut body = if use_new_param {
            serde_json::json!({
                "model": model,
                "messages": api_messages,
                "max_completion_tokens": request.max_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            })
        } else {
            serde_json::json!({
                "model": model,
                "messages": api_messages,
                "max_tokens": request.max_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            })
        };

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Reasoning effort: provider-agnostic `reasoning_effort` option
        // ("minimal"/"low"/"medium"/"high"), mapped onto the OpenAI request body.
        // Only the o-series / gpt-5 reasoning models accept it.
        if let Some(effort) = reasoning_effort_for(&model, &request.options) {
            body["reasoning_effort"] = serde_json::json!(effort);
        }

        // F-09: without this, Ollama runs the whole session at its
        // server-side default window (often 4k) while compaction budgets
        // against the model's real window — the front of the prompt silently
        // truncates. The runner supplies the value; the flag gates the wire.
        if self.send_num_ctx {
            if let Some(num_ctx) = request.options.get::<u64>("num_ctx") {
                body["options"] = serde_json::json!({ "num_ctx": num_ctx });
            }
        }

        if !request.tools.is_empty() {
            // B1: schemas cross the provider boundary only through
            // `adapt_tools`. The dialect is router-resolved (B2): loose until
            // quirks opt a model into strict.
            let tools = crate::adapt::adapt_tools(&request.tools, self.dialect);
            body["tools"] = serde_json::Value::Array(tools);

            // F-08: forced tool choice, requested per-turn by the runner's
            // no-tool-call nudge.
            if request.options.get::<String>("tool_choice").as_deref() == Some("required") {
                body["tool_choice"] = serde_json::json!("required");
            }
        }

        let url = format!("{}/chat/completions", self.base_url);
        let auth_header = match &self.auth {
            Auth::ApiKey(key) | Auth::Bearer(key) => format!("Bearer {}", key),
            Auth::OAuth { token, .. } => format!("Bearer {}", token.access_token),
            Auth::Custom(_) => String::new(),
        };

        let req = self
            .client
            .post(&url)
            .header("authorization", &auth_header)
            .header("content-type", "application/json")
            .json(&body)
            .build()
            .map_err(CerseiError::Http)?;

        // F-02: await the response and check its status *before* spawning, so a
        // non-2xx returns as a typed `Err` from `complete()`. The runner's retry
        // loop guards `provider.complete()` and nothing else — a status reported
        // from inside the spawned reader lands below that loop, where it can only
        // end the session.
        let response = self.client.execute(req).await.map_err(CerseiError::Http)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = crate::parse_retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(CerseiError::from_http_status(status, retry_after, body));
        }

        let (tx, rx) = mpsc::channel(256);
        let reasoning_field = self.reasoning_field;

        tokio::spawn(async move {
            let _ = tx
                .send(StreamEvent::MessageStart {
                    id: String::new(),
                    model: String::new(),
                    usage: None,
                })
                .await;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut text_started = false;
            // Reasoning models stream their thinking in `delta.reasoning` (or
            // `reasoning_content` / `reasoning_details`), not in `content`.
            // When present it becomes a `thinking` block at index 0, pushing
            // answer text and tool calls up one index slot. See the reader's
            // reasoning handling below.
            let mut thinking_started = false;
            // Track tool calls being assembled across chunks
            // OpenAI sends: tool_calls[i].id, tool_calls[i].function.name (first chunk)
            //               tool_calls[i].function.arguments (subsequent chunks, accumulated)
            // Ordered so the post-loop flush emits calls in ascending slot
            // order (a HashMap made live event order nondeterministic).
            let mut tool_calls: std::collections::BTreeMap<usize, (String, String, String)> =
                std::collections::BTreeMap::new(); // slot -> (id, name, args_json)
            // F-A2: servers that omit `tool_calls[].index` (llama.cpp, some
            // Ollama builds, LiteLLM proxies) get synthetic slots correlated
            // by call id, instead of every parallel call collapsing onto slot
            // 0 and its argument bodies concatenating into invalid JSON.
            let mut slot_for_id: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut last_slot: Option<usize> = None;
            // F-03: tool calls are flushed exactly once, after the read loop.
            // `[DONE]` now only records that the stream terminated cleanly.
            let mut saw_done = false;
            let mut final_stop: Option<StopReason> = None;

            'read: while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find("\n") {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                let data = data.trim();
                                if data == "[DONE]" {
                                    // F-03: do not flush here. Record the
                                    // clean termination and fall out to the
                                    // single finalize block after the read
                                    // loop, which also runs on plain EOF.
                                    saw_done = true;
                                    break 'read;
                                }

                                if let Ok(json) =
                                    serde_json::from_str::<serde_json::Value>(data)
                                {
                                    let delta = &json["choices"][0]["delta"];
                                    let finish_reason =
                                        json["choices"][0]["finish_reason"].as_str();
                                    // Capture the terminal reason wherever it
                                    // appears. The mapping further down only
                                    // runs when the same chunk also carries
                                    // `usage`, and the finalize MessageDelta
                                    // would overwrite it regardless.
                                    match finish_reason {
                                        Some("stop") => {
                                            final_stop = Some(StopReason::EndTurn)
                                        }
                                        Some("tool_calls") => {
                                            final_stop = Some(StopReason::ToolUse)
                                        }
                                        Some("length") => {
                                            final_stop = Some(StopReason::MaxTokens)
                                        }
                                        _ => {}
                                    }

                                    // Thinking content (reasoning models:
                                    // openrouter's stealth/ox-alpha, deepseek-r1,
                                    // qwen3, ...). OpenAI-compatible servers vary
                                    // the wire field: `reasoning` (ox-alpha),
                                    // `reasoning_content` (deepseek), or
                                    // `reasoning_details` (structured
                                    // `reasoning.text` parts). ox-alpha sends both
                                    // `reasoning` and `reasoning_details` on every
                                    // thinking chunk, so prefer the string form to
                                    // avoid double-emitting; encrypted parts
                                    // (`encrypted_content`) are skipped. Which
                                    // field is read follows the provider's
                                    // `reasoning_field` (per-model-family config);
                                    // the default auto-detects.
                                    let reasoning = match reasoning_field {
                                        ReasoningField::Off => None,
                                        ReasoningField::Field("reasoning_details") => delta[
                                            "reasoning_details"
                                        ]
                                        .as_array()
                                        .and_then(|parts| {
                                            parts.iter().find_map(|part| {
                                                if part["type"] == "reasoning.text" {
                                                    part["text"]
                                                        .as_str()
                                                        .filter(|s| !s.is_empty())
                                                } else {
                                                    None
                                                }
                                            })
                                        }),
                                        ReasoningField::Field(field) => delta[field]
                                            .as_str()
                                            .filter(|s| !s.is_empty()),
                                        ReasoningField::Auto => delta["reasoning"]
                                            .as_str()
                                            .filter(|s| !s.is_empty())
                                            .or_else(|| {
                                                delta["reasoning_content"]
                                                    .as_str()
                                                    .filter(|s| !s.is_empty())
                                            })
                                            .or_else(|| {
                                                delta["reasoning_details"]
                                                    .as_array()
                                                    .and_then(|parts| {
                                                        parts.iter().find_map(|part| {
                                                            if part["type"]
                                                                == "reasoning.text"
                                                            {
                                                                part["text"]
                                                                    .as_str()
                                                                    .filter(|s| {
                                                                        !s.is_empty()
                                                                    })
                                                            } else {
                                                                None
                                                            }
                                                        })
                                                    })
                                            }),
                                    };
                                    if let Some(thinking) = reasoning {
                                        if !thinking_started {
                                            thinking_started = true;
                                            let _ = tx
                                                .send(StreamEvent::ContentBlockStart {
                                                    index: 0,
                                                    block_type: "thinking".into(),
                                                    id: None,
                                                    name: None,
                                                })
                                                .await;
                                        }
                                        let _ = tx
                                            .send(StreamEvent::ThinkingDelta {
                                                index: 0,
                                                thinking: thinking.to_string(),
                                            })
                                            .await;
                                    }

                                    // Text content. Sits after the thinking block
                                    // (index 0) when reasoning was streamed, so
                                    // the accumulator keeps the blocks distinct.
                                    let text_index = thinking_started as usize;
                                    if let Some(text) = delta["content"].as_str() {
                                        if !text_started {
                                            text_started = true;
                                            let _ = tx
                                                .send(StreamEvent::ContentBlockStart {
                                                    index: text_index,
                                                    block_type: "text".into(),
                                                    id: None,
                                                    name: None,
                                                })
                                                .await;
                                        }
                                        let _ = tx
                                            .send(StreamEvent::TextDelta {
                                                index: text_index,
                                                text: text.to_string(),
                                            })
                                            .await;
                                    }

                                    // Tool calls (accumulated across chunks)
                                    if let Some(tc_array) = delta["tool_calls"].as_array() {
                                        for tc in tc_array {
                                            let tc_id =
                                                tc["id"].as_str().filter(|s| !s.is_empty());
                                            // F-A2: only an explicit `index` is
                                            // trusted. Without one, correlate by
                                            // call id so parallel calls land in
                                            // distinct slots; an id-less delta is
                                            // a continuation of the slot most
                                            // recently touched.
                                            let idx = match tc["index"].as_u64() {
                                                Some(i) => i as usize,
                                                None => match tc_id
                                                    .and_then(|id| {
                                                        slot_for_id.get(id).copied()
                                                    }) {
                                                    Some(slot) => slot,
                                                    None if tc_id.is_some() => tool_calls
                                                        .keys()
                                                        .next_back()
                                                        .map(|k| k + 1)
                                                        .unwrap_or(0),
                                                    None => last_slot.unwrap_or(0),
                                                },
                                            };
                                            if let Some(id) = tc_id {
                                                slot_for_id.insert(id.to_string(), idx);
                                            }
                                            last_slot = Some(idx);
                                            let entry = tool_calls
                                                .entry(idx)
                                                .or_insert_with(|| {
                                                    (
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                    )
                                                });

                                            // First chunk has id and function.name.
                                            // F-A3: never let an empty-string field
                                            // from a later delta clobber a good one.
                                            if let Some(id) = tc_id {
                                                entry.0 = id.to_string();
                                            }
                                            if let Some(name) = tc["function"]["name"]
                                                .as_str()
                                                .filter(|s| !s.is_empty())
                                            {
                                                entry.1 = name.to_string();
                                            }
                                            // Arguments accumulate across chunks
                                            if let Some(args) =
                                                tc["function"]["arguments"].as_str()
                                            {
                                                entry.2.push_str(args);
                                            }
                                        }
                                    }

                                    // Usage from the final chunk
                                    if let Some(usage) = json["usage"].as_object() {
                                        let input_tokens = usage
                                            .get("prompt_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        let output_tokens = usage
                                            .get("completion_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        let _ = tx
                                            .send(StreamEvent::MessageDelta {
                                                stop_reason: finish_reason.and_then(|r| {
                                                    match r {
                                                        "stop" => Some(StopReason::EndTurn),
                                                        "tool_calls" => {
                                                            Some(StopReason::ToolUse)
                                                        }
                                                        "length" => {
                                                            Some(StopReason::MaxTokens)
                                                        }
                                                        _ => None,
                                                    }
                                                }),
                                                usage: Some(Usage {
                                                    input_tokens,
                                                    output_tokens,
                                                    ..Default::default()
                                                }),
                                            })
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamEvent::Error {
                                message: e.to_string(),
                            })
                            .await;
                        return;
                    }
                }
            }

            // ── Finalize (F-03) ──────────────────────────────────────
            // Reached both on `[DONE]` (via `break 'read`) and on plain
            // EOF. This is the ONLY place tool calls are emitted, so a
            // cleanly terminated stream cannot double-emit.
            // Tool calls sit after the sequential blocks in index space: the
            // original reader reserved slot 0 for text, so a thinking block
            // shifts the whole tool-call range up by one more.
            let tool_index_offset = 1 + thinking_started as usize;
            let text_index = thinking_started as usize;
            let mut emitted = 0usize;
            // Subset of `emitted` the runner can actually *dispatch*:
            // routable id/name AND arguments stream.rs will hand over as
            // real input instead of stamping `__parse_error` on. Truncated
            // arguments still get emitted (the dispatch layer echoes the
            // parse error plus the schema back to the model, F-05), but a
            // call nobody can run is not evidence the turn produced work,
            // so it must not override a truncation stop reason.
            let mut executable = 0usize;
            let mut rejected: Vec<String> = Vec::new();
            for (idx, (id, name, args)) in &tool_calls {
                // F-A3: never emit a call the dispatch layer cannot route.
                // An empty name produces "Unknown tool: "; an empty id is
                // echoed back as "tool_call_id": "" on the next request and
                // rejected with a 400, wedging the conversation permanently.
                if id.is_empty() || name.is_empty() {
                    rejected.push(format!(
                        "slot {}: id={:?} name={:?} arguments={:?}",
                        idx, id, name, args
                    ));
                    continue;
                }
                let _ = tx
                    .send(StreamEvent::ContentBlockStart {
                        index: *idx + tool_index_offset,
                        block_type: "tool_use".into(),
                        id: Some(id.clone()),
                        name: Some(name.clone()),
                    })
                    .await;
                let _ = tx
                    .send(StreamEvent::InputJsonDelta {
                        index: *idx + tool_index_offset,
                        partial_json: args.clone(),
                    })
                    .await;
                let _ = tx
                    .send(StreamEvent::ContentBlockStop {
                        index: *idx + tool_index_offset,
                    })
                    .await;
                emitted += 1;
                // Mirror stream.rs's ContentBlockStop parse exactly: empty
                // arguments are a no-argument call (`{}`); anything that
                // deserializes is usable; only a parse failure is not.
                if args.trim().is_empty()
                    || serde_json::from_str::<serde_json::Value>(args).is_ok()
                {
                    executable += 1;
                }
            }

            // P1-HIGH: an unusable call must not take its valid siblings
            // down with it. When something usable survived, report the loss
            // in-band on this same assistant message rather than raising a
            // stream-level Error — stream.rs short-circuits `into_response`
            // on the first error and never looks at `content_blocks`, so the
            // good calls would be silently destroyed and the turn aborted.
            // A text block reaches the model on the next request (assistant
            // `content` alongside `tool_calls`), so it can re-issue whatever
            // was dropped.
            if !rejected.is_empty() && emitted > 0 {
                tracing::warn!(
                    rejected = rejected.len(),
                    emitted,
                    "provider emitted unusable tool call(s); keeping the valid ones"
                );
                let note = format!(
                    "{}[cersei] dropped {} unusable tool call(s) (empty id or name): {}. \
                     {} valid call(s) were kept; re-issue the dropped one(s) if you \
                     still need them.",
                    if text_started { "\n\n" } else { "" },
                    rejected.len(),
                    rejected.join("; "),
                    emitted
                );
                if !text_started {
                    text_started = true;
                    let _ = tx
                        .send(StreamEvent::ContentBlockStart {
                            index: text_index,
                            block_type: "text".into(),
                            id: None,
                            name: None,
                        })
                        .await;
                }
                let _ = tx
                    .send(StreamEvent::TextDelta {
                        index: text_index,
                        text: note,
                    })
                    .await;
            }

            if thinking_started {
                let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
            }
            if text_started {
                let _ = tx
                    .send(StreamEvent::ContentBlockStop { index: text_index })
                    .await;
            }

            let stop = match final_stop {
                // P1-BLOCKER: `finish_reason` describes how generation
                // *ended*, not what it produced. Once a dispatchable call is
                // on the wire it must be run, and ToolUse is the only stop
                // reason that makes the runner dispatch it. Any other value
                // drops the calls while the assistant message is still
                // serialized WITH `tool_calls` (see the Role::Assistant arm
                // above), so the next request carries a tool_call that no
                // `role: "tool"` message answers -> provider 400 ->
                // CerseiError::Provider, which is not retryable -> the
                // conversation is wedged for good. "length" is the dangerous
                // one: hitting the cap on the token *after* a complete call
                // still leaves that call fully executable.
                _ if executable > 0 => StopReason::ToolUse,
                // Some servers report "stop" even when they emitted tool
                // calls; the calls are the ground truth.
                Some(StopReason::EndTurn) if emitted > 0 => StopReason::ToolUse,
                // Nothing dispatchable came out, so the provider's own
                // reason stands (a truncated call really is MaxTokens).
                Some(sr) => sr,
                None if emitted > 0 => StopReason::ToolUse,
                None => StopReason::EndTurn,
            };
            let _ = tx
                .send(StreamEvent::MessageDelta {
                    stop_reason: Some(stop),
                    usage: None,
                })
                .await;

            // Surface what the stream got wrong instead of laundering it —
            // but only kill the turn when the rejection left nothing to run.
            // The surviving-siblings case was already reported in-band above.
            if !rejected.is_empty() && emitted == 0 {
                let _ = tx
                    .send(StreamEvent::Error {
                        message: format!(
                            "provider emitted {} unusable tool call(s) \
                             (empty id or name): {}",
                            rejected.len(),
                            rejected.join("; ")
                        ),
                    })
                    .await;
            } else if !saw_done && emitted == 0 && !text_started {
                // A stream that was cut short AND yielded nothing. Without
                // this the accumulator reports a confident, empty EndTurn.
                let _ = tx
                    .send(StreamEvent::Error {
                        message: "stream ended without [DONE] and produced no content"
                            .into(),
                    })
                    .await;
            }

            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

// ─── Multimodal helpers ──────────────────────────────────────────────────────

/// Convert an [`ImageSource`] to the `image_url.url` string OpenAI expects.
/// Base64 sources become `data:` URLs; remote URL sources pass through. Returns
/// `None` for non-image media (e.g. video), which the Chat Completions API can't
/// accept, so it's dropped rather than rejected by the server.
fn openai_image_url(source: &ImageSource) -> Option<String> {
    if let Some(mt) = &source.media_type {
        if !mt.starts_with("image/") {
            return None;
        }
    }
    if let Some(url) = &source.url {
        return Some(url.clone());
    }
    let data = source.data.as_ref()?;
    let mt = source.media_type.as_deref().unwrap_or("image/png");
    Some(format!("data:{mt};base64,{data}"))
}

/// Convert a [`DocumentSource`] to an OpenAI `file` content part. Only base64
/// data is supported (sent as a `file_data` data URL); URL-only documents are
/// dropped since Chat Completions has no remote-file fetch.
fn openai_file_part(source: &DocumentSource) -> Option<serde_json::Value> {
    let data = source.data.as_ref()?;
    let mt = source.media_type.as_deref().unwrap_or("application/pdf");
    Some(serde_json::json!({
        "type": "file",
        "file": { "file_data": format!("data:{mt};base64,{data}") },
    }))
}

/// Resolve the OpenAI `reasoning_effort` request field from the provider-agnostic
/// `reasoning_effort` option, gated to models that accept it (o-series / gpt-5).
/// Returns `None` (omit the field) for non-reasoning models or when unset.
fn reasoning_effort_for(model: &str, options: &ProviderOptions) -> Option<String> {
    let reasoning_model =
        model.starts_with("gpt-5") || model.starts_with("o1") || model.starts_with("o3");
    if !reasoning_model {
        return None;
    }
    options.get::<String>("reasoning_effort")
}

// ─── Builder ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct OpenAiBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    send_num_ctx: bool,
    dialect: Option<crate::adapt::SchemaDialect>,
    reasoning_field: Option<ReasoningField>,
}

impl OpenAiBuilder {
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// F-09: opt this provider into emitting `options.num_ctx` (Ollama only).
    pub fn send_num_ctx(mut self, enabled: bool) -> Self {
        self.send_num_ctx = enabled;
        self
    }

    /// B2: schema dialect for tool serialization (default `OpenAiLoose`).
    pub fn dialect(mut self, dialect: crate::adapt::SchemaDialect) -> Self {
        self.dialect = Some(dialect);
        self
    }

    /// How the SSE reader treats reasoning deltas. Defaults to
    /// [`ReasoningField::Auto`] (auto-detect among the known shapes); pass a
    /// specific field or [`ReasoningField::Off`] for explicit per-model-family
    /// control.
    pub fn reasoning_field(mut self, field: ReasoningField) -> Self {
        self.reasoning_field = Some(field);
        self
    }

    pub fn build(self) -> Result<OpenAi> {
        let auth = if let Some(key) = self.api_key {
            Auth::ApiKey(key)
        } else {
            return Err(CerseiError::Auth(
                "No API key provided. Set OPENAI_API_KEY or use .api_key()".into(),
            ));
        };

        Ok(OpenAi {
            auth,
            base_url: self.base_url.unwrap_or_else(|| OPENAI_API_BASE.to_string()),
            default_model: self.model.unwrap_or_else(|| "gpt-4o".to_string()),
            send_num_ctx: self.send_num_ctx,
            dialect: self
                .dialect
                .unwrap_or(crate::adapt::SchemaDialect::OpenAiLoose),
            reasoning_field: self.reasoning_field.unwrap_or(ReasoningField::Auto),
            client: reqwest::Client::new(),
        })
    }
}

#[cfg(test)]
mod multimodal_tests {
    use super::*;

    #[test]
    fn base64_image_becomes_data_url() {
        let block = ContentBlock::image_base64("image/png", "QUJD");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(
            openai_image_url(&source).as_deref(),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn remote_image_url_passes_through() {
        let block = ContentBlock::image_url("https://x/y.jpg");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(openai_image_url(&source).as_deref(), Some("https://x/y.jpg"));
    }

    #[test]
    fn video_is_dropped_for_openai() {
        let block = ContentBlock::image_bytes("video/mp4", b"data");
        let ContentBlock::Image { source } = block else {
            panic!("expected image");
        };
        assert_eq!(openai_image_url(&source), None);
    }

    #[test]
    fn pdf_becomes_file_part() {
        let block = ContentBlock::document_base64("application/pdf", "UERG");
        let ContentBlock::Document { source, .. } = block else {
            panic!("expected document");
        };
        let part = openai_file_part(&source).unwrap();
        assert_eq!(part["type"], "file");
        assert_eq!(part["file"]["file_data"], "data:application/pdf;base64,UERG");
    }

    #[test]
    fn reasoning_effort_only_on_reasoning_models_when_set() {
        let mut opts = ProviderOptions::default();
        opts.set("reasoning_effort", "high");

        // Reasoning models map the option through...
        assert_eq!(reasoning_effort_for("gpt-5.3", &opts).as_deref(), Some("high"));
        assert_eq!(reasoning_effort_for("o3-mini", &opts).as_deref(), Some("high"));
        // ...non-reasoning models omit it...
        assert_eq!(reasoning_effort_for("gpt-4o", &opts), None);
        // ...and an unset option omits it even on reasoning models.
        assert_eq!(reasoning_effort_for("gpt-5.3", &ProviderOptions::default()), None);
    }
}

// These tests drive the whole OpenAI-compatible SSE reader (HTTP framing
// through event emission) against a local mock server, using bodies modeled
// on the wire shapes reasoning models actually produce — captured live from
// openrouter's stealth/ox-alpha in apps/agent-cli/tests/fixtures/reasoning/
// (see direct_prompt.rs, live-tests feature).
#[cfg(test)]
mod reasoning_tests {
    use super::*;
    use crate::stream::StreamAccumulator;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve one SSE response body through a local HTTP server and run the
    /// OpenAI-compatible reader over it (with the given reasoning field),
    /// returning the accumulated response.
    async fn run_reader_with(body: &str, field: ReasoningField) -> Result<CompletionResponse> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Drain the request headers so the client can proceed to read the
            // response; the connection close after the body frames its end.
            let mut buf = vec![0u8; 8192];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        let provider = OpenAi::builder()
            .api_key("test-key")
            .base_url(&format!("http://{addr}/v1"))
            .reasoning_field(field)
            .build()
            .unwrap();
        let request = CompletionRequest::new("stealth/ox-alpha");
        let stream = provider.complete(request).await?;
        let mut acc = StreamAccumulator::new();
        let mut rx = stream.into_receiver();
        while let Some(event) = rx.recv().await {
            acc.process_event(event);
        }
        acc.into_response()
    }

    async fn run_reader(body: &str) -> Result<CompletionResponse> {
        run_reader_with(body, ReasoningField::Auto).await
    }

    /// ox-alpha-style stream: thinking in BOTH `reasoning` (string) and
    /// `reasoning_details` (structured parts) on the same chunks, then the
    /// answer in `content`. The reader must emit the thinking as a separate
    /// block and keep it out of the answer text.
    #[tokio::test]
    async fn ox_alpha_style_stream_splits_thinking_from_answer() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"reasoning\":\"Let me \",\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"Let me \",\"format\":\"unknown\",\"index\":0}],\"content\":\"\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"think.\",\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"think.\",\"format\":\"unknown\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"The answer\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\" is 42.\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content, got {:?}", response.message.content);
        };
        assert_eq!(blocks.len(), 2, "blocks: {blocks:?}");
        let ContentBlock::Thinking { thinking, .. } = &blocks[0] else {
            panic!("expected thinking block first, got {:?}", blocks[0]);
        };
        assert_eq!(thinking, "Let me think.");
        // The string form must win over reasoning_details — the thinking
        // appears exactly once, never doubled.
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "The answer is 42."));
    }

    /// A stream where the same text appears in the answer AND in thinking
    /// (the model repeats itself) must keep the blocks distinct.
    #[tokio::test]
    async fn thinking_never_leaks_into_answer_text() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"42 is the answer\",\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"42 is the answer\",\"format\":\"unknown\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"42 is the answer\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert_eq!(blocks.len(), 2, "blocks: {blocks:?}");
        assert!(matches!(&blocks[0], ContentBlock::Thinking { thinking, .. }
            if thinking == "42 is the answer"));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "42 is the answer"));
    }

    /// DeepSeek-style endpoint: thinking in `reasoning_content`, answer in
    /// `content`, no `reasoning_details` at all.
    #[tokio::test]
    async fn reasoning_content_field_streams_thinking() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Deep think\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert!(matches!(&blocks[0], ContentBlock::Thinking { thinking, .. }
            if thinking == "Deep think"));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "Done."));
    }

    /// Qwen-style endpoint: only `reasoning_details` reasoning.text parts, no
    /// string reasoning field.
    #[tokio::test]
    async fn reasoning_details_only_streams_thinking() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"Step by step\",\"format\":\"unknown\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Result\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert!(matches!(&blocks[0], ContentBlock::Thinking { thinking, .. }
            if thinking == "Step by step"));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "Result"));
    }

    /// Encrypted reasoning parts (`encrypted_content`) must be skipped: they
    /// are not readable thinking. With no usable reasoning the answer text
    /// stays at block index 0, exactly like a non-reasoning stream.
    #[tokio::test]
    async fn encrypted_reasoning_parts_are_skipped() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"encrypted_content\",\"data\":\"ZGF0YQ==\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Plain answer\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert_eq!(blocks.len(), 1, "blocks: {blocks:?}");
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Plain answer"));
    }

    /// Regression guard: a plain content-only stream (no reasoning fields at
    /// all) must produce exactly what it did before this patch — one text
    /// block at index 0 and no thinking block.
    #[tokio::test]
    async fn plain_stream_is_unchanged() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";

        let response = run_reader(body).await.expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert_eq!(blocks.len(), 1, "blocks: {blocks:?}");
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Hello world"));
    }

    /// Reasoning followed by a tool call: the thinking block occupies index 0
    /// and the tool call must land in its own slot after it (not collide with
    /// the thinking block's index space).
    #[tokio::test]
    async fn thinking_then_tool_call_keeps_blocks_distinct() {
        // Arguments are streamed across three chunks: `{`, the key/value, `}`.
        let chunks = [
            serde_json::json!({ "choices": [{"delta": { "reasoning": "I need to read a file" }}] }),
            serde_json::json!({ "choices": [{"delta": { "tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "Read", "arguments": "{" },
            }]}}]}),
            serde_json::json!({ "choices": [{"delta": { "tool_calls": [{
                "index": 0,
                "function": { "arguments": "\"path\":\"/a.rs\"" },
            }]}}]}),
            serde_json::json!({ "choices": [{"delta": { "tool_calls": [{
                "index": 0,
                "function": { "arguments": "}" },
            }]}}]}),
            serde_json::json!({ "choices": [{ "finish_reason": "tool_calls" }] }),
        ];
        let body = chunks
            .iter()
            .map(|chunk| format!("data: {chunk}\n"))
            .collect::<String>()
            + "data: [DONE]\n";

        let response = run_reader(&body).await.expect("stream must parse");
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content, got {:?}", response.message.content);
        };
        let ContentBlock::Thinking { thinking, .. } = &blocks[0] else {
            panic!("expected thinking block first, got {:?}", blocks[0]);
        };
        assert_eq!(thinking, "I need to read a file");
        // The tool call lands in its own slot after the thinking block (the
        // reader always reserves a text slot, so an empty Text block may gap
        // it when the model went straight from reasoning to a tool call).
        let tool = blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolUse { name, input, .. } => Some((name, input)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a tool_use block, got {blocks:?}"));
        assert_eq!(tool.0, "Read");
        assert_eq!(tool.1, &serde_json::json!({"path": "/a.rs"}));
    }

    /// Config-driven `Field("reasoning_content")`: an ox-alpha body (thinking
    /// in `reasoning`) must NOT be parsed as thinking — only the configured
    /// field is read — while a deepseek body is.
    #[tokio::test]
    async fn explicit_field_reads_only_that_field() {
        let ox_alpha = "\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"ignored thinking\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Answer\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";
        let response = run_reader_with(
            ox_alpha,
            ReasoningField::Field("reasoning_content"),
        )
        .await
        .expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert_eq!(blocks.len(), 1, "blocks: {blocks:?}");
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Answer"));

        let deepseek = "\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Deep think\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";
        let response = run_reader_with(
            deepseek,
            ReasoningField::Field("reasoning_content"),
        )
        .await
        .expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert!(matches!(&blocks[0], ContentBlock::Thinking { thinking, .. }
            if thinking == "Deep think"));
    }

    /// Config-driven `Field("reasoning_details")` reads the structured parts.
    #[tokio::test]
    async fn explicit_field_can_target_structured_details() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"Step by step\",\"format\":\"unknown\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Result\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";
        let response = run_reader_with(
            body,
            ReasoningField::Field("reasoning_details"),
        )
        .await
        .expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert!(matches!(&blocks[0], ContentBlock::Thinking { thinking, .. }
            if thinking == "Step by step"));
    }

    /// Config-driven `Off` (the "plain" family): reasoning deltas are never
    /// parsed, so the answer text stays at block index 0 exactly like a
    /// non-reasoning stream.
    #[tokio::test]
    async fn off_disables_thinking_parsing() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"hidden\",\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"hidden\",\"format\":\"unknown\",\"index\":0}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Plain answer\"}}]}\n\
data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\
data: [DONE]\n";
        let response = run_reader_with(body, ReasoningField::Off)
            .await
            .expect("stream must parse");
        let MessageContent::Blocks(blocks) = &response.message.content else {
            panic!("expected block content");
        };
        assert_eq!(blocks.len(), 1, "blocks: {blocks:?}");
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Plain answer"));
    }
}
