//! The transcript model: reduces `session/update` notifications into an
//! ordered list of rows the UI can render.
//!
//! One row per logical item (a message, a tool call, a plan, a notice). Streamed
//! text grows an existing row instead of adding rows per chunk, which is what
//! lets the UI virtualize the transcript and remeasure a single row while a
//! response streams.

use std::collections::HashMap;

use agent_client_protocol::schema::v1::{
    AuthMethod, AvailableCommand, ContentBlock, PermissionOption, SessionConfigOption,
    SessionModeId, SessionModeState, SessionNotification, SessionUpdate, ToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, UsageUpdate,
};
use agent_client_protocol::schema::MaybeUndefined;

/// Status of a tool call row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

/// A file modification reported by a tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

/// One row of an agent plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRow {
    pub text: String,
    pub status: String,
}

/// Severity of a transcript notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Error,
}

/// One option the agent offered for a permission request, flattened so the UI
/// needs no protocol types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionChoice {
    pub option_id: String,
    pub name: String,
    /// `PermissionOptionKind` rendered as a label; the UI styles by this.
    pub kind: String,
}

impl PermissionChoice {
    pub fn from_options(options: &[PermissionOption]) -> Vec<Self> {
        options
            .iter()
            .map(|option| Self::from_option(option))
            .collect()
    }

    pub fn from_option(option: &PermissionOption) -> Self {
        Self {
            option_id: option.option_id.0.to_string(),
            name: option.name.clone(),
            kind: format!("{:?}", option.kind),
        }
    }

    /// Whether the option grants the operation.
    pub fn is_allow(&self) -> bool {
        self.kind == "AllowOnce" || self.kind == "AllowAlways"
    }

    /// Whether the protocol remembers this answer for the agent.
    pub fn is_sticky(&self) -> bool {
        self.kind == "AllowAlways" || self.kind == "RejectAlways"
    }
}

/// The answer recorded on a permission card, so it can collapse to one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRecord {
    pub option_id: String,
    pub name: String,
    pub kind: String,
    /// True when the client answered without asking (policy auto-decision).
    pub automatic: bool,
}

impl PermissionRecord {
    pub fn from_choice(choice: &PermissionChoice, automatic: bool) -> Self {
        Self {
            option_id: choice.option_id.clone(),
            name: choice.name.clone(),
            kind: choice.kind.clone(),
            automatic,
        }
    }

    pub fn is_allow(&self) -> bool {
        self.kind == "AllowOnce" || self.kind == "AllowAlways"
    }
}

/// An auth method the agent advertised, flattened for the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthMethodRow {
    pub id: String,
    pub name: String,
    /// `AuthMethod` variant rendered as a label: `agent` or `terminal`.
    pub kind: String,
    pub description: Option<String>,
}

impl AuthMethodRow {
    pub fn from_method(method: &AuthMethod) -> Self {
        let (kind, description) = match method {
            AuthMethod::Agent(agent) => ("agent", agent.description.clone()),
            AuthMethod::Terminal(terminal) => ("terminal", terminal.description.clone()),
            _ => ("other", None),
        };
        Self {
            id: method.id().0.to_string(),
            name: method.name().to_string(),
            kind: kind.to_string(),
            description,
        }
    }

    /// Whether this method runs a command the client must execute itself.
    pub fn is_terminal(&self) -> bool {
        self.kind == "terminal"
    }
}

/// What the agent is doing, as far as the transcript can tell: the row it is
/// writing into, or the tool call it is waiting on. The pane turns this into
/// the "something is happening" line above the prompt box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    /// A thought is streaming: the agent is reasoning before answering.
    Thinking,
    /// The answer itself is streaming.
    Writing,
    /// A tool call is running.
    Tool { title: String },
}

#[derive(Clone, Debug)]
pub enum EntryKind {
    UserMessage {
        text: String,
    },
    AgentText {
        text: String,
    },
    Thought {
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        /// `ToolKind` rendered as a label; kept as text so the UI needs no
        /// protocol types.
        kind: String,
        status: ToolStatus,
        output: Option<String>,
        diffs: Vec<FileDiff>,
        terminal_id: Option<String>,
    },
    Plan {
        rows: Vec<PlanRow>,
    },
    /// A `session/request_permission` card. Collapses to a one-line record
    /// once `decision` is set.
    Permission {
        title: String,
        options: Vec<PermissionChoice>,
        decision: Option<PermissionRecord>,
    },
    /// A pane-level failure (launch, process exit, resume).
    Error {
        title: String,
        detail: String,
    },
    /// The agent refused to continue until the user authenticates.
    AuthRequired {
        methods: Vec<AuthMethodRow>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    Usage {
        used: u64,
        size: u64,
        cost: Option<f64>,
    },
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub kind: EntryKind,
}

impl Entry {
    /// Short label for the row, used by tests and the accessibility tree.
    pub fn summary(&self) -> String {
        match &self.kind {
            EntryKind::UserMessage { text } => format!("you: {text}"),
            EntryKind::AgentText { text } => format!("agent: {text}"),
            EntryKind::Thought { text } => format!("thought: {text}"),
            EntryKind::ToolCall { title, status, .. } => format!("{title} ({status:?})"),
            EntryKind::Plan { rows } => format!("plan ({} rows)", rows.len()),
            EntryKind::Permission {
                title, decision, ..
            } => match decision {
                Some(record) => format!("permission: {} ({})", title, record.name),
                None => format!("permission: {title} (pending)"),
            },
            EntryKind::Error { title, .. } => format!("error: {title}"),
            EntryKind::AuthRequired { methods } => {
                format!("auth required ({} methods)", methods.len())
            }
            EntryKind::Notice { text, .. } => format!("notice: {text}"),
            EntryKind::Usage { used, size, .. } => format!("usage: {used}/{size}"),
        }
    }
}

/// What changed in the transcript, so the view can update minimally: append one
/// row, remeasure one row, or refresh the header/controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptDelta {
    pub appended: bool,
    pub changed: Option<usize>,
    pub controls_changed: bool,
}

impl TranscriptDelta {
    pub fn is_empty(&self) -> bool {
        !self.appended && self.changed.is_none() && !self.controls_changed
    }
}

/// Session state the pane renders outside the row list.
#[derive(Clone, Debug)]
pub struct SessionControls {
    pub commands: Vec<AvailableCommand>,
    pub mode: Option<SessionModeState>,
    pub config_options: Vec<SessionConfigOption>,
    pub title: Option<String>,
}

impl Default for SessionControls {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            mode: None,
            config_options: Vec::new(),
            title: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    /// `tool_call_id -> row index`, so updates are O(1).
    tool_calls: HashMap<String, usize>,
    /// Streaming message key -> row index. Keys are `agent:<message id>` and
    /// `thought:<message id>`, falling back to a per-kind key when the agent
    /// sends chunks without a message id.
    streaming: HashMap<String, usize>,
    /// The row currently being streamed into. Set by `push_streamed` and
    /// cleared by any other append, so a live indicator only ever marks the
    /// row the agent is writing right now.
    live: Option<usize>,
    next_entry_id: u64,
    controls: SessionControls,
    usage_row: Option<usize>,
}

impl Transcript {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub fn controls(&self) -> &SessionControls {
        &self.controls
    }

    /// Seed the controls from a `session/new` / `session/load` response, which
    /// arrives before any `session/update` notification.
    pub fn seed_controls(
        &mut self,
        mode: Option<SessionModeState>,
        config_options: Vec<SessionConfigOption>,
    ) {
        if let Some(mode) = mode {
            self.controls.mode = Some(mode);
        }
        if !config_options.is_empty() {
            self.controls.config_options = config_options;
        }
    }

    /// Replace the controls' config options with the set a
    /// `session/set_config_option` response returned: that response is the
    /// authoritative set, and an agent need not also announce the change, so a
    /// model switch opencode answers this way would otherwise be seen nowhere.
    /// An empty set is ignored — dropping every chip would be worse than
    /// showing the set the picker last had.
    pub fn set_config_options(&mut self, config_options: Vec<SessionConfigOption>) {
        if !config_options.is_empty() {
            self.controls.config_options = config_options;
        }
    }

    /// Row index of the tool call owning `terminal_id`, so streamed terminal
    /// output remeasures its row.
    pub fn index_of_terminal(&self, terminal_id: &str) -> Option<usize> {
        self.entries.iter().position(|entry| {
            matches!(
                &entry.kind,
                EntryKind::ToolCall {
                    terminal_id: Some(id),
                    ..
                } if id == terminal_id
            )
        })
    }

    /// Reset for a new conversation, keeping nothing but the session controls
    /// the next session will replace.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.tool_calls.clear();
        self.streaming.clear();
        self.live = None;
        self.usage_row = None;
        self.controls = SessionControls::default();
    }

    /// The row the agent is streaming into right now, if any.
    pub fn live_entry(&self) -> Option<usize> {
        self.live
    }

    /// Whether a permission card is unanswered in the current turn: the turn is
    /// paused on the user, not on the agent. Only the rows after the last user
    /// message count, so a card abandoned in an earlier turn (a cancelled turn
    /// leaves it unanswered) cannot keep claiming the agent is waiting.
    pub fn has_pending_permission(&self) -> bool {
        let turn_start = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry.kind, EntryKind::UserMessage { .. }))
            .map_or(0, |index| index + 1);
        self.entries[turn_start..].iter().any(|entry| {
            matches!(
                entry.kind,
                EntryKind::Permission {
                    decision: None,
                    ..
                }
            )
        })
    }

    /// What the agent is doing, from the transcript's point of view. `None`
    /// means nothing is visibly in flight — the pane falls back to its own
    /// busy flag (the turn has started but no content has arrived yet).
    pub fn activity(&self) -> Option<Activity> {
        if let Some(index) = self.live
            && let Some(entry) = self.entries.get(index)
        {
            return Some(match entry.kind {
                EntryKind::Thought { .. } => Activity::Thinking,
                _ => Activity::Writing,
            });
        }
        match self.entries.last().map(|entry| &entry.kind) {
            Some(EntryKind::ToolCall { title, status, .. }) => match status {
                ToolStatus::Pending | ToolStatus::Running => Some(Activity::Tool {
                    title: title.clone(),
                }),
                ToolStatus::Completed | ToolStatus::Failed => None,
            },
            _ => None,
        }
    }

    /// Record the user's own message. Agents usually echo it back as a
    /// `UserMessageChunk`, which [`Transcript::apply`] deliberately ignores so
    /// the prompt does not appear twice.
    pub fn push_user_message(&mut self, text: impl Into<String>) -> usize {
        self.append(EntryKind::UserMessage { text: text.into() })
    }

    pub fn push_notice(&mut self, level: NoticeLevel, text: impl Into<String>) -> usize {
        self.append(EntryKind::Notice {
            level,
            text: text.into(),
        })
    }

    pub fn push_error(&mut self, text: impl Into<String>) -> usize {
        self.push_notice(NoticeLevel::Error, text)
    }

    /// Add a permission card, returning the row's entry id so the answer can
    /// be routed back to it.
    pub fn push_permission(
        &mut self,
        title: impl Into<String>,
        options: Vec<PermissionChoice>,
    ) -> u64 {
        let index = self.append(EntryKind::Permission {
            title: title.into(),
            options,
            decision: None,
        });
        self.entries[index].id
    }

    /// Collapse a permission card to the answer it received. Returns the row
    /// index, so the view can remeasure just that row.
    pub fn resolve_permission(
        &mut self,
        entry_id: u64,
        decision: PermissionRecord,
    ) -> Option<usize> {
        let index = self.index_of(entry_id)?;
        let entry = self.entries.get_mut(index)?;
        if let EntryKind::Permission { decision: slot, .. } = &mut entry.kind {
            *slot = Some(decision);
        }
        Some(index)
    }

    /// Add a failure card for the launch/session error state.
    pub fn push_error_entry(
        &mut self,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> usize {
        self.append(EntryKind::Error {
            title: title.into(),
            detail: detail.into(),
        })
    }

    /// Add the auth-required state with the agent's advertised methods.
    pub fn push_auth_required(&mut self, methods: Vec<AuthMethodRow>) -> usize {
        self.append(EntryKind::AuthRequired { methods })
    }

    /// Row index of an entry id, for remeasuring a single row.
    pub fn index_of(&self, entry_id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == entry_id)
    }

    /// The most recent usage row, rendered next to the prompt box.
    pub fn latest_usage(&self) -> Option<(u64, u64, Option<f64>)> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match entry.kind {
                EntryKind::Usage { used, size, cost } => Some((used, size, cost)),
                _ => None,
            })
    }

    fn append(&mut self, kind: EntryKind) -> usize {
        let id = self.next_entry_id;
        self.next_entry_id += 1;
        self.entries.push(Entry { id, kind });
        // Anything appended outside the stream ends the previous live row:
        // a tool call or a fresh block means the last one is finished.
        self.live = None;
        self.entries.len() - 1
    }

    /// Reduce one `session/update` notification.
    pub fn apply(&mut self, notification: &SessionNotification) -> TranscriptDelta {
        self.apply_update(&notification.update)
    }

    pub fn apply_update(&mut self, update: &SessionUpdate) -> TranscriptDelta {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                let text = text_of(&chunk.content);
                let key = match &chunk.message_id {
                    Some(id) => format!("agent:{}", id.0),
                    None => "agent".to_string(),
                };
                self.push_streamed(&key, EntryKind::AgentText { text })
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                let text = text_of(&chunk.content);
                let key = match &chunk.message_id {
                    Some(id) => format!("thought:{}", id.0),
                    None => "thought".to_string(),
                };
                self.push_streamed(&key, EntryKind::Thought { text })
            }
            SessionUpdate::ToolCall(tool_call) => self.upsert_tool_call(tool_call),
            SessionUpdate::ToolCallUpdate(update) => self.update_tool_call(update),
            SessionUpdate::Plan(plan) => {
                let rows = plan
                    .entries
                    .iter()
                    .map(|entry| PlanRow {
                        text: entry.content.clone(),
                        status: format!("{:?}", entry.status),
                    })
                    .collect::<Vec<_>>();
                match self
                    .entries
                    .iter()
                    .position(|entry| matches!(entry.kind, EntryKind::Plan { .. }))
                {
                    Some(index) => {
                        if let Some(entry) = self.entries.get_mut(index) {
                            entry.kind = EntryKind::Plan { rows };
                        }
                        TranscriptDelta {
                            changed: Some(index),
                            ..Default::default()
                        }
                    }
                    None => {
                        let index = self.append(EntryKind::Plan { rows });
                        TranscriptDelta {
                            appended: true,
                            changed: Some(index),
                            controls_changed: false,
                        }
                    }
                }
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.controls.commands = update.available_commands.clone();
                TranscriptDelta {
                    controls_changed: true,
                    ..Default::default()
                }
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                self.set_current_mode(update.current_mode_id.clone());
                TranscriptDelta {
                    controls_changed: true,
                    ..Default::default()
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.controls.config_options = update.config_options.clone();
                TranscriptDelta {
                    controls_changed: true,
                    ..Default::default()
                }
            }
            SessionUpdate::SessionInfoUpdate(update) => {
                if let MaybeUndefined::Value(title) = &update.title {
                    self.controls.title = Some(title.clone());
                }
                TranscriptDelta {
                    controls_changed: true,
                    ..Default::default()
                }
            }
            SessionUpdate::UsageUpdate(update) => self.apply_usage(update),
            // Unknown and unstable updates are ignored on purpose: this client
            // does not enable the `unstable_*` schema features.
            _ => TranscriptDelta::default(),
        }
    }

    fn set_current_mode(&mut self, mode_id: SessionModeId) {
        match &mut self.controls.mode {
            Some(state) => state.current_mode_id = mode_id,
            None => {
                self.controls.mode = Some(SessionModeState::new(mode_id, Vec::new()));
            }
        }
    }

    fn apply_usage(&mut self, update: &UsageUpdate) -> TranscriptDelta {
        let kind = EntryKind::Usage {
            used: update.used,
            size: update.size,
            cost: update.cost.as_ref().map(|cost| cost.amount),
        };
        match self.usage_row {
            Some(index) if index < self.entries.len() => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.kind = kind;
                }
                TranscriptDelta {
                    changed: Some(index),
                    ..Default::default()
                }
            }
            _ => {
                let index = self.append(kind);
                self.usage_row = Some(index);
                TranscriptDelta {
                    appended: true,
                    changed: Some(index),
                    controls_changed: false,
                }
            }
        }
    }

    fn push_streamed(&mut self, key: &str, kind: EntryKind) -> TranscriptDelta {
        let text = match &kind {
            EntryKind::AgentText { text } | EntryKind::Thought { text } => text.clone(),
            _ => String::new(),
        };
        if let Some(index) = self.streaming.get(key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                match &mut entry.kind {
                    EntryKind::AgentText { text: existing }
                    | EntryKind::Thought { text: existing } => existing.push_str(&text),
                    _ => entry.kind = kind,
                }
                self.live = Some(index);
                return TranscriptDelta {
                    changed: Some(index),
                    ..Default::default()
                };
            }
        }
        let index = self.append(kind);
        self.streaming.insert(key.to_string(), index);
        self.live = Some(index);
        TranscriptDelta {
            appended: true,
            changed: Some(index),
            controls_changed: false,
        }
    }

    fn upsert_tool_call(&mut self, tool_call: &ToolCall) -> TranscriptDelta {
        let id = tool_call.tool_call_id.0.to_string();
        let kind = EntryKind::ToolCall {
            tool_call_id: id.clone(),
            title: tool_call.title.clone(),
            kind: format!("{:?}", tool_call.kind),
            status: status_of(&tool_call.status),
            output: String::new().into(),
            diffs: Vec::new(),
            terminal_id: None,
        };
        let (index, appended) = match self.tool_calls.get(&id).copied() {
            Some(index) if index < self.entries.len() => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.kind = kind;
                }
                (index, false)
            }
            _ => {
                let index = self.append(kind);
                self.tool_calls.insert(id, index);
                (index, true)
            }
        };
        let (output, diffs, terminal_id) = collect_content(&tool_call.content);
        if let Some(EntryKind::ToolCall {
            output: row_output,
            diffs: row_diffs,
            terminal_id: row_terminal,
            ..
        }) = self.entries.get_mut(index).map(|entry| &mut entry.kind)
        {
            *row_output = output;
            *row_diffs = diffs;
            *row_terminal = terminal_id;
        }
        TranscriptDelta {
            appended,
            changed: Some(index),
            controls_changed: false,
        }
    }

    fn update_tool_call(&mut self, update: &ToolCallUpdate) -> TranscriptDelta {
        let id = update.tool_call_id.0.to_string();
        let index = match self.tool_calls.get(&id).copied() {
            Some(index) if index < self.entries.len() => index,
            _ => {
                // An update may arrive before the call itself; create the row
                // so nothing is lost.
                let index = self.append(EntryKind::ToolCall {
                    tool_call_id: id.clone(),
                    title: String::new(),
                    kind: "Other".to_string(),
                    status: ToolStatus::Pending,
                    output: None,
                    diffs: Vec::new(),
                    terminal_id: None,
                });
                self.tool_calls.insert(id, index);
                index
            }
        };
        let (output, diffs, terminal_id) =
            collect_content(update.fields.content.as_deref().unwrap_or_default());
        if let Some(EntryKind::ToolCall {
            title,
            kind,
            status,
            output: row_output,
            diffs: row_diffs,
            terminal_id: row_terminal,
            ..
        }) = self.entries.get_mut(index).map(|entry| &mut entry.kind)
        {
            if let Some(new_title) = &update.fields.title {
                *title = new_title.clone();
            }
            if let Some(new_kind) = &update.fields.kind {
                *kind = format!("{new_kind:?}");
            }
            if let Some(new_status) = &update.fields.status {
                *status = status_of(new_status);
            }
            if let Some(output) = output {
                *row_output = Some(output);
            }
            if !diffs.is_empty() {
                *row_diffs = diffs;
            }
            if let Some(terminal_id) = terminal_id {
                *row_terminal = Some(terminal_id);
            }
        }
        TranscriptDelta {
            changed: Some(index),
            ..Default::default()
        }
    }
}

fn status_of(status: &ToolCallStatus) -> ToolStatus {
    match status {
        ToolCallStatus::Pending => ToolStatus::Pending,
        ToolCallStatus::InProgress => ToolStatus::Running,
        ToolCallStatus::Completed => ToolStatus::Completed,
        ToolCallStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

fn text_of(content: &ContentBlock) -> String {
    match content {
        ContentBlock::Text(text) => text.text.clone(),
        _ => String::new(),
    }
}

/// Flatten a tool call's content into the pieces the pane renders.
fn collect_content(content: &[ToolCallContent]) -> (Option<String>, Vec<FileDiff>, Option<String>) {
    let mut output = String::new();
    let mut diffs = Vec::new();
    let mut terminal_id = None;
    for item in content {
        match item {
            ToolCallContent::Content(item) => {
                if let ContentBlock::Text(text) = &item.content {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&text.text);
                }
            }
            ToolCallContent::Diff(diff) => diffs.push(FileDiff {
                path: diff.path.display().to_string(),
                old_text: diff.old_text.clone(),
                new_text: diff.new_text.clone(),
            }),
            ToolCallContent::Terminal(terminal) => {
                terminal_id = Some(terminal.terminal_id.0.to_string());
            }
            _ => {}
        }
    }
    ((!output.is_empty()).then_some(output), diffs, terminal_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ContentChunk, SessionConfigKind, TextContent};

    /// A `model`-shaped select option, the way an agent advertises one.
    fn model_option(current: &str) -> SessionConfigOption {
        serde_json::from_value(serde_json::json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current,
            "options": [
                {"value": "a", "name": "A"},
                {"value": "b", "name": "B"}
            ]
        }))
        .expect("model option")
    }

    fn current_model(transcript: &Transcript) -> String {
        let option = transcript
            .controls()
            .config_options
            .first()
            .expect("a config option");
        let SessionConfigKind::Select(select) = &option.kind else {
            panic!("a select option");
        };
        select.current_value.0.to_string()
    }

    fn thought(text: &str) -> SessionUpdate {
        SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text.to_string(),
        ))))
    }

    fn message(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text.to_string(),
        ))))
    }

    fn tool_call(id: &str, title: &str, status: ToolCallStatus) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(id.to_string(), title.to_string()).status(status),
        )
    }

    /// A `session/set_config_option` answer carries the full current set, and
    /// an agent need not announce the change, so folding the answer in is the
    /// only way a switch reported that way reaches the UI. An answer that
    /// carries nothing leaves the set alone.
    #[test]
    fn a_config_answer_replaces_the_set_it_returned() {
        let mut transcript = Transcript::default();
        transcript.seed_controls(None, vec![model_option("a")]);
        assert_eq!(current_model(&transcript), "a");

        transcript.set_config_options(vec![model_option("b")]);
        assert_eq!(current_model(&transcript), "b");

        transcript.set_config_options(Vec::new());
        assert_eq!(current_model(&transcript), "b");
    }

    #[test]
    fn streaming_thoughts_grow_one_row_and_stay_live() {
        let mut transcript = Transcript::default();
        let first = transcript.apply_update(&thought("Let me "));
        assert!(first.appended);
        let second = transcript.apply_update(&thought("think."));
        assert!(!second.appended, "chunks grow the row instead of appending");
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript.activity(), Some(Activity::Thinking));
        assert_eq!(transcript.live_entry(), Some(0));
        assert_eq!(transcript.entry(0).map(|row| row.summary()), Some("thought: Let me think.".to_string()));
    }

    #[test]
    fn a_finished_thought_is_no_longer_live() {
        let mut transcript = Transcript::default();
        transcript.apply_update(&thought("Considering options"));
        transcript.apply_update(&tool_call("t1", "Read src/main.rs", ToolCallStatus::InProgress));
        assert_eq!(
            transcript.live_entry(),
            None,
            "the live mark belongs to the streaming row only"
        );
        assert_eq!(
            transcript.activity(),
            Some(Activity::Tool { title: "Read src/main.rs".to_string() })
        );
        transcript.apply_update(&tool_call("t1", "Read src/main.rs", ToolCallStatus::Completed));
        assert_eq!(transcript.activity(), None, "a finished turn is idle");
    }

    #[test]
    fn writing_replaces_thinking_as_the_activity() {
        let mut transcript = Transcript::default();
        transcript.apply_update(&thought("Drafting"));
        transcript.apply_update(&message("Here is the answer"));
        assert_eq!(transcript.activity(), Some(Activity::Writing));
        assert_eq!(transcript.len(), 2, "a message is its own row");
        assert_eq!(transcript.live_entry(), Some(1));
    }

    #[test]
    fn pending_permission_is_reported_until_it_is_answered() {
        let mut transcript = Transcript::default();
        assert!(!transcript.has_pending_permission());
        let choice = PermissionChoice {
            option_id: "allow".to_string(),
            name: "Allow".to_string(),
            kind: "allow_once".to_string(),
        };
        let entry_id =
            transcript.push_permission("Write src/main.rs".to_string(), vec![choice.clone()]);
        assert!(transcript.has_pending_permission());
        transcript.resolve_permission(entry_id, PermissionRecord::from_choice(&choice, false));
        assert!(!transcript.has_pending_permission());
    }

    #[test]
    fn a_card_abandoned_in_an_earlier_turn_no_longer_counts_as_waiting() {
        let mut transcript = Transcript::default();
        let choice = PermissionChoice {
            option_id: "allow".to_string(),
            name: "Allow".to_string(),
            kind: "allow_once".to_string(),
        };
        transcript.push_permission("Write src/main.rs".to_string(), vec![choice]);
        assert!(transcript.has_pending_permission());
        // The user cancelled that turn and asked something else; the abandoned
        // card stays in the transcript but must not describe the new turn.
        transcript.push_user_message("never mind, explain the parser instead");
        assert!(!transcript.has_pending_permission());
    }

    #[test]
    fn clearing_resets_the_live_row() {
        let mut transcript = Transcript::default();
        transcript.apply_update(&thought("half a thought"));
        transcript.clear();
        assert_eq!(transcript.live_entry(), None);
        assert_eq!(transcript.activity(), None);
    }
}
