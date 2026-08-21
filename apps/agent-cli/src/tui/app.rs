//! TUI application state.

// use crate::permissions::SharedPermissionMode;
use crate::tui::scroll::ScrollState;
use cersei::tools::permissions::PermissionDecision;
use std::time::Instant;
use tokio::sync::oneshot;

/// A single message turn in the conversation.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub content: String,
    pub tools: Vec<ToolCall>,
    pub thinking: Option<String>,
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

/// Provider/model picker shown by `/model`.
#[derive(Debug, Clone)]
pub struct ModelPickerState {
    /// (provider, model) entries in display order.
    pub entries: Vec<(String, String)>,
    pub selected: usize,
    /// Joined "provider/model" id of the current selection (for the marker).
    pub current: String,
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
    if qi == q.len() {
        Some(score)
    } else {
        None
    }
}

/// Commands matching `query`, best matches first.
pub fn filter_commands(query: &str) -> Vec<CommandMatch> {
    let mut scored: Vec<(u32, CommandMatch)> = SLASH_COMMANDS
        .iter()
        .filter_map(|(name, description)| {
            fuzzy_score(query, name).map(|score| {
                (score, CommandMatch {
                    name: name.to_string(),
                    description,
                })
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
    Graph(crate::tui::widgets::graph::GraphOverlayState),
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
                | (Self::Graph(_), Self::Graph(_))
        )
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

/// Full application state for the TUI.
pub struct AppState {
    // ── Conversation ──
    pub turns: Vec<Turn>,
    pub streaming_text: String,
    pub streaming_thinking: String,
    pub is_streaming: bool,
    pub active_tools: Vec<ToolCall>,
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
    /// Vertical scroll of the input content from the last frame — for mouse hit-testing.
    pub input_scroll: u16,

    // ── Scroll + Virtual List ──
    pub scroll: ScrollState,
    pub virtual_list: crate::tui::virtual_list::VirtualList,
    pub messages_dirty: bool,

    // ── Status ──
    pub model: String,
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

    // ── Animation ──
    pub frame_count: u64,

    // ── Flags ──
    pub should_quit: bool,
    pub dirty: bool,
}

impl AppState {
    pub fn new(
        model: &str,
        // session_id: &str,
        // effort: &str,
        subagent_rx: Option<tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity>>,
    ) -> Self {
        Self {
            turns: Vec::new(),
            streaming_text: String::new(),
            streaming_thinking: String::new(),
            is_streaming: false,
            active_tools: Vec::new(),
            subagent_rx,
            pending_subagent: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            input_area: None,
            input_scroll: 0,
            scroll: ScrollState::new(),
            virtual_list: crate::tui::virtual_list::VirtualList::new(),
            messages_dirty: true,
            model: model.to_string(),
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
            frame_count: 0,
            should_quit: false,
            dirty: true,
        }
    }

    /// Recompute the fuzzy command selector from the current input.
    /// Called after any input mutation while not streaming.
    pub fn refresh_command_selector(&mut self) {
        let input = &self.input;
        let at_end = self.cursor_pos == input.len();
        if self.is_streaming || !input.starts_with('/') || !at_end {
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

    /// Commit the current streaming text into a completed turn.
    pub fn commit_turn(&mut self) {
        if !self.streaming_text.is_empty() || !self.active_tools.is_empty() {
            self.turns.push(Turn {
                role: TurnRole::Assistant,
                content: std::mem::take(&mut self.streaming_text),
                tools: std::mem::take(&mut self.active_tools),
                thinking: if self.streaming_thinking.is_empty() {
                    None
                } else {
                    Some(std::mem::take(&mut self.streaming_thinking))
                },
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
            tools: Vec::new(),
            thinking: None,
        });
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Add a system (status) turn.
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.turns.push(Turn {
            role: TurnRole::System,
            content: text.into(),
            tools: Vec::new(),
            thinking: None,
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
}
