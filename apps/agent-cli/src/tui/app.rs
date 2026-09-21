//! TUI application state.

// use crate::permissions::SharedPermissionMode;
use crate::tui::scroll::ScrollState;
use cersei::tools::permissions::PermissionDecision;
use std::time::Instant;
use tokio::sync::oneshot;

/// An active `ask_user` question set waiting for user answers.
#[derive(Debug, Clone)]
pub struct AskUserPending {
    /// Matches the request id from the agent.
    pub request_id: u64,
    /// The questions to display.
    pub questions: Vec<serde_json::Value>,
    /// Cached display strings for each question.
    pub question_summaries: Vec<String>,
    /// The user's typed answers so far (free-text for each question).
    pub answers: Vec<String>,
    /// Which question is currently being edited (for navigation).
    pub focused_question: usize,
    /// The current text being typed for the focused question.
    pub current_input: String,
    /// Cursor position in `current_input`.
    pub cursor_pos: usize,
    /// Freeform text per question.
    pub custom_inputs: Vec<String>,
    /// Whether the focused question is currently editing its freeform text.
    pub custom_editing: bool,
    /// Cursor position within the focused custom input.
    pub custom_cursor: usize,
    /// Whether each question has been answered (shows elided Q+A summary).
    pub answered: Vec<bool>,
}

impl AskUserPending {
    pub fn new(
        request_id: u64,
        questions: Vec<serde_json::Value>,
        question_summaries: Vec<String>,
    ) -> Self {
        let count = questions.len();
        Self {
            request_id,
            questions,
            question_summaries,
            answers: vec![String::new(); count],
            focused_question: 0,
            current_input: String::new(),
            cursor_pos: 0,
            custom_inputs: vec![String::new(); count],
            custom_editing: true,
            custom_cursor: 0,
            answered: vec![false; count],
        }
    }
}

/// A single message turn in the conversation.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    /// Assistant turns render from these ordered blocks — text, thinking, and
    /// tool calls in the order they happened, so the output box shows the
    /// turn exactly as it unfolded instead of all text and all tools in two
    /// separate piles. Empty for User/System turns.
    pub blocks: Vec<OutputBlock>,
    /// Plain text for User/System turns.
    pub content: String,
    /// Suggested next steps from the `suggest_followups` tool, rendered as
    /// clickable rows at the end of a System turn. Empty for all other turns.
    pub followups: Vec<crate::subagents::Followup>,
}

/// Transient feedback after a copy attempt, shown in the one-line strip above
/// the input box until it times out (see [`CopyFeedback::expired`]). Set on a
/// copy attempt; the draw loop reads it via [`AppState::copy_feedback_message`]
/// and it self-expires after [`COPY_FEEDBACK_FRAMES`] (~2.5 s at the 16 ms
/// tick rate used by the draw loop).
#[derive(Debug, Clone)]
pub struct CopyFeedback {
    /// The frame (`state.frame_count`) the feedback was set on. None before the
    /// first copy attempt (never shown).
    pub frame: Option<u64>,
    /// Whether the clipboard write succeeded.
    pub copied: bool,
    /// True when the copy shortcut was pressed with no active selection.
    pub nothing_selected: bool,
    /// A clipboard error message, when the copy tool failed.
    pub error: Option<String>,
    /// A one-line preview of the copied text, echoed back as `copied: …`
    /// (elided to the strip width by the renderer). None when nothing was
    /// selected.
    pub snippet: Option<String>,
}

/// How many frames the copy feedback stays visible: 2.5 s at the 16 ms draw
/// tick (157 frames ≈ 2.51 s).
pub const COPY_FEEDBACK_FRAMES: u64 = 157;

impl CopyFeedback {
    /// An empty feedback: never shown (expired immediately), used before the
    /// first copy attempt.
    pub fn none() -> Self {
        Self {
            frame: None,
            copied: false,
            nothing_selected: false,
            error: None,
            snippet: None,
        }
    }

    /// Whether the feedback is past its display window at `now` and should no
    /// longer be drawn.
    pub fn expired(&self, now: u64) -> bool {
        match self.frame {
            None => true,
            Some(frame) => now.wrapping_sub(frame) >= COPY_FEEDBACK_FRAMES,
        }
    }

    /// Whether the draw loop should paint this frame: true during the display
    /// window *and* on the frame the feedback expires, so the strip is redrawn
    /// with its normal content before rendering stops (otherwise the last
    /// feedback frame would linger on screen indefinitely).
    pub fn needs_redraw(&self, now: u64) -> bool {
        match self.frame {
            None => false,
            Some(frame) => now.wrapping_sub(frame) <= COPY_FEEDBACK_FRAMES,
        }
    }

    /// The feedback strip line for this attempt (None once expired): a green
    /// `copied: {text}`, an amber failure line, or an amber hint when there
    /// was no selection to copy.
    pub fn message(&self, now: u64) -> Option<String> {
        if self.expired(now) {
            return None;
        }
        let snippet = self.snippet.as_deref().unwrap_or_default();
        if self.copied {
            Some(format!("copied: {snippet}"))
        } else if let Some(err) = &self.error {
            Some(format!("copy failed: {err}"))
        } else {
            Some("nothing selected — drag over text to select it first".into())
        }
    }
}

#[cfg(test)]
mod copy_feedback_tests {
    use super::{COPY_FEEDBACK_FRAMES, CopyFeedback};

    #[test]
    fn feedback_expires_after_window_frames() {
        let fb = CopyFeedback {
            frame: Some(100),
            copied: true,
            nothing_selected: false,
            error: None,
            snippet: Some("hello".into()),
        };
        // Within the window the message shows (echoing the snippet); on the
        // frame right after the window it is gone.
        assert_eq!(fb.message(100), Some("copied: hello".into()));
        assert_eq!(
            fb.message(100 + COPY_FEEDBACK_FRAMES - 1),
            Some("copied: hello".into())
        );
        assert_eq!(fb.message(100 + COPY_FEEDBACK_FRAMES), None);
        assert_eq!(fb.message(u64::MAX), None);
    }

    #[test]
    fn feedback_repaints_once_on_the_expiry_frame() {
        let fb = CopyFeedback {
            frame: Some(100),
            copied: true,
            nothing_selected: false,
            error: None,
            snippet: Some("x".into()),
        };
        // The expiry frame still needs one repaint so the strip reverts to its
        // normal status text; only afterwards can rendering stop.
        assert!(fb.needs_redraw(100 + COPY_FEEDBACK_FRAMES));
        assert!(!fb.needs_redraw(100 + COPY_FEEDBACK_FRAMES + 1));
        assert!(!fb.expired(100 + COPY_FEEDBACK_FRAMES - 1));
        assert!(fb.expired(100 + COPY_FEEDBACK_FRAMES));
    }

    #[test]
    fn failure_and_no_selection_messages() {
        let failed = CopyFeedback {
            frame: Some(5),
            copied: false,
            nothing_selected: false,
            error: Some("pbcopy is not installed".into()),
            snippet: None,
        };
        assert_eq!(
            failed.message(5),
            Some("copy failed: pbcopy is not installed".into())
        );
        let empty = CopyFeedback {
            frame: Some(5),
            copied: false,
            nothing_selected: true,
            error: None,
            snippet: None,
        };
        assert_eq!(
            empty.message(5),
            Some("nothing selected — drag over text to select it first".into())
        );
    }

    #[test]
    fn empty_feedback_is_never_shown() {
        let fb = CopyFeedback::none();
        assert_eq!(fb.message(0), None);
        assert!(!fb.needs_redraw(0));
        assert!(fb.expired(0));
        assert!(fb.expired(u64::MAX));
    }
}

/// Append `text` to `out` for a single-line preview: control characters and
/// runs of whitespace collapse to a single space, and appending stops once
/// `out` reaches `cap` bytes. Callers that only show a few dozen characters
/// never pay for scanning content beyond the cap.
fn append_preview(out: &mut String, text: &str, cap: usize) {
    for ch in text.chars() {
        if out.len() >= cap {
            return;
        }
        if ch.is_whitespace() || ch.is_control() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
}

/// Flatten `text` into a single trimmed line of at most `cap` bytes, used for
/// the `selected:`/`copied:` feedback snippets. Multi-line selections become
/// one line (newlines and indentation collapse to spaces).
pub(crate) fn preview_line(text: &str, cap: usize) -> String {
    let mut out = String::new();
    append_preview(&mut out, text, cap);
    let trimmed = out.trim_end();
    trimmed.to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

/// A tool invocation with its status.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub input_summary: String,
    pub status: ToolStatus,
    pub output_preview: Option<String>,
    pub started_at: Instant,
    pub duration_ms: Option<u64>,
    /// Sub-agent tool calls rendered nested under this call (only set on
    /// `spawn_agents` calls).
    pub children: Vec<ToolCall>,
    /// The sub-agent run this call belongs to, for associating forwarded
    /// activity events with the right `spawn_agents` parent call.
    pub run_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

/// One unit of assistant output, kept in arrival order: a text chunk, a
/// thinking chunk, or a tool call. Consecutive text (or thinking) deltas are
/// coalesced into a single block; tool calls get their own block so the UI
/// can interleave them with the surrounding prose exactly as the agent
/// produced them.
#[derive(Debug, Clone)]
pub enum OutputBlock {
    Text(String),
    Thinking(String),
    Tool(ToolCall),
}

impl OutputBlock {
    /// The tool call inside this block, if it is one.
    pub fn as_tool(&self) -> Option<&ToolCall> {
        match self {
            OutputBlock::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// Mutable access to the tool call inside this block, if it is one.
    pub fn as_tool_mut(&mut self) -> Option<&mut ToolCall> {
        match self {
            OutputBlock::Tool(tool) => Some(tool),
            _ => None,
        }
    }
}

/// Provider/model picker shown by `/model`.
#[derive(Debug, Clone)]
pub struct ModelPickerState {
    /// (provider, model) entries in display order.
    pub entries: Vec<(String, String)>,
    /// Fuzzy query typed in the filter box.
    pub query: String,
    pub selected: usize,
    /// Joined "provider/model" id of the current selection (for the marker).
    pub current: String,
}

impl ModelPickerState {
    /// Entries matching the query, best matches first. Virtual combos rank
    /// above concrete models so the fallback chains are always at the top;
    /// within each group the fuzzy score orders matches.
    pub fn filtered(&self) -> Vec<&(String, String)> {
        let mut scored: Vec<(u32, &(String, String))> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let id = crate::providers::display_model_id(&entry.0, &entry.1);
                fuzzy_score(&self.query, &id).map(|score| (score, entry))
            })
            .collect();
        scored.sort_by(|(score_a, entry_a), (score_b, entry_b)| {
            let a_combo = entry_a.0 == "combos";
            let b_combo = entry_b.0 == "combos";
            b_combo
                .cmp(&a_combo)
                .then_with(|| score_a.cmp(score_b))
                .then_with(|| {
                    crate::providers::display_model_id(&entry_a.0, &entry_a.1)
                        .cmp(&crate::providers::display_model_id(&entry_b.0, &entry_b.1))
                })
        });
        scored.into_iter().map(|(_, entry)| entry).collect()
    }
}

/// Fuzzy combo picker shown by `/combos`: one row per configured combo
/// (virtual provider/model, routed across its fallback entries). Typing
/// filters the list; Enter switches the runtime to the selected combo.
#[derive(Debug, Clone)]
pub struct ComboPickerState {
    /// Configured combos, in config order.
    pub combos: Vec<crate::providers::Combo>,
    /// Fuzzy query typed in the filter box.
    pub query: String,
    pub selected: usize,
    /// Name of the active combo, if the current selection is a combo (for
    /// the marker).
    pub current: Option<String>,
}

impl ComboPickerState {
    /// Combos matching the query, best matches first.
    pub fn filtered(&self) -> Vec<&crate::providers::Combo> {
        let mut scored: Vec<(u32, &crate::providers::Combo)> = self
            .combos
            .iter()
            .filter_map(|c| fuzzy_score(&self.query, &c.name).map(|s| (s, c)))
            .collect();
        scored.sort_by_key(|(s, c)| (*s, c.name.clone()));
        scored.into_iter().map(|(_, c)| c).collect()
    }
}

/// A single command shown in the fuzzy `/` selector.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandMatch {
    pub name: String,
    pub description: &'static str,
}

/// Fuzzy command selector popup shown while typing a `/` command.
#[derive(Debug, Clone)]
pub struct CommandSelectorState {
    /// The query that produced these matches (text after `/`).
    pub query: String,
    pub matches: Vec<CommandMatch>,
    pub selected: usize,
}

/// All slash commands, in display order. Must mirror `handle_slash_command`.
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("model", "Switch provider/model"),
    ("combos", "List/switch combos"),
    ("why", "Explain the last model choice"),
    ("provider", "Browse provider models"),
    ("interview", "Run an interview and write a spec"),
    ("default-config", "Show default config in editor"),
    ("help", "Show help"),
    ("clear", "Clear conversation"),
    ("panel", "Toggle side panel"),
    ("diff", "Open git diff panel"),
    ("files", "Open file tree panel"),
    ("rewind", "Rewind last turn"),
    ("memory", "Memory info"),
    ("compact", "Context compaction info"),
    ("proxy", "Proxy status"),
    ("exit", "Exit"),
];

/// Subsequence-match `query` against `candidate`; lower score is a better match.
fn fuzzy_score(query: &str, candidate: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let mut qi = 0;
    let mut score: u32 = 0;
    let mut prev: Option<usize> = None;
    for (i, c) in candidate.chars().enumerate() {
        if qi < q.len() && c == q[qi] {
            // Matches at the start and adjacent matches score better.
            score += match prev {
                Some(p) => (i - p) as u32 + 1,
                None => (i as u32) * 2 + 1,
            };
            prev = Some(i);
            qi += 1;
        }
    }
    if qi == q.len() { Some(score) } else { None }
}

/// Commands matching `query`, best matches first.
pub fn filter_commands(query: &str) -> Vec<CommandMatch> {
    let mut scored: Vec<(u32, CommandMatch)> = SLASH_COMMANDS
        .iter()
        .filter_map(|(name, description)| {
            fuzzy_score(query, name).map(|score| {
                (
                    score,
                    CommandMatch {
                        name: name.to_string(),
                        description,
                    },
                )
            })
        })
        .collect();
    scored.sort_by_key(|(score, m)| (*score, m.name.clone()));
    scored.into_iter().map(|(_, m)| m).collect()
}

/// Overlay currently displayed on top of the main content.
#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Help,
    Permission(PermissionOverlay),
    Recovery(RecoveryOverlay),
    ModelPicker(ModelPickerState),
    ComboPicker(ComboPickerState),
    ProviderExplorer(ProviderExplorerState),
    Graph(crate::tui::widgets::graph::GraphOverlayState),
    AskUser(AskUserPending),
}

impl PartialEq for Overlay {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::None, Self::None)
                | (Self::Help, Self::Help)
                | (Self::Permission(_), Self::Permission(_))
                | (Self::Recovery(_), Self::Recovery(_))
                | (Self::ModelPicker(_), Self::ModelPicker(_))
                | (Self::ComboPicker(_), Self::ComboPicker(_))
                | (Self::ProviderExplorer(_), Self::ProviderExplorer(_))
                | (Self::Graph(_), Self::Graph(_))
                | (Self::AskUser(_), Self::AskUser(_))
        )
    }
}

/// Result channel for a `/provider` model-list fetch: the provider name plus
/// the fetched model ids (or an error string).
pub type ProviderFetchRx = tokio::sync::oneshot::Receiver<(String, Result<Vec<String>, String>)>;

/// Which list the `/provider` explorer is currently fuzzy-picking from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderExplorerPhase {
    /// Picking a provider from the configured list.
    Providers,
    /// Picking a model from the selected provider's `/models` list.
    Models,
}

/// Provider/model explorer shown by `/provider`: first fuzzy-pick a provider
/// from the configured list, then fuzzy-pick one of its models (fetched from
/// the provider's OpenAI-compatible `/models` endpoint and cached in memory
/// for the whole run). Selecting a model switches the runtime to it live.
#[derive(Debug, Clone)]
pub struct ProviderExplorerState {
    pub phase: ProviderExplorerPhase,
    /// Fuzzy query for the current phase's list.
    pub query: String,
    /// Index into the current phase's filtered list.
    pub selected: usize,
    /// Providers to pick from (the virtual `combos` provider excluded).
    pub providers: Vec<crate::providers::Provider>,
    /// The provider being browsed in the Models phase.
    pub provider: Option<crate::providers::Provider>,
    /// Name of the runtime's current provider, for the marker in the list.
    pub current_provider: Option<String>,
    /// Full unfiltered model list from the provider's `/models` endpoint
    /// (empty while loading or on error).
    pub all_models: Vec<String>,
    /// `true` while the `/models` fetch is in flight.
    pub loading: bool,
    /// Error message when the fetch failed (empty otherwise).
    pub error: String,
}

impl ProviderExplorerState {
    /// The filtered providers for the current query, best matches first.
    pub fn filtered_providers(&self) -> Vec<&crate::providers::Provider> {
        let mut scored: Vec<(u32, &crate::providers::Provider)> = self
            .providers
            .iter()
            .filter_map(|p| fuzzy_score(&self.query, &p.name).map(|s| (s, p)))
            .collect();
        scored.sort_by_key(|(s, p)| (*s, p.name.clone()));
        scored.into_iter().map(|(_, p)| p).collect()
    }

    /// The filtered model list for the current query, best matches first.
    pub fn filtered_models(&self) -> Vec<&str> {
        let mut scored: Vec<(u32, &String)> = self
            .all_models
            .iter()
            .filter_map(|m| fuzzy_score(&self.query, m).map(|s| (s, m)))
            .collect();
        scored.sort_by_key(|(s, m)| (*s, (*m).clone()));
        scored.into_iter().map(|(_, m)| m.as_str()).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOverlay {
    pub tool_name: String,
    pub description: String,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryOverlay {
    pub error_msg: String,
    pub options: Vec<String>,
    pub selected: usize,
}

/// Permission mode (Shift+Tab to cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Auto,
    Plan,
    Editor,
    Bypass,
    BypassAlert,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Plan,
            Self::Plan => Self::Editor,
            Self::Editor => Self::Bypass,
            Self::Bypass => Self::BypassAlert,
            Self::BypassAlert => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Plan => "Plan",
            Self::Editor => "Editor",
            Self::Bypass => "Bypass",
            Self::BypassAlert => "Bypass+Alert",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Ask for permissions interactively",
            Self::Plan => "Read-only: plan without executing",
            Self::Editor => "All permissions except shell commands",
            Self::Bypass => "Bypass all permissions",
            Self::BypassAlert => "Bypass all, notify on shell commands",
        }
    }
}

/// Side panel tab selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanelTab {
    GitDiff,
    FileTree,
}

impl SidePanelTab {
    pub fn toggle(self) -> Self {
        match self {
            Self::GitDiff => Self::FileTree,
            Self::FileTree => Self::GitDiff,
        }
    }
}

/// Which widget a mouse selection lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTarget {
    Input,
    Output,
}

/// One endpoint of a mouse selection, in the coordinate space of the target
/// widget's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPoint {
    /// Byte offset into `AppState::input`.
    Input(usize),
    /// (virtual-list row index, grapheme-cluster index into that row's
    /// rendered text). A grapheme index counts user-perceived characters
    /// (so it never splits a codepoint or a combining-mark cluster); it is
    /// resolved to a byte offset via `VirtualList::row_grapheme_bytes`.
    Output(usize, usize),
}

/// Mouse-driven text selection. `anchor` is where the drag started, `active`
/// is the current drag endpoint; they are equal before any text is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub target: SelectionTarget,
    pub anchor: SelectionPoint,
    pub active: SelectionPoint,
    /// True while the mouse button is held down.
    pub dragging: bool,
}

/// A normalized output selection: rows `start_row..=end_row`, grapheme
/// indices `start_col..end_col` on the boundary rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSelectionRange {
    pub start_row: u16,
    pub start_col: usize,
    pub end_row: u16,
    pub end_col: usize,
}

/// Full application state for the TUI.
pub struct AppState {
    // ── Conversation ──
    pub turns: Vec<Turn>,
    /// The in-progress assistant turn as an ordered block list (text,
    /// thinking, and tool calls interleaved in arrival order). Committed into
    /// a `Turn` by [`AppState::commit_turn`].
    pub active_blocks: Vec<OutputBlock>,
    pub is_streaming: bool,
    /// Sub-agent activity stream (forwarded from the `spawn_agents` tool).
    pub subagent_rx: Option<tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity>>,
    /// Sub-agent events that arrived before their `spawn_agents` parent call
    /// was seen (drained when the parent ToolStart is processed).
    pub pending_subagent: Vec<(u64, crate::subagents::SubAgentActivity)>,

    // ── Input ──
    pub input: String,
    pub cursor_pos: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    /// Last drawn input box rect as (x, y, width, height) — for mouse hit-testing.
    pub input_area: Option<(u16, u16, u16, u16)>,
    /// Last drawn messages (output) rect as (x, y, width, height) — for mouse hit-testing.
    pub messages_area: Option<(u16, u16, u16, u16)>,
    /// Last drawn feedback/status strip rect as (x, y, width, height). The
    /// strip sits directly above the input box; clicking it copies the active
    /// selection (a mouse path that works even in terminals that intercept
    /// Cmd+C).
    pub feedback_area: Option<(u16, u16, u16, u16)>,
    /// Vertical scroll of the input content from the last frame — for mouse hit-testing.
    pub input_scroll: u16,
    /// Cursor position the input scroll was last aligned to. When the cursor
    /// moves (typing/navigation/click) the view follows it again; otherwise a
    /// wheel-scrolled offset is preserved.
    pub input_scroll_cursor: usize,
    /// Active mouse text selection, if any.
    pub selection: Option<Selection>,

    // ── Scroll + Virtual List ──
    pub scroll: ScrollState,
    pub virtual_list: crate::tui::virtual_list::VirtualList,
    pub messages_dirty: bool,

    // ── Followups (next steps) ──
    /// Parallel to the committed virtual-list rows: which followup
    /// `(turn index, followup index)` each row belongs to, if any. Rebuilt
    /// together with the committed lines so clicks and hovers can map a row
    /// back to the followup that produced it.
    pub followup_rows: Vec<Option<(usize, usize)>>,
    /// The followup the mouse is currently hovering, if any.
    pub hovered_followup: Option<(usize, usize)>,
    /// A followup prompt awaiting submission: set when a followup row is
    /// clicked, drained by the run loop like the Enter key's returned prompt.
    pub pending_prompt: Option<String>,

    // ── Status ──
    pub model: String,
    /// Display id ("provider/model") of the concrete model the agent runs on.
    /// Differs from `model` while a combo is active (e.g. after the combo
    /// fell back to another entry); the header shows it after the selection.
    pub effective_model: Option<String>,
    // pub session_id: String,
    // pub effort: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub context_pct: f64,
    pub turn_count: u32,
    pub tool_count: u32,
    pub stream_start: Option<Instant>,

    // ── Permission mode ──
    pub permission_mode: PermissionMode,
    // pub shared_permission_mode: Option<SharedPermissionMode>,

    // ── Side panel ──
    pub side_panel_open: bool,
    pub side_panel_focused: bool,
    pub side_panel_tab: SidePanelTab,
    pub side_panel_scroll: ScrollState,
    pub side_panel_diff: String,
    pub side_panel_tree: String,

    // ── Overlay ──
    pub overlay: Overlay,
    /// Pending permission response sender (for TUI-based permission flow).
    pub pending_permission_tx: Option<oneshot::Sender<PermissionDecision>>,

    // ── Command selector (fuzzy `/` popup) ──
    pub command_selector: Option<CommandSelectorState>,
    /// In-flight `/provider` model-list fetch. Stored so dropping the overlay
    /// cancels the fetch.
    pub _provider_fetch_task: Option<tokio::task::JoinHandle<()>>,
    /// Receiver for the `/provider` fetch result; the tick loop drains it.
    pub provider_fetch_rx: Option<ProviderFetchRx>,

    // ── Animation ──
    pub frame_count: u64,

    // ── Interview flow ──
    /// When set, the next user message is treated as the interview target
    /// instead of a normal chat message.
    pub pending_interview_target: Option<String>,
    /// When set, the run loop suspends the TUI and opens this file in the
    /// user's editor (`$EDITOR` / `$VISUAL`).
    pub pending_editor: Option<std::path::PathBuf>,
    /// Buffer for ask_user requests drained from the runtime (polled each tick).
    pub ask_user_buffer: Vec<crate::providers::AskUserRequest>,

    // ── Flags ──
    pub should_quit: bool,
    pub dirty: bool,
    /// One-frame feedback after a copy attempt so the user knows whether it
    /// succeeded or whether nothing was selected.
    pub copy_feedback: crate::tui::app::CopyFeedback,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new("", None)
    }
}

impl AppState {
    /// The status-bar message for the last copy attempt, if it is still within
    /// its display window. Returns None once the feedback has expired (or no
    /// copy has been attempted yet).
    pub fn copy_feedback_message(&self) -> Option<String> {
        self.copy_feedback.message(self.frame_count)
    }

    /// Whether the draw loop should keep re-rendering so the copy feedback can
    /// disappear on its own once its window elapses. True during the feedback
    /// window and for one extra frame after it, so the final repaint restores
    /// the normal status line before rendering stops.
    pub fn copy_feedback_active(&self) -> bool {
        self.copy_feedback.needs_redraw(self.frame_count)
    }

    pub fn new(
        model: &str,
        // session_id: &str,
        // effort: &str,
        subagent_rx: Option<tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity>>,
    ) -> Self {
        Self {
            turns: Vec::new(),
            active_blocks: Vec::new(),
            is_streaming: false,
            subagent_rx,
            pending_subagent: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            input_area: None,
            messages_area: None,
            feedback_area: None,
            input_scroll: 0,
            input_scroll_cursor: 0,
            selection: None,
            scroll: ScrollState::new(),
            virtual_list: crate::tui::virtual_list::VirtualList::new(),
            messages_dirty: true,
            followup_rows: Vec::new(),
            hovered_followup: None,
            pending_prompt: None,
            model: model.to_string(),
            effective_model: None,
            // session_id: if session_id.len() > 8 {
            //     session_id[..8].to_string()
            // } else {
            //     session_id.to_string()
            // },
            // effort: effort.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            context_pct: 0.0,
            turn_count: 0,
            tool_count: 0,
            stream_start: None,
            permission_mode: PermissionMode::Auto,
            // shared_permission_mode: None,
            side_panel_open: false,
            side_panel_focused: false,
            side_panel_tab: SidePanelTab::GitDiff,
            side_panel_scroll: ScrollState::new(),
            side_panel_diff: String::new(),
            side_panel_tree: String::new(),
            overlay: Overlay::None,
            pending_permission_tx: None,
            command_selector: None,
            _provider_fetch_task: None,
            provider_fetch_rx: None,
            frame_count: 0,
            should_quit: false,
            dirty: true,
            pending_interview_target: None,
            pending_editor: None,
            ask_user_buffer: Vec::new(),
            copy_feedback: CopyFeedback::none(),
        }
    }

    /// Recompute the fuzzy command selector from the current input.
    /// Called after any input mutation while not streaming.
    pub fn refresh_command_selector(&mut self) {
        let input = &self.input;
        let at_end = self.cursor_pos == input.len();
        if self.is_streaming
            || self.pending_interview_target.is_some()
            || !input.starts_with('/')
            || !at_end
        {
            self.command_selector = None;
            return;
        }
        let query = input[1..].to_string();
        let matches = filter_commands(&query);
        if matches.is_empty() {
            self.command_selector = None;
            return;
        }
        let query_changed = self
            .command_selector
            .as_ref()
            .is_none_or(|s| s.query != query);
        let selected = if query_changed {
            0
        } else {
            self.command_selector
                .as_ref()
                .map(|s| s.selected.min(matches.len() - 1))
                .unwrap_or(0)
        };
        self.command_selector = Some(CommandSelectorState {
            query,
            matches,
            selected,
        });
    }

    /// The selected byte range of the input, if the active selection targets it.
    pub fn input_selection(&self) -> Option<(usize, usize)> {
        let sel = self.selection.as_ref()?;
        if sel.target != SelectionTarget::Input {
            return None;
        }
        let (SelectionPoint::Input(anchor), SelectionPoint::Input(active)) =
            (sel.anchor, sel.active)
        else {
            return None;
        };
        Some((anchor.min(active), anchor.max(active)))
    }

    /// The normalized output selection range, if the active selection targets it.
    pub fn output_selection(&self) -> Option<OutputSelectionRange> {
        let sel = self.selection.as_ref()?;
        if sel.target != SelectionTarget::Output {
            return None;
        }
        let (SelectionPoint::Output(ar, ac), SelectionPoint::Output(br, bc)) =
            (sel.anchor, sel.active)
        else {
            return None;
        };
        let range = match ar.cmp(&br) {
            std::cmp::Ordering::Less => OutputSelectionRange {
                start_row: ar as u16,
                start_col: ac,
                end_row: br as u16,
                end_col: bc,
            },
            std::cmp::Ordering::Greater => OutputSelectionRange {
                start_row: br as u16,
                start_col: bc,
                end_row: ar as u16,
                end_col: ac,
            },
            std::cmp::Ordering::Equal => OutputSelectionRange {
                start_row: ar as u16,
                start_col: ac.min(bc),
                end_row: ar as u16,
                end_col: ac.max(bc),
            },
        };
        Some(range)
    }

    /// The selected text, if any (for copy). Output rows are joined with `\n`.
    /// Returns None when nothing is selected or the selection is empty.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        if sel.dragging {
            return None;
        }
        match sel.target {
            SelectionTarget::Input => {
                let (start, end) = self.input_selection()?;
                // The input may have shifted since the selection was made;
                // snap to char boundaries so we never panic mid-character.
                let start = self.input.floor_char_boundary(start.min(self.input.len()));
                let end = self.input.floor_char_boundary(end.min(self.input.len()));
                if start == end {
                    return None;
                }
                Some(self.input[start..end].to_string())
            }
            SelectionTarget::Output => {
                let range = self.output_selection()?;
                let start_row = range.start_row as usize;
                // Selection columns are grapheme indices; slice by grapheme
                // so combining-mark clusters and multi-byte chars never split.
                if range.start_row == range.end_row {
                    let text = self.virtual_list.row_slice_by_graphemes(
                        start_row,
                        range.start_col,
                        range.end_col,
                    );
                    if text.is_empty() {
                        return None;
                    }
                    return Some(text);
                }
                let mut parts = vec![self.virtual_list.row_slice_by_graphemes(
                    start_row,
                    range.start_col,
                    usize::MAX,
                )];
                for row in (range.start_row + 1)..range.end_row {
                    parts.push(self.virtual_list.row_text(row as usize));
                }
                let end_row = range.end_row as usize;
                let end_text = self
                    .virtual_list
                    .row_slice_by_graphemes(end_row, 0, range.end_col);
                if !end_text.is_empty() {
                    parts.push(end_text);
                }
                Some(parts.join("\n"))
            }
        }
    }

    /// Whether a completed, non-empty selection exists and can be copied. Used
    /// to decide whether the feedback strip shows `selected: …` and whether a
    /// click on the strip should copy. Checks only the selection geometry, so
    /// it never materializes the selected text.
    pub fn has_copyable_selection(&self) -> bool {
        let Some(sel) = self.selection.as_ref() else {
            return false;
        };
        if sel.dragging {
            return false;
        }
        match sel.target {
            SelectionTarget::Input => self
                .input_selection()
                .is_some_and(|(start, end)| start != end),
            SelectionTarget::Output => self.output_selection().is_some_and(|range| {
                range.start_row != range.end_row || range.start_col != range.end_col
            }),
        }
    }

    /// A short, single-line preview of the current selection for the feedback
    /// strip (`selected: {text}`), flattened to one line and capped at
    /// `max_bytes` so huge selections stay cheap to render every frame.
    pub fn selection_preview(&self, max_bytes: usize) -> Option<String> {
        let sel = self.selection.as_ref()?;
        if sel.dragging {
            return None;
        }
        match sel.target {
            SelectionTarget::Input => {
                let (start, end) = self.input_selection()?;
                if start == end {
                    return None;
                }
                // The input may have shifted since the selection was made;
                // snap to char boundaries so we never panic mid-character.
                let start = self.input.floor_char_boundary(start.min(self.input.len()));
                let end = self.input.floor_char_boundary(end.min(self.input.len()));
                if start == end {
                    return None;
                }
                Some(preview_line(&self.input[start..end], max_bytes))
            }
            SelectionTarget::Output => {
                let range = self.output_selection()?;
                if range.start_row == range.end_row && range.start_col == range.end_col {
                    return None;
                }
                // Walk only the rows needed to fill the preview cap; rows are
                // single visual lines, so this stops after a handful of rows
                // even for a whole-document selection.
                let mut out = String::new();
                if range.start_row == range.end_row {
                    let text = self.virtual_list.row_slice_by_graphemes(
                        range.start_row as usize,
                        range.start_col,
                        range.end_col,
                    );
                    return if text.is_empty() {
                        None
                    } else {
                        Some(preview_line(&text, max_bytes))
                    };
                }
                append_preview(
                    &mut out,
                    &self.virtual_list.row_slice_by_graphemes(
                        range.start_row as usize,
                        range.start_col,
                        usize::MAX,
                    ),
                    max_bytes,
                );
                // Row boundaries are newlines in the copied text; feed them
                // through the same flattener so they become single spaces.
                for row in (range.start_row + 1)..range.end_row {
                    if out.len() >= max_bytes {
                        break;
                    }
                    append_preview(&mut out, "\n", max_bytes);
                    append_preview(
                        &mut out,
                        &self.virtual_list.row_text(row as usize),
                        max_bytes,
                    );
                }
                if out.len() < max_bytes {
                    append_preview(&mut out, "\n", max_bytes);
                    append_preview(
                        &mut out,
                        &self.virtual_list.row_slice_by_graphemes(
                            range.end_row as usize,
                            0,
                            range.end_col,
                        ),
                        max_bytes,
                    );
                }
                if out.is_empty() { None } else { Some(out) }
            }
        }
    }

    /// Append a text delta to the streaming output, coalescing consecutive
    /// deltas into one block (keeps the block list small and the text
    /// contiguous).
    pub fn append_text(&mut self, text: &str) {
        match self.active_blocks.last_mut() {
            Some(OutputBlock::Text(t)) => t.push_str(text),
            _ => self.active_blocks.push(OutputBlock::Text(text.to_string())),
        }
    }

    /// Append a thinking delta to the streaming output, coalescing
    /// consecutive deltas into one block.
    pub fn append_thinking(&mut self, text: &str) {
        match self.active_blocks.last_mut() {
            Some(OutputBlock::Thinking(t)) => t.push_str(text),
            _ => self
                .active_blocks
                .push(OutputBlock::Thinking(text.to_string())),
        }
    }

    /// The active (streaming) tool calls, in order.
    pub fn active_tools(&self) -> impl Iterator<Item = &ToolCall> {
        self.active_blocks.iter().filter_map(OutputBlock::as_tool)
    }

    /// Mutable access to the active (streaming) tool calls, in order.
    pub fn active_tools_mut(&mut self) -> impl Iterator<Item = &mut ToolCall> {
        self.active_blocks
            .iter_mut()
            .filter_map(OutputBlock::as_tool_mut)
    }

    /// Commit the current streaming blocks into a completed turn.
    pub fn commit_turn(&mut self) {
        if !self.active_blocks.is_empty() {
            self.turns.push(Turn {
                role: TurnRole::Assistant,
                blocks: std::mem::take(&mut self.active_blocks),
                content: String::new(),
                followups: Vec::new(),
            });
            self.turn_count += 1;
        }
        self.is_streaming = false;
        self.stream_start = None;
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Add a user message turn.
    pub fn push_user(&mut self, text: &str) {
        self.turns.push(Turn {
            role: TurnRole::User,
            content: text.to_string(),
            blocks: Vec::new(),
            followups: Vec::new(),
        });
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Add a system (status) turn.
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.turns.push(Turn {
            role: TurnRole::System,
            content: text.into(),
            blocks: Vec::new(),
            followups: Vec::new(),
        });
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Add a system turn carrying suggested next steps. Each followup is
    /// rendered as a clickable row; clicking one sends its prompt to the
    /// model (see `handle_mouse` in event_loop.rs).
    pub fn push_followups(&mut self, followups: Vec<crate::subagents::Followup>) {
        if followups.is_empty() {
            return;
        }
        self.turns.push(Turn {
            role: TurnRole::System,
            content: "Suggested next steps".into(),
            blocks: Vec::new(),
            followups,
        });
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Elapsed time since streaming started.
    pub fn elapsed_ms(&self) -> u64 {
        self.stream_start
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    // /// Cycle permission mode and update the shared atomic.
    // pub fn cycle_permission_mode(&mut self) {
    //     self.permission_mode = self.permission_mode.next();
    //     if let Some(ref shared) = self.shared_permission_mode {
    //         let val = match self.permission_mode {
    //             PermissionMode::Auto => 0,
    //             PermissionMode::Plan => 1,
    //             PermissionMode::Editor => 2,
    //             PermissionMode::Bypass => 3,
    //             PermissionMode::BypassAlert => 4,
    //         };
    //         shared.store(val, Ordering::Relaxed);
    //     }
    // }

    // /// Set the shared permission mode reference.
    // pub fn set_shared_mode(&mut self, mode: SharedPermissionMode) {
    //     self.shared_permission_mode = Some(mode);
    // }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_blocks_coalesce_and_commit_in_order() {
        let mut s = AppState::new("test-model", None);
        s.is_streaming = true;
        // The agent alternates: text, tool call, thinking, then more text.
        s.append_text("Hello ");
        s.append_text("world");
        s.active_blocks.push(OutputBlock::Tool(ToolCall {
            name: "Grep".into(),
            input_summary: "pattern".into(),
            status: ToolStatus::Running,
            output_preview: None,
            started_at: Instant::now(),
            duration_ms: None,
            children: Vec::new(),
            run_id: None,
        }));
        s.append_thinking("let me reason");
        s.append_text(" Done.");

        // Consecutive text (or thinking) deltas coalesce into one block;
        // tools and interleaved chunks keep their own blocks, in order.
        let kinds: Vec<&str> = s
            .active_blocks
            .iter()
            .map(|b| match b {
                OutputBlock::Text(_) => "text",
                OutputBlock::Thinking(_) => "thinking",
                OutputBlock::Tool(_) => "tool",
            })
            .collect();
        assert_eq!(kinds, vec!["text", "tool", "thinking", "text"]);
        match &s.active_blocks[0] {
            OutputBlock::Text(t) => assert_eq!(t, "Hello world"),
            _ => panic!("expected text block"),
        }

        // Committing moves the ordered blocks into the turn untouched.
        s.commit_turn();
        assert!(s.active_blocks.is_empty());
        let turn = s.turns.last().expect("committed");
        assert_eq!(turn.role, TurnRole::Assistant);
        let kinds: Vec<&str> = turn
            .blocks
            .iter()
            .map(|b| match b {
                OutputBlock::Text(_) => "text",
                OutputBlock::Thinking(_) => "thinking",
                OutputBlock::Tool(_) => "tool",
            })
            .collect();
        assert_eq!(kinds, vec!["text", "tool", "thinking", "text"]);
        // A turn with only thinking (no text/tools) still commits.
        let mut s = AppState::new("test-model", None);
        s.append_thinking("deep thought");
        s.commit_turn();
        assert_eq!(s.turns.len(), 1);
        assert!(matches!(s.turns[0].blocks[0], OutputBlock::Thinking(_)));
    }

    #[test]
    fn empty_query_lists_all_commands() {
        let matches = filter_commands("");
        assert_eq!(matches.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn fuzzy_prefix_matches() {
        let matches = filter_commands("mod");
        assert_eq!(matches[0].name, "model");
        assert!(matches.iter().all(|m| m.name.starts_with("mod")));
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        // "mdl" is a subsequence of "model" but not a prefix.
        let names: Vec<String> = filter_commands("mdl").into_iter().map(|m| m.name).collect();
        assert!(names.iter().any(|n| n == "model"), "{names:?}");
    }

    #[test]
    fn non_match_returns_empty() {
        assert!(filter_commands("zzz-nope").is_empty());
    }

    #[test]
    fn input_selection_normalizes_and_slices() {
        let mut s = AppState::new("test-model", None);
        s.input = "hello world".into();

        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(2),
            active: SelectionPoint::Input(7),
            dragging: false,
        });
        assert_eq!(s.input_selection(), Some((2, 7)));
        assert_eq!(s.selection_text().as_deref(), Some("llo w"));

        // Dragging the other way normalizes to the same range.
        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(7),
            active: SelectionPoint::Input(2),
            dragging: false,
        });
        assert_eq!(s.input_selection(), Some((2, 7)));
        assert_eq!(s.selection_text().as_deref(), Some("llo w"));

        // A zero-width selection copies nothing.
        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(3),
            active: SelectionPoint::Input(3),
            dragging: false,
        });
        assert_eq!(s.selection_text(), None);

        // Input selections never report an output range and vice versa.
        assert_eq!(s.output_selection(), None);
    }

    #[test]
    fn output_selection_joins_rows_with_newlines() {
        let mut s = AppState::new("test-model", None);
        s.virtual_list.set_committed(vec![
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("row one")),
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("row two")),
        ]);

        // Anchor at (1, 3), active at (0, 2) — normalizes to (0, 2)..(1, 3).
        s.selection = Some(Selection {
            target: SelectionTarget::Output,
            anchor: SelectionPoint::Output(1, 3),
            active: SelectionPoint::Output(0, 2),
            dragging: false,
        });
        let range = s.output_selection().expect("range");
        assert_eq!(
            (
                range.start_row,
                range.start_col,
                range.end_row,
                range.end_col
            ),
            (0, 2, 1, 3)
        );
        assert_eq!(s.selection_text().as_deref(), Some("w one\nrow"));

        // Same row: a simple slice.
        s.selection = Some(Selection {
            target: SelectionTarget::Output,
            anchor: SelectionPoint::Output(1, 1),
            active: SelectionPoint::Output(1, 6),
            dragging: false,
        });
        assert_eq!(s.selection_text().as_deref(), Some("ow tw"));
    }

    #[test]
    fn copyable_selection_reflects_geometry() {
        let mut s = AppState::new("test-model", None);
        s.input = "abc def".into();

        // A normal input selection is copyable.
        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(0),
            active: SelectionPoint::Input(3),
            dragging: false,
        });
        assert!(s.has_copyable_selection());

        // Still dragging (button held): nothing is copyable yet.
        s.selection.as_mut().unwrap().dragging = true;
        assert!(!s.has_copyable_selection());
        s.selection.as_mut().unwrap().dragging = false;

        // Zero-width selection copies nothing.
        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(3),
            active: SelectionPoint::Input(3),
            dragging: false,
        });
        assert!(!s.has_copyable_selection());
        assert_eq!(s.selection_preview(200), None);

        s.virtual_list.set_committed(vec![
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("row one")),
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("row two")),
        ]);
        s.selection = Some(Selection {
            target: SelectionTarget::Output,
            anchor: SelectionPoint::Output(1, 5),
            active: SelectionPoint::Output(0, 1),
            dragging: false,
        });
        assert!(s.has_copyable_selection());
    }

    #[test]
    fn selection_preview_flattens_and_caps() {
        let mut s = AppState::new("test-model", None);

        // Input containing newlines previews as a single space-joined line.
        s.input = "line one\n  line two  ".into();
        s.selection = Some(Selection {
            target: SelectionTarget::Input,
            anchor: SelectionPoint::Input(0),
            active: SelectionPoint::Input(s.input.len()),
            dragging: false,
        });
        assert_eq!(
            s.selection_preview(200).as_deref(),
            Some("line one line two")
        );

        // Multi-row output selection flattens to one line too.
        s.virtual_list.set_committed(vec![
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("fn main() {")),
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("    let x = 1;")),
            crate::tui::virtual_list::VItem::new(ratatui::prelude::Line::from("}")),
        ]);
        s.selection = Some(Selection {
            target: SelectionTarget::Output,
            anchor: SelectionPoint::Output(0, 0),
            active: SelectionPoint::Output(2, 1),
            dragging: false,
        });
        assert_eq!(
            s.selection_preview(200).as_deref(),
            Some("fn main() { let x = 1; }")
        );

        // The cap bounds the preview length (in bytes).
        let preview = s.selection_preview(12).expect("preview");
        assert!(preview.len() <= 16, "preview too long: {preview:?}");

        // Plain-text helper keeps leading whitespace off and collapses runs.
        assert_eq!(preview_line("  a\n\n\tb  ", 100), "a b");
        assert_eq!(preview_line("hello world", 5), "hello");
    }

    #[test]
    fn combo_picker_filter_narrows_by_subsequence() {
        let combo = |name: &str| crate::providers::Combo {
            name: name.into(),
            entries: Vec::new(),
        };
        let p = ComboPickerState {
            combos: vec![combo("coding"), combo("writing"), combo("docs")],
            query: "cod".into(),
            selected: 0,
            current: None,
        };
        let names: Vec<&str> = p.filtered().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["coding"]);

        // Empty query matches everything.
        let all = ComboPickerState {
            query: String::new(),
            ..p.clone()
        };
        assert_eq!(all.filtered().len(), 3);

        // No match returns nothing.
        let none = ComboPickerState {
            query: "zzz".into(),
            ..p
        };
        assert!(none.filtered().is_empty());
    }

    /// A Models-phase explorer for a provider.
    fn model_explorer(provider_name: &str, all_models: Vec<&str>) -> ProviderExplorerState {
        let provider = crate::providers::Provider {
            name: provider_name.into(),
            base_url: "http://x".into(),
            api_key: "k".into(),
            models: Vec::new(),
        };
        ProviderExplorerState {
            phase: ProviderExplorerPhase::Models,
            query: String::new(),
            selected: 0,
            providers: vec![provider.clone()],
            provider: Some(provider),
            current_provider: None,
            all_models: all_models.into_iter().map(String::from).collect(),
            loading: false,
            error: String::new(),
        }
    }

    #[test]
    fn provider_explorer_filter_narrows_by_subsequence() {
        let p = model_explorer(
            "groq",
            vec![
                "llama-3.3-70b",
                "llama-3.1-8b-instant",
                "compound",
                "gpt-oss-20b",
            ],
        );
        let p = ProviderExplorerState {
            query: "ll".into(),
            ..p
        };
        // Both llama models match the "ll" subsequence; compound and gpt-oss don't.
        let names: Vec<&str> = p.filtered_models();
        assert_eq!(names, vec!["llama-3.1-8b-instant", "llama-3.3-70b"]);
    }

    #[test]
    fn provider_explorer_empty_query_returns_all() {
        let p = model_explorer("groq", vec!["b", "a", "c"]);
        // Empty query matches everything, sorted by score then name.
        assert_eq!(p.filtered_models(), vec!["a", "b", "c"]);
    }

    #[test]
    fn provider_explorer_no_match_returns_empty() {
        let p = model_explorer("groq", vec!["llama"]);
        let p = ProviderExplorerState {
            query: "xyz".into(),
            ..p
        };
        assert!(p.filtered_models().is_empty());
    }

    #[test]
    fn provider_explorer_filters_providers_by_name() {
        let provider = |name: &str| crate::providers::Provider {
            name: name.into(),
            base_url: String::new(),
            api_key: String::new(),
            models: Vec::new(),
        };
        let p = ProviderExplorerState {
            phase: ProviderExplorerPhase::Providers,
            query: "gro".into(),
            selected: 0,
            providers: vec![
                provider("groq"),
                provider("poolside"),
                provider("openrouter"),
            ],
            provider: None,
            current_provider: Some("poolside".into()),
            all_models: Vec::new(),
            loading: false,
            error: String::new(),
        };
        let names: Vec<&str> = p
            .filtered_providers()
            .iter()
            .map(|pp| pp.name.as_str())
            .collect();
        assert_eq!(names, vec!["groq"]);
    }
}
