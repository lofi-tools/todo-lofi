//! The agent pane: a right-hand pane running one ACP session per
//! directory-backed project.
//!
//! Sessions are keyed by project tag name and outlive pane switching, so a
//! streaming turn keeps running while another project is on screen (its
//! navbar row shows a busy dot). Transcript rows come straight from
//! [`acp_client::Transcript`]; this module owns the gpui surface around them:
//! header, rows, prompt box, controls and per-project states.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use acp_client::schema::{
    PermissionOptionId, SessionConfigId, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectOptions, SessionId,
    SessionModeId, SessionModeState, ToolCallUpdate,
};
use acp_client::{
    AcpConnection, AcpError, AcpEvent, Activity, AgentServer, AuthMethodRow, ConnectOptions, EntryKind,
    NoticeLevel, OpenCodeAgent, PermissionChoice, PermissionDecision, PermissionRecord,
    PermissionReply, PermissionRule, SessionRoots, SessionSpec, SessionStore, SpawnSpec,
    StoredSession, ToolPermissions, ToolStatus, Transcript, connect,
};
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, actions, div, px,
    rgb, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_component::text::TextView;
use gpui_component::{Disableable, IconName, Sizable, StyledExt as _};

use crate::theme::{
    APP_BG, CARD_BG, DANGER, DIFF_ADD_BG, DIFF_DEL_BG, HAIRLINE, PANEL_BG, PANEL_HOVER, SUCCESS,
    TEXT_FAINT, TEXT_MUTED, TEXT_STRONG,
};
use crate::ui_parts::notifications::{self, NoticeLevel as Severity};

const AGENT_PANE_CONTEXT: &str = "AgentPane";
/// How long after an outside-mousedown close a selector-button click is
/// swallowed instead of reopening the dropdown.
const OVERLAY_OUTSIDE_CLOSE_IGNORE_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(300);
/// Rows shown in a dropdown; the rest is reachable by typing. The card never
/// scrolls: a scrollable wrapper would take the card out of the floating
/// context (its styles don't transfer to the wrapper), so the list is
/// bounded by construction instead.
const OVERLAY_MAX_ROWS: usize = 12;
/// Queued prompts are in-memory and session-scoped; beyond this, Send waits.
const QUEUE_CAP: usize = 10;
/// Lines of terminal output shown before the row needs expanding.
const TERMINAL_TAIL_LINES: usize = 200;
/// Upper bound on retained terminal output per terminal.
const TERMINAL_MAX_LINES: usize = 2_000;
/// Upper bound on tool output lines rendered when a tool call is expanded.
/// Bounding the input keeps the first synchronous shape+layout of the block
/// cheap; the line-layout cache makes repeat frames fast.
const TOOL_MAX_LINES: usize = 1_000;
/// Lines of captured stderr shown inside an error card.
const ERROR_TAIL_LINES: usize = 20;
const MONO_FONT: &str = "ui-monospace";

actions!(agent_pane, [SlashUp, SlashDown, DismissOverlay]);

/// Register the pane's key bindings. Called once at startup, before the first
/// pane is created; the bindings only match while the pane has focus.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SlashUp, Some(AGENT_PANE_CONTEXT)),
        KeyBinding::new("down", SlashDown, Some(AGENT_PANE_CONTEXT)),
        KeyBinding::new("escape", DismissOverlay, Some(AGENT_PANE_CONTEXT)),
    ]);
}

/// The checkout an active coding run works in, as the pane points at it: the
/// run's worktree instead of the user's checkout (decision 11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCheckout {
    /// The project tag the run's directories belong to; the checkout only ever
    /// applies to the pane entry for this tag.
    pub tag_id: u64,
    /// The worktree for the run's main repo directory; the session's primary
    /// root while the run is active.
    pub worktree: PathBuf,
    /// The repo directory the worktree was cut from. It is dropped from the
    /// session's secondary roots, so the agent cannot edit the user's own
    /// checkout and leave the branch missing half the work.
    pub repo_dir: PathBuf,
    /// The build directory every worktree of the run shares (decision 12), or
    /// `None` when the project opted out of the shared cache (spec §6.4) and
    /// each worktree builds into its own `target/`.
    pub target_dir: Option<PathBuf>,
}

/// The build directory to point a worktree session at, if any: the run's
/// shared cache normally, nothing when the project isolated its builds.
fn checkout_target_dir(checkout: Option<&RunCheckout>) -> Option<PathBuf> {
    checkout?.target_dir.clone()
}

/// A directory-backed project the pane can run an agent for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProject {
    pub tag_id: u64,
    /// The tag's unique name; the key a session is stored under.
    pub tag_name: String,
    /// Display label, used until the agent reports a session title.
    pub label: String,
    /// Candidate directories in resolution order: the path encoded in the tag
    /// name first, then `tag_settings.dirs`.
    pub candidates: Vec<PathBuf>,
    /// The active run's worktree, when the selected task has one. `None`
    /// keeps the session on the project directory.
    pub checkout: Option<RunCheckout>,
}

impl AgentProject {
    /// Split the candidates into the session `cwd` and the additional roots,
    /// dropping candidates that do not exist. `None` when none exist.
    ///
    /// A checkout makes the worktree the `cwd` and the repo it replaces a
    /// non-root (decisions 11 and 21).
    pub fn resolve(&self) -> Option<(PathBuf, Vec<PathBuf>)> {
        let candidates: Vec<PathBuf> = match self
            .checkout
            .as_ref()
            .filter(|checkout| checkout.worktree.is_dir())
        {
            Some(checkout) => std::iter::once(checkout.worktree.clone())
                .chain(
                    self.candidates
                        .iter()
                        .filter(|dir| **dir != checkout.repo_dir && **dir != checkout.worktree)
                        .cloned(),
                )
                .collect(),
            None => self.candidates.clone(),
        };
        // Through the same resolution as any other directory: the candidates
        // are canonicalized, so one directory reached by two spellings still
        // keys one session.
        SessionSpec::new(candidates).resolve()
    }
}

/// An agent launched with an extra environment variable. Used to give a
/// worktree session the run's shared build cache (decision 12) without
/// teaching the agent registry about individual projects.
struct EnvAgent {
    inner: Arc<dyn AgentServer>,
    name: &'static str,
    value: String,
}

impl AgentServer for EnvAgent {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn display_name(&self) -> &'static str {
        self.inner.display_name()
    }

    fn program(&self) -> &'static str {
        self.inner.program()
    }

    fn args(&self) -> &'static [&'static str] {
        self.inner.args()
    }

    fn spawn_spec(&self, cwd: &std::path::Path) -> Result<SpawnSpec, AcpError> {
        let mut spec = self.inner.spawn_spec(cwd)?;
        spec.env.insert(self.name.to_string(), self.value.clone());
        Ok(spec)
    }
}

/// What the pane reports to the layout about agent state.
#[derive(Clone, Debug)]
pub enum AgentPaneEvent {
    /// A project's turn started or ended, so its navbar row can show the
    /// busy dot.
    BusyChanged { tag_name: String, busy: bool },
    /// The prompt box asked for the selected task's context to be inserted.
    AttachTaskRequested,
}

/// Build the prompt text for the currently selected task.
///
/// Single seam for "what the agent is told about the task": a later revision
/// can widen this to several tasks without touching the caller.
pub fn build_task_context(task: &storage::TaskWithMeta) -> String {
    task_context_text(&task.title, task.description.as_deref(), &task.direct_tags)
}

fn task_context_text(title: &str, description: Option<&str>, tags: &[String]) -> String {
    let mut context = format!("Task: {}", title.trim());
    if let Some(description) = description
        .map(str::trim)
        .filter(|description| !description.is_empty())
    {
        context.push_str("\n\n");
        context.push_str(description);
    }
    if !tags.is_empty() {
        context.push_str("\n\nTags: ");
        context.push_str(&tags.join(", "));
    }
    context
}

/// Everything the pane knows about one project.
struct ProjectEntry {
    project: AgentProject,
    state: PaneState,
    transcript: Transcript,
    busy: bool,
    queue: VecDeque<String>,
    /// The project's tool approval policy, in Zed's `tool_permissions` shape.
    tool_permissions: ToolPermissions,
    /// Answers waiting for a user decision, keyed by transcript entry id.
    permission_replies: HashMap<u64, PermissionReply>,
    /// Tool rule key per pending permission request, keyed the same way.
    permission_tools: HashMap<u64, String>,
    /// Live terminal output per terminal id.
    terminals: HashMap<String, TerminalRow>,
    /// Rows the user expanded to their full content.
    expanded: HashSet<u64>,
    /// Canonical project path; the session store key.
    stored_path: Option<String>,
    /// Last mode already reported as a notice.
    mode_seen: Option<String>,
    /// Bumped on every (re)start so a stale launch result is discarded.
    generation: u64,
    /// Launch work, held so it is not cancelled by being dropped.
    _launch_task: Option<Task<()>>,
    /// One-off work (sign-in, config writes), held for the same reason.
    _misc_task: Option<Task<()>>,
}

impl ProjectEntry {
    fn new(project: AgentProject, tool_permissions: ToolPermissions) -> Self {
        Self {
            project,
            state: PaneState::NoDirectory {
                candidates: Vec::new(),
            },
            transcript: Transcript::default(),
            busy: false,
            queue: VecDeque::new(),
            tool_permissions,
            permission_replies: HashMap::new(),
            permission_tools: HashMap::new(),
            terminals: HashMap::new(),
            expanded: HashSet::new(),
            stored_path: None,
            mode_seen: None,
            generation: 0,
            _launch_task: None,
            _misc_task: None,
        }
    }

    fn live(&self) -> Option<&LiveSession> {
        match &self.state {
            PaneState::Ready(live) | PaneState::AuthRequired(live) => Some(live),
            _ => None,
        }
    }

    fn live_mut(&mut self) -> Option<&mut LiveSession> {
        match &mut self.state {
            PaneState::Ready(live) | PaneState::AuthRequired(live) => Some(live),
            _ => None,
        }
    }

    fn has_session(&self) -> bool {
        self.live()
            .map(|live| live.session_id.is_some())
            .unwrap_or(false)
    }

    /// A mode change the transcript has not reported yet, as
    /// `(previous, current)`.
    fn mode_notice(&mut self) -> Option<(String, String)> {
        let controls = self.transcript.controls();
        let mode = controls.mode.as_ref()?;
        if mode.available_modes.is_empty() {
            return None;
        }
        let current = mode.current_mode_id.0.to_string();
        let previous = self.mode_seen.clone();
        if previous.as_deref() == Some(current.as_str()) {
            return None;
        }
        self.mode_seen = Some(current.clone());
        previous.map(|previous| (previous, current))
    }
}

enum PaneState {
    /// No candidate directory exists on disk: nothing is spawned.
    NoDirectory { candidates: Vec<PathBuf> },
    /// The process is starting and the session is being created.
    Launching { command: String, cwd: PathBuf },
    /// A live session, ready for prompts.
    Ready(Box<LiveSession>),
    /// The agent wants the user to authenticate before a session can start.
    AuthRequired(Box<LiveSession>),
    /// Launch or session failure; Retry starts over.
    Failed { title: String, detail: String },
}

struct LiveSession {
    connection: AcpConnection,
    /// `None` while authenticating, before a session exists.
    session_id: Option<SessionId>,
    /// Forwards the connection's event stream into the pane.
    _events_task: Task<()>,
    /// The turn currently streaming, held so it is not cancelled.
    _turn_task: Option<Task<()>>,
}

#[derive(Default)]
struct TerminalRow {
    output: String,
    truncated: bool,
    exit_code: Option<Option<u32>>,
}

/// A select-valued session config option, flattened for the chip UI.
#[derive(Clone, Debug)]
struct SelectChip {
    id: String,
    name: String,
    is_model: bool,
    /// A `mode`-category option (opencode reports session mode this way
    /// instead of session `modes`): rendered as the dedicated mode button.
    is_mode: bool,
    current: String,
    current_label: String,
    values: Vec<(String, String)>,
}

/// Which dropdown is open above the prompt box.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Overlay {
    /// The model-shaped config option.
    Model,
    /// A session mode.
    Mode,
    /// Another select-valued config option, by id.
    Config(String),
}

/// Title, visible `(value, label)` rows and current value for an open dropdown.
type OverlayRows = (String, Vec<(String, String)>, String);

pub struct AgentPane {
    session_store: SessionStore,
    agent: Arc<dyn AgentServer>,
    /// One entry per project the pane has touched, keyed by tag name.
    projects: HashMap<String, ProjectEntry>,
    /// Tag name of the project on screen; `None` when the selection is not
    /// directory-backed.
    active: Option<String>,
    /// Row count the scroller was last told about.
    row_count: usize,
    scroller: Entity<MessageScrollerState>,
    prompt: Entity<TextareaState>,
    focus_handle: FocusHandle,
    _prompt_events: Subscription,
    /// Highlighted row of the slash-command dropdown.
    slash_index: usize,
    overlay: Option<Overlay>,
    /// Which dropdown was last closed by an outside mousedown, and when. The
    /// click that follows that mousedown (e.g. re-clicking the selector
    /// button) must be swallowed instead of reopening it; clicking a
    /// *different* selector still switches to it.
    overlay_outside_closed: Option<(Overlay, std::time::Instant)>,
    /// Fuzzy-filter input for the model dropdown.
    overlay_query: Entity<InputState>,
    _overlay_query_events: Subscription,
    /// Highlighted row of the open dropdown.
    overlay_cursor: usize,
}

impl AgentPane {
    pub fn new(
        session_store: SessionStore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_placeholder("Ask the agent to do something…", window, cx);
            state
        });
        let _prompt_events = cx.subscribe_in(
            &prompt,
            window,
            |this: &mut AgentPane, _, event, window, cx| {
                this.on_input_event(event, window, cx);
            },
        );
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let overlay_query = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Filter models…", window, cx);
            state
        });
        let _overlay_query_events = cx.subscribe_in(
            &overlay_query,
            window,
            |this: &mut AgentPane, _, event, window, cx| {
                this.on_overlay_query_event(event, window, cx);
            },
        );
        Self {
            session_store,
            agent: Arc::new(OpenCodeAgent),
            projects: HashMap::new(),
            active: None,
            row_count: 0,
            scroller,
            prompt,
            focus_handle: cx.focus_handle(),
            _prompt_events,
            slash_index: 0,
            overlay: None,
            overlay_outside_closed: None,
            overlay_query,
            _overlay_query_events,
            overlay_cursor: 0,
        }
    }

    /// The project currently on screen, if any.
    pub fn active_project(&self) -> Option<&AgentProject> {
        self.active_entry().map(|entry| &entry.project)
    }

    /// Whether the on-screen project has a turn running.
    pub fn is_busy(&self) -> bool {
        self.active_entry().map(|entry| entry.busy).unwrap_or(false)
    }

    /// Stop the on-screen project's streaming turn, as the pane's own Stop
    /// control does. The run's Workflow row delegates here (decision #27).
    pub fn stop_turn(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
    }

    /// Number of prompts queued for the on-screen project.
    pub fn queue_len(&self) -> usize {
        self.active_entry()
            .map(|entry| entry.queue.len())
            .unwrap_or(0)
    }

    /// Put keyboard focus in the prompt box.
    pub fn focus_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Insert the selected task's context into the prompt box without sending
    /// it, so the message can be edited first.
    pub fn insert_prompt_text(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.prompt.read(cx).value().to_string();
        let combined = if existing.trim().is_empty() {
            text
        } else {
            format!("{}\n\n{}", existing.trim_end(), text)
        };
        self.prompt
            .update(cx, |state, cx| state.set_value(combined, window, cx));
        self.slash_index = 0;
        self.overlay = None;
        cx.notify();
    }

    /// Close an open dropdown. Returns whether anything was open, so the
    /// window-wide Escape handler can leave the rest to the layout.
    pub fn dismiss_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        if self.overlay.is_some() || self.slash_open(cx) {
            self.overlay = None;
            cx.notify();
            return true;
        }
        false
    }

    /// Point the pane at the selected project, resolving (or re-resolving)
    /// its directories. `None` covers every selection that is not
    /// directory-backed.
    pub fn set_project(&mut self, project: Option<AgentProject>, cx: &mut Context<Self>) {
        let Some(project) = project else {
            if self.active.take().is_some() {
                self.overlay = None;
                self.sync_scroller(cx);
                cx.notify();
            }
            return;
        };
        let tag_name = project.tag_name.clone();
        let resumable = self.projects.contains_key(&tag_name);
        self.refresh_entry(project, resumable, cx);
        let switched = self.active.as_deref() != Some(tag_name.as_str());
        self.active = Some(tag_name);
        if switched {
            self.overlay = None;
            self.slash_index = 0;
            self.sync_scroller(cx);
            cx.notify();
        }
    }

    /// Refresh a project's entry from a newly resolved project description.
    fn refresh_entry(
        &mut self,
        project: AgentProject,
        resumable: bool,
        cx: &mut Context<Self>,
    ) {
        let tag_name = project.tag_name.clone();
        let resolution = project.resolve();
        let tool_permissions = self
            .projects
            .get(&tag_name)
            .map(|entry| entry.tool_permissions.clone())
            .unwrap_or_else(|| self.stored_tool_permissions(&project));

        let Some(existing) = self.projects.get(&tag_name) else {
            let mut entry = ProjectEntry::new(project, tool_permissions);
            entry.state = match &resolution {
                Some((cwd, _)) => {
                    let command = self.agent.command_line(cwd);
                    PaneState::Launching {
                        command,
                        cwd: cwd.clone(),
                    }
                }
                None => PaneState::NoDirectory {
                    candidates: entry.project.candidates.clone(),
                },
            };
            self.projects.insert(tag_name.clone(), entry);
            if resolution.is_some() {
                self.start_session(&tag_name, resumable, cx);
            }
            return;
        };

        // A session whose cwd still exists stays up: its roots are fixed at
        // `session/new` time, so a changed candidate list only affects the
        // next session created for this project.
        let vanished = match (&resolution, existing.stored_path.as_deref()) {
            (None, _) => true,
            (Some((cwd, _)), Some(stored)) => cwd.display().to_string() != stored,
            _ => false,
        };
        if !vanished {
            if let Some(entry) = self.projects.get_mut(&tag_name) {
                entry.project = project;
            }
            if matches!(
                self.projects.get(&tag_name).map(|entry| &entry.state),
                Some(PaneState::Failed { .. })
            ) {
                if let Some((cwd, _)) = resolution {
                    self.restart_in_place(&tag_name, resumable, cwd, cx);
                }
            }
            return;
        }

        // A different cwd that still resolves is a checkpoint switch — the
        // run's worktree becoming available, or its removal restoring the
        // project directory. Restart there: the session key is the cwd, so
        // this is a new agent session, and the interview session stays
        // reachable as history once the run ends (§6.3).
        if let Some((cwd, _)) = resolution {
            if let Some(entry) = self.projects.get_mut(&tag_name) {
                entry.project = project;
            }
            self.restart_in_place(&tag_name, false, cwd, cx);
            return;
        }

        // No directory resolves any more: tear the project down rather than
        // leaving an agent editing a directory that is gone.
        let candidates = existing.project.candidates.clone();
        self.teardown(&tag_name, cx);
        let mut entry = ProjectEntry::new(project, tool_permissions);
        entry.state = PaneState::NoDirectory { candidates };
        self.projects.insert(tag_name, entry);
    }

    /// Point the pane at the selected task's active run (decision 11), or back
    /// at the project directory (`None`) when there is none. Only the
    /// checkout's own project is repointed, so selecting a task in another
    /// project cannot move the pane away from the project on screen.
    pub fn set_checkout(&mut self, checkout: Option<RunCheckout>, cx: &mut Context<Self>) {
        // Only the project on screen is repointed: a task selected in another
        // project must not restart that project's session in the background.
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let Some(project) = self.projects.get(&tag_name).map(|entry| entry.project.clone()) else {
            return;
        };
        // A checkout only ever applies to its own project; anything else means
        // "this project's run is not the one selected" and puts the session
        // back on the project directory.
        let checkout = checkout.filter(|checkout| checkout.tag_id == project.tag_id);
        if project.checkout == checkout {
            return;
        }
        let mut project = project;
        project.checkout = checkout;
        let resumable = self.projects.contains_key(&tag_name);
        self.refresh_entry(project, resumable, cx);
    }

    fn restart_in_place(
        &mut self,
        tag_name: &str,
        resumable: bool,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let command = self.agent.command_line(&cwd);
        if let Some(entry) = self.projects.get_mut(tag_name) {
            entry.state = PaneState::Launching { command, cwd };
        }
        self.start_session(tag_name, resumable, cx);
    }

    /// Read the project's stored tool approval policy, if any.
    fn stored_tool_permissions(&self, project: &AgentProject) -> ToolPermissions {
        project
            .resolve()
            .and_then(|(cwd, _)| self.session_store.get(&cwd.display().to_string()))
            .map(|stored| stored.tool_permissions)
            .unwrap_or_default()
    }

    /// Start (or restart) the agent process and session for a project.
    fn start_session(&mut self, tag_name: &str, resume: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.projects.get_mut(tag_name) else {
            return;
        };
        entry.transcript.clear();
        entry.permission_replies.clear();
        entry.permission_tools.clear();
        entry.terminals.clear();
        entry.expanded.clear();
        entry.queue.clear();
        entry.mode_seen = None;
        let Some((cwd, additional)) = entry.project.resolve() else {
            let candidates = entry.project.candidates.clone();
            entry.state = PaneState::NoDirectory { candidates };
            return;
        };
        let command = self.agent.command_line(&cwd);
        entry.state = PaneState::Launching {
            command,
            cwd: cwd.clone(),
        };
        entry.stored_path = Some(cwd.display().to_string());
        entry.generation += 1;
        let generation = entry.generation;
        let spec = SessionSpec::new(entry.project.candidates.clone());
        let project_path = entry.stored_path.clone().unwrap_or_default();
        let tag_id = entry.project.tag_id;
        let tool_permissions = entry.tool_permissions.clone();
        // A worktree session builds into the run's shared cache rather than
        // a per-worktree `target/` (decision 12): a second worktree would
        // otherwise be a full second build. A project with the isolated build
        // cache leaves the variable off, so cargo uses the worktree's own.
        let agent: Arc<dyn AgentServer> = match checkout_target_dir(entry.project.checkout.as_ref())
        {
            Some(target_dir) => Arc::new(EnvAgent {
                inner: self.agent.clone(),
                name: "CARGO_TARGET_DIR",
                value: target_dir.display().to_string(),
            }),
            None => self.agent.clone(),
        };
        let agent_id = agent.id().to_string();
        let store = self.session_store.clone();

        let launch = gpui_tokio::Tokio::spawn_result(cx, async move {
            let roots =
                SessionRoots::new(std::iter::once(cwd.clone()).chain(additional.iter().cloned()));
            let connection = connect(agent, &spec, |_| ConnectOptions {
                roots,
                tool_permissions: tool_permissions.clone(),
            })
            .await?;
            let stored = if resume {
                store.get(&project_path)
            } else {
                None
            };
            let created = match stored {
                Some(previous) if connection.info.load_session => {
                    let session_id = SessionId::new(previous.session_id.clone());
                    match connection
                        .requester
                        .load_session(session_id.clone(), cwd.clone(), additional.clone())
                        .await
                    {
                        Ok((modes, config_options)) => {
                            Ok((session_id, modes, config_options, None))
                        }
                        Err(error) => {
                            tracing::warn!("could not resume ACP session: {error}");
                            let notice =
                                format!("Could not resume session {}: {error}", previous.session_id);
                            let (session_id, modes, config_options) =
                                create_session(&connection, &cwd, &additional).await?;
                            Ok((session_id, modes, config_options, Some(notice)))
                        }
                    }
                }
                _ => {
                    let (session_id, modes, config_options) =
                        create_session(&connection, &cwd, &additional).await?;
                    Ok((session_id, modes, config_options, None))
                }
            };
            let created: anyhow::Result<SessionOpened> = created;
            let (session_id, modes, config_options, notice) = match created {
                Ok(created) => created,
                Err(error) => {
                    let methods: Vec<AuthMethodRow> = connection
                        .info
                        .auth_methods
                        .iter()
                        .map(AuthMethodRow::from_method)
                        .collect();
                    if methods.is_empty() {
                        return Err(error);
                    }
                    return Ok(LaunchOutcome::Auth {
                        connection,
                        methods,
                        detail: error.to_string(),
                    });
                }
            };
            let record = StoredSession {
                project_path: project_path.clone(),
                tag_id,
                agent_id,
                session_id: session_id.0.to_string(),
                updated_at: acp_client::session_store::now_secs(),
                tool_permissions,
            };
            if let Err(error) = store.put(record) {
                tracing::warn!("could not persist the ACP session id: {error}");
            }
            Ok(LaunchOutcome::Ready {
                connection,
                session_id,
                modes,
                config_options,
                notice,
            })
        });

        let tag_owned = tag_name.to_string();
        let task = cx.spawn(async move |this, cx| {
            let result = launch.await;
            this.update(cx, |pane, cx| {
                pane.on_launch_finished(&tag_owned, generation, result, cx)
            })
            .ok();
        });
        if let Some(entry) = self.projects.get_mut(tag_name) {
            entry._launch_task = Some(task);
        }
    }

    fn on_launch_finished(
        &mut self,
        tag_name: &str,
        generation: u64,
        result: anyhow::Result<LaunchOutcome>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.projects.get_mut(tag_name) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        match result {
            Ok(LaunchOutcome::Ready {
                mut connection,
                session_id,
                modes,
                config_options,
                notice,
            }) => {
                let events = forward_events(&mut connection, tag_name, cx);
                entry.transcript.seed_controls(modes, config_options);
                if let Some(notice) = notice {
                    // An agent failure belongs in the app's notification log
                    // as well as the transcript, which the pane owns.
                    notifications::report(cx, Severity::Error, notice.clone());
                    entry.transcript.push_notice(NoticeLevel::Error, notice);
                }
                entry.state = PaneState::Ready(Box::new(LiveSession {
                    connection,
                    session_id: Some(session_id),
                    _events_task: events,
                    _turn_task: None,
                }));
            }
            Ok(LaunchOutcome::Auth {
                mut connection,
                methods,
                detail,
            }) => {
                let events = forward_events(&mut connection, tag_name, cx);
                entry.transcript.push_error_entry(
                    "Authentication required",
                    detail,
                );
                entry.transcript.push_auth_required(methods);
                entry.state = PaneState::AuthRequired(Box::new(LiveSession {
                    connection,
                    session_id: None,
                    _events_task: events,
                    _turn_task: None,
                }));
            }
            Err(error) => {
                let title = format!("Could not start {}", self.agent.display_name());
                entry.state = PaneState::Failed {
                    title,
                    detail: error.to_string(),
                };
            }
        }
        let _ = self.set_busy(tag_name, false, cx);
        self.sync_scroller(cx);
        cx.notify();
    }

    /// Send a prompt, or queue it while a turn is running.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let text = self.prompt.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(entry) = self.projects.get_mut(&tag_name) else {
            return;
        };
        if !entry.has_session() {
            return;
        }
        self.prompt
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.slash_index = 0;
        if entry.busy {
            if entry.queue.len() < QUEUE_CAP {
                entry.queue.push_back(text);
            }
        } else {
            entry.transcript.push_user_message(text.clone());
            entry.busy = true;
            self.spawn_turn(&tag_name, text, cx);
        }
        self.set_busy(&tag_name, true, cx);
        self.sync_scroller(cx);
        cx.notify();
    }

    fn spawn_turn(&mut self, tag_name: &str, text: String, cx: &mut Context<Self>) {
        let Some(entry) = self.projects.get(tag_name) else {
            return;
        };
        let Some(live) = entry.live() else {
            return;
        };
        let Some(session_id) = live.session_id.clone() else {
            return;
        };
        let requester = live.connection.requester.clone();
        let turn = gpui_tokio::Tokio::spawn_result(cx, async move {
            requester
                .prompt(&session_id, text)
                .await
                .map(|stop_reason| format!("{stop_reason:?}"))
                .map_err(anyhow::Error::from)
        });
        let tag_owned = tag_name.to_string();
        let task = cx.spawn(async move |this, cx| {
            let result = turn.await;
            this.update(cx, |pane, cx| {
                pane.on_turn_finished(&tag_owned, result, cx)
            })
            .ok();
        });
        if let Some(entry) = self.projects.get_mut(tag_name) {
            if let Some(live) = entry.live_mut() {
                live._turn_task = Some(task);
            }
        }
    }

    fn on_turn_finished(
        &mut self,
        tag_name: &str,
        result: anyhow::Result<String>,
        cx: &mut Context<Self>,
    ) {
        let next = {
            let Some(entry) = self.projects.get_mut(tag_name) else {
                return;
            };
            entry.busy = false;
            if let Err(error) = result {
                let message = format!("The turn ended with an error: {error}");
                notifications::report(cx, Severity::Error, message.clone());
                entry.transcript.push_error(message);
            }
            entry.queue.pop_front()
        };
        self.set_busy(tag_name, false, cx);
        if let Some(next) = next {
            if let Some(entry) = self.projects.get_mut(tag_name) {
                entry.transcript.push_user_message(next.clone());
                entry.busy = true;
            }
            self.set_busy(tag_name, true, cx);
            self.spawn_turn(tag_name, next, cx);
        }
        self.sync_scroller(cx);
        cx.notify();
    }

    /// Stop the streaming turn and drop the queue, reporting what was lost.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let dropped = {
            let Some(entry) = self.projects.get_mut(&tag_name) else {
                return;
            };
            let dropped = entry.queue.len();
            entry.queue.clear();
            if let Some(live) = entry.live() {
                if let Some(session_id) = &live.session_id {
                    if let Err(error) = live.connection.requester.cancel(session_id) {
                        tracing::warn!("could not cancel the ACP turn: {error}");
                    }
                }
            }
            dropped
        };
        if dropped > 0 {
            if let Some(entry) = self.projects.get_mut(&tag_name) {
                entry.transcript.push_notice(
                    NoticeLevel::Info,
                    format!("Stop cleared {dropped} queued message(s)"),
                );
            }
        }
        self.sync_scroller(cx);
        cx.notify();
    }

    /// Drop the queue without touching the running turn.
    fn clear_queue(&mut self, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        if let Some(entry) = self.projects.get_mut(&tag_name) {
            let dropped = entry.queue.len();
            entry.queue.clear();
            if dropped > 0 {
                entry.transcript.push_notice(
                    NoticeLevel::Info,
                    format!("Cleared {dropped} queued message(s)"),
                );
            }
        }
        self.sync_scroller(cx);
        cx.notify();
    }

    /// `session/new` on a fresh process, replacing the stored id.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        if self.is_busy() {
            return;
        }
        self.start_session(&tag_name, false, cx);
        self.sync_scroller(cx);
        cx.notify();
    }

    /// Retry after a launch failure or a vanished directory.
    fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        if !self.projects.contains_key(&tag_name) {
            return;
        }
        self.start_session(&tag_name, true, cx);
        self.sync_scroller(cx);
        cx.notify();
    }

    /// Run an advertised auth method's `authenticate` handshake.
    fn sign_in(&mut self, method_id: String, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let requester = self
            .projects
            .get(&tag_name)
            .and_then(|entry| entry.live())
            .map(|live| live.connection.requester.clone());
        let Some(requester) = requester else {
            return;
        };
        let sign_in = gpui_tokio::Tokio::spawn_result(cx, async move {
            requester
                .authenticate(method_id)
                .await
                .map_err(anyhow::Error::from)
        });
        let tag_owned = tag_name.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = sign_in.await;
            this.update(cx, |pane, cx| {
                match result {
                    Ok(()) => pane.start_session(&tag_owned, true, cx),
                    Err(error) => {
                        let message = format!("Sign-in failed: {error}");
                        notifications::report(cx, Severity::Error, message.clone());
                        if let Some(entry) = pane.projects.get_mut(&tag_owned) {
                            entry.transcript.push_error(message);
                        }
                    }
                }
                pane.sync_scroller(cx);
                cx.notify();
            })
            .ok();
        });
        if let Some(entry) = self.projects.get_mut(&tag_name) {
            entry._misc_task = Some(task);
        }
    }

    /// Answer a pending permission request with one of the offered options.
    fn answer_permission(
        &mut self,
        entry_id: u64,
        choice: PermissionChoice,
        cx: &mut Context<Self>,
    ) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let Some(entry) = self.projects.get_mut(&tag_name) else {
            return;
        };
        let Some(reply) = entry.permission_replies.remove(&entry_id) else {
            return;
        };
        let decision =
            PermissionDecision::Selected(PermissionOptionId::new(choice.option_id.clone()));
        if let Err(decision) = reply.answer(decision) {
            tracing::debug!("permission request was already resolved: {decision:?}");
        }
        let record = PermissionRecord::from_choice(&choice, false);
        if let Some(index) = entry.transcript.resolve_permission(entry_id, record) {
            self.remeasure(index, cx);
        }
        cx.notify();
    }

    /// Remember an allow/deny rule for the tool behind a pending permission
    /// request: answer it with the strongest matching option, store the
    /// tool-level default, persist it, and apply it to the live connection.
    fn remember_permission(&mut self, entry_id: u64, allow: bool, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let (tool, choice) = {
            let Some(entry) = self.projects.get(&tag_name) else {
                return;
            };
            let tool = entry
                .permission_tools
                .get(&entry_id)
                .cloned()
                .unwrap_or_else(|| "other".to_string());
            let options = entry
                .transcript
                .index_of(entry_id)
                .and_then(|index| entry.transcript.entry(index))
                .and_then(|row| match &row.kind {
                    EntryKind::Permission { options, .. } => Some(options.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let sticky = if allow { "AllowAlways" } else { "RejectAlways" };
            let once = if allow { "AllowOnce" } else { "RejectOnce" };
            let choice = options
                .iter()
                .find(|option| option.kind == sticky)
                .or_else(|| options.iter().find(|option| option.kind == once))
                .cloned();
            (tool, choice)
        };
        if let Some(choice) = choice {
            self.answer_permission(entry_id, choice, cx);
        }
        let rule = if allow {
            PermissionRule::Allow
        } else {
            PermissionRule::Deny
        };
        let persisted = {
            let Some(entry) = self.projects.get_mut(&tag_name) else {
                return;
            };
            entry.tool_permissions.set_tool_default(&tool, rule);
            entry.permission_tools.remove(&entry_id);
            if let Some(live) = entry.live() {
                live.connection
                    .set_tool_permissions(entry.tool_permissions.clone());
            }
            entry
                .stored_path
                .clone()
                .map(|path| (path, entry.tool_permissions.clone()))
        };
        if let Some((path, permissions)) = persisted {
            if let Err(error) = self.session_store.set_tool_permissions(&path, permissions) {
                tracing::warn!("could not persist the tool approval policy: {error}");
            }
        }
        cx.notify();
    }

    /// Apply one batch of agent events. Batched on purpose: the transcript is
    /// notified once per batch instead of once per streamed chunk.
    fn on_agent_events(
        &mut self,
        tag_name: &str,
        events: Vec<AcpEvent>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.projects.get_mut(tag_name) else {
            return;
        };
        let mut appended = false;
        let mut last_changed: Option<usize> = None;
        let mut exited = false;
        for event in events {
            match event {
                AcpEvent::SessionUpdate(notification) => {
                    let delta = entry.transcript.apply(&notification);
                    if delta.appended {
                        appended = true;
                    }
                    last_changed = delta.changed.or(last_changed);
                }
                AcpEvent::PermissionRequested {
                    tool_call,
                    options,
                    decision,
                } => {
                    let title = permission_title(&tool_call);
                    let choices = PermissionChoice::from_options(&options);
                    let entry_id = entry.transcript.push_permission(title, choices);
                    entry.permission_replies.insert(entry_id, decision);
                    entry
                        .permission_tools
                        .insert(entry_id, acp_client::tool_key(&tool_call));
                    appended = true;
                }
                AcpEvent::PermissionAutoDecided { tool_call, choice } => {
                    let entry_id = entry
                        .transcript
                        .push_permission(permission_title(&tool_call), vec![choice.clone()]);
                    let record = PermissionRecord::from_choice(&choice, true);
                    last_changed = entry
                        .transcript
                        .resolve_permission(entry_id, record)
                        .or(last_changed);
                    appended = true;
                }
                AcpEvent::TerminalOutput {
                    terminal_id,
                    chunk,
                    truncated,
                } => {
                    let index = entry.transcript.index_of_terminal(&terminal_id);
                    let row = entry.terminals.entry(terminal_id).or_default();
                    row.output.push_str(&chunk);
                    row.truncated = truncated;
                    cap_lines(&mut row.output, TERMINAL_MAX_LINES);
                    last_changed = index.or(last_changed);
                }
                AcpEvent::TerminalExited {
                    terminal_id,
                    exit_code,
                    ..
                } => {
                    let index = entry.transcript.index_of_terminal(&terminal_id);
                    let row = entry.terminals.entry(terminal_id).or_default();
                    row.exit_code = Some(exit_code);
                    last_changed = index.or(last_changed);
                }
                AcpEvent::Stderr(line) => {
                    tracing::debug!("agent stderr: {line}");
                }
                AcpEvent::Exited { code, .. } => {
                    let detail = entry
                        .live()
                        .map(|live| live.connection.recent_stderr().join("\n"))
                        .unwrap_or_default();
                    entry.state = PaneState::Failed {
                        title: match code {
                            Some(code) => format!("Agent exited with status {code}"),
                            None => "Agent exited".to_string(),
                        },
                        detail,
                    };
                    entry.busy = false;
                    exited = true;
                }
            }
        }
        // `entry` borrows `projects` while the scroller is a sibling field,
        // so the two can be used together without a second self borrow.
        if appended {
            self.row_count = entry.transcript.len();
            self.scroller.update(cx, |state, cx| {
                state.append(1, cx);
            });
        }
        if let Some(index) = last_changed {
            self.scroller.update(cx, |state, cx| {
                state.remeasure_items(index..index + 1, cx);
            });
        }
        if let Some((_previous, current)) = entry.mode_notice() {
            entry
                .transcript
                .push_notice(NoticeLevel::Info, format!("Mode changed to {current}"));
            self.row_count = entry.transcript.len();
            self.scroller.update(cx, |state, cx| {
                state.append(1, cx);
            });
        }
        if exited {
            self.set_busy(tag_name, false, cx);
        }
        cx.notify();
    }

    /// Keep the scroller's row count in step with the active transcript.
    fn sync_scroller(&mut self, cx: &mut Context<Self>) {
        let count = self
            .active_entry()
            .map(|entry| entry.transcript.len())
            .unwrap_or(0);
        if count == self.row_count {
            return;
        }
        self.row_count = count;
        self.scroller.update(cx, |state, cx| {
            state.reset(count, cx);
        });
    }

    fn remeasure(&mut self, index: usize, cx: &mut Context<Self>) {
        self.scroller.update(cx, |state, cx| {
            state.remeasure_items(index..index + 1, cx);
        });
    }

    fn set_busy(&mut self, tag_name: &str, busy: bool, cx: &mut Context<Self>) {
        let changed = self
            .projects
            .get_mut(tag_name)
            .map(|entry| {
                let changed = entry.busy != busy;
                entry.busy = busy;
                changed
            })
            .unwrap_or(false);
        if changed {
            cx.emit(AgentPaneEvent::BusyChanged {
                tag_name: tag_name.to_string(),
                busy,
            });
        }
    }

    /// Tear a project's session down: cancel, close, drop the process, and
    /// forget the stored session id.
    fn teardown(&mut self, tag_name: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.projects.remove(tag_name) else {
            return;
        };
        if let Some(live) = entry.live() {
            if let Some(session_id) = &live.session_id {
                if let Err(error) = live.connection.requester.cancel(session_id) {
                    tracing::debug!("cancel on teardown failed: {error}");
                }
            }
        }
        if let Some(path) = &entry.stored_path {
            if let Err(error) = self.session_store.remove(path) {
                tracing::warn!("could not forget the stored session: {error}");
            }
        }
        self.set_busy(tag_name, false, cx);
    }

    fn active_entry(&self) -> Option<&ProjectEntry> {
        self.active
            .as_ref()
            .and_then(|tag_name| self.projects.get(tag_name))
    }

    fn active_entry_mut(&mut self) -> Option<&mut ProjectEntry> {
        let tag_name = self.active.clone()?;
        self.projects.get_mut(&tag_name)
    }

    fn on_input_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                if !self.slash_open(cx) {
                    self.slash_index = 0;
                }
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } => {
                if *shift {
                    return;
                }
                if let Some(command) = self.highlighted_command(cx) {
                    self.accept_command(command, window, cx);
                    return;
                }
                self.submit(window, cx);
            }
            _ => {}
        }
    }

    /// Whether the prompt starts with `/` and has no argument yet.
    fn slash_open(&self, cx: &App) -> bool {
        let text = self.prompt.read(cx).value();
        text.starts_with('/') && !text.contains(char::is_whitespace)
    }

    /// The command Enter would accept right now.
    fn highlighted_command(&self, cx: &App) -> Option<String> {
        if !self.slash_open(cx) {
            return None;
        }
        self.matching_commands(cx)
            .get(self.slash_index)
            .cloned()
    }

    fn matching_commands(&self, cx: &App) -> Vec<String> {
        let text = self.prompt.read(cx).value();
        let filter = text.trim_start_matches('/').to_lowercase();
        let Some(entry) = self.active_entry() else {
            return Vec::new();
        };
        entry
            .transcript
            .controls()
            .commands
            .iter()
            .filter(|command| {
                command.name.to_lowercase().contains(&filter)
                    || command.description.to_lowercase().contains(&filter)
            })
            .map(|command| command.name.clone())
            .collect()
    }

    /// Fill the prompt with `/name ` so arguments can be appended.
    fn accept_command(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt
            .update(cx, |state, cx| state.set_value(format!("/{name} "), window, cx));
        self.slash_index = 0;
        cx.notify();
    }

    fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) {
        let matches = self.matching_commands(cx);
        if matches.is_empty() {
            self.slash_index = 0;
            return;
        }
        let len = matches.len() as isize;
        self.slash_index = (self.slash_index as isize + delta).rem_euclid(len) as usize;
        cx.notify();
    }

    fn toggle_expanded(&mut self, entry_id: u64, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry_mut() else {
            return;
        };
        if !entry.expanded.remove(&entry_id) {
            entry.expanded.insert(entry_id);
        }
        if let Some(index) = entry.transcript.index_of(entry_id) {
            self.remeasure(index, cx);
        }
        cx.notify();
    }

    /// Open (or close, when already open) a dropdown, clearing the model
    /// filter and focusing it when the model dropdown opens. Re-clicking the
    /// selector button works because the outside-mousedown that precedes the
    /// click only closes; the click itself is swallowed when it arrives
    /// within the ignore window (same idiom as the task-details pickers).
    fn open_overlay(
        &mut self,
        overlay: Overlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.as_ref() == Some(&overlay) {
            self.overlay = None;
        } else if self.take_recent_overlay_outside_close(&overlay) {
            // The mousedown before this click already closed this same
            // dropdown; don't reopen it. A different selector still opens.
        } else {
            self.overlay = Some(overlay);
        }
        self.overlay_cursor = 0;
        let focus_filter = matches!(self.overlay, Some(Overlay::Model));
        if self.overlay.is_some() {
            self.overlay_query.update(cx, |state, cx| {
                state.set_value("", window, cx);
                if focus_filter {
                    state.focus(window, cx);
                }
            });
        }
        cx.notify();
    }

    /// True when the given dropdown was closed by an outside mousedown
    /// within the ignore window, consuming the marker so only that closing
    /// click is swallowed.
    fn take_recent_overlay_outside_close(&mut self, overlay: &Overlay) -> bool {
        let Some((closed, closed_at)) = self.overlay_outside_closed.take() else {
            return false;
        };
        closed == *overlay && closed_at.elapsed() < OVERLAY_OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn on_overlay_query_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.overlay_cursor = 0;
                cx.notify();
            }
            // Enter picks the first visible row, then returns focus to the
            // prompt box.
            InputEvent::PressEnter { .. } => self.accept_overlay_first(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Title, visible `(value, label)` rows and current value for an open
    /// dropdown. The model list is narrowed by the fuzzy filter; mode and
    /// config lists are short enough to show whole. `None` when the backing
    /// chip vanished.
    fn overlay_rows(&self, overlay: &Overlay, cx: &App) -> Option<OverlayRows> {
        let (title, values, current) = match overlay {
            Overlay::Model => {
                let chip = self
                    .select_chips()
                    .into_iter()
                    .find(|chip| chip.is_model)?;
                (chip.name.clone(), chip.values.clone(), chip.current.clone())
            }
            Overlay::Mode => {
                if let Some(mode) = self
                    .active_entry()
                    .and_then(|entry| entry.transcript.controls().mode.clone())
                {
                    (
                        "Mode".to_string(),
                        mode.available_modes
                            .iter()
                            .map(|available| {
                                (available.id.0.to_string(), available.name.clone())
                            })
                            .collect(),
                        mode.current_mode_id.0.to_string(),
                    )
                } else {
                    // Agents like opencode report session mode as a
                    // `mode`-category config option instead of session modes.
                    let chip = self
                        .select_chips()
                        .into_iter()
                        .find(|chip| chip.is_mode)?;
                    (chip.name.clone(), chip.values.clone(), chip.current.clone())
                }
            }
            Overlay::Config(id) => {
                let chip = self
                    .select_chips()
                    .into_iter()
                    .find(|chip| &chip.id == id)?;
                (chip.name.clone(), chip.values.clone(), chip.current.clone())
            }
        };
        let values = if *overlay == Overlay::Model {
            let query = self.overlay_query.read(cx).value().to_string();
            let mut ranked: Vec<(usize, (String, String))> = values
                .into_iter()
                .filter_map(|(value, label)| {
                    fuzzy_rank(&query, &label).map(|score| (score, (value, label)))
                })
                .collect();
            ranked.sort_by_key(|(score, _)| *score);
            ranked.into_iter().map(|(_, row)| row).collect()
        } else {
            values
        };
        Some((title, values, current))
    }

    /// Pick the highlighted visible dropdown row (Enter in the filter
    /// field), then return focus to the prompt box.
    fn accept_overlay_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.clone() else {
            return;
        };
        let Some((_, values, _)) = self.overlay_rows(&overlay, cx) else {
            return;
        };
        let visible = values.len().min(OVERLAY_MAX_ROWS);
        let Some((value, _)) = values
            .into_iter()
            .take(visible)
            .nth(self.overlay_cursor.min(visible.saturating_sub(1)))
        else {
            return;
        };
        self.select_overlay_value(overlay, value, cx);
        self.focus_prompt(window, cx);
    }

    /// Route a picked dropdown value to the session: modes go through
    /// `select_mode`, everything else (models included) is a select-valued
    /// config option on its chip.
    fn select_overlay_value(
        &mut self,
        overlay: Overlay,
        value: String,
        cx: &mut Context<Self>,
    ) {
        match overlay {
            Overlay::Mode => self.select_mode(value, cx),
            Overlay::Config(config_id) => self.select_config_value(config_id, value, cx),
            Overlay::Model => {
                if let Some(chip) = self
                    .select_chips()
                    .into_iter()
                    .find(|chip| chip.is_model)
                {
                    self.select_config_value(chip.id, value, cx);
                }
            }
        }
    }

    fn move_overlay_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.clone() else {
            return;
        };
        // The cursor only travels the visible (capped) rows.
        let count = self
            .overlay_rows(&overlay, cx)
            .map(|(_, values, _)| values.len().min(OVERLAY_MAX_ROWS))
            .unwrap_or(0) as isize;
        if count == 0 {
            return;
        }
        self.overlay_cursor = (self.overlay_cursor as isize + delta).rem_euclid(count) as usize;
        cx.notify();
    }

    /// Change a select-valued session config option.
    fn select_config_value(&mut self, config_id: String, value_id: String, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let request = self.projects.get(&tag_name).and_then(|entry| entry.live()).and_then(
            |live| {
                live.session_id
                    .clone()
                    .map(|session_id| (live.connection.requester.clone(), session_id))
            },
        );
        let Some((requester, session_id)) = request else {
            return;
        };
        self.overlay = None;
        cx.spawn(async move |this, cx| {
            let result = requester
                .set_config_option(
                    &session_id,
                    SessionConfigId::new(config_id),
                    SessionConfigOptionValue::value_id(value_id),
                )
                .await;
            this.update(cx, |pane, cx| {
                if let Err(error) = result {
                    let message = format!("Could not change the option: {error}");
                    notifications::report(cx, Severity::Error, message.clone());
                    if let Some(entry) = pane.active_entry_mut() {
                        entry.transcript.push_error(message);
                    }
                    pane.sync_scroller(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Change the session mode.
    fn select_mode(&mut self, mode_id: String, cx: &mut Context<Self>) {
        let Some(tag_name) = self.active.clone() else {
            return;
        };
        let request = self.projects.get(&tag_name).and_then(|entry| entry.live()).and_then(
            |live| {
                live.session_id
                    .clone()
                    .map(|session_id| (live.connection.requester.clone(), session_id))
            },
        );
        let Some((requester, session_id)) = request else {
            return;
        };
        self.overlay = None;
        cx.spawn(async move |this, cx| {
            let result = requester
                .set_mode(&session_id, SessionModeId::new(mode_id))
                .await;
            this.update(cx, |pane, cx| {
                if let Err(error) = result {
                    let message = format!("Could not change the mode: {error}");
                    notifications::report(cx, Severity::Error, message.clone());
                    if let Some(entry) = pane.active_entry_mut() {
                        entry.transcript.push_error(message);
                    }
                    pane.sync_scroller(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The select-valued config options of the on-screen project, the
    /// model-shaped one first.
    fn select_chips(&self) -> Vec<SelectChip> {
        let Some(entry) = self.active_entry() else {
            return Vec::new();
        };
        let mut chips: Vec<SelectChip> = entry
            .transcript
            .controls()
            .config_options
            .iter()
            .filter_map(select_chip)
            .collect();
        chips.sort_by_key(|chip| !chip.is_model);
        chips
    }

    fn model_label(&self) -> Option<String> {
        self.select_chips()
            .into_iter()
            .find(|chip| chip.is_model)
            .map(|chip| chip.current_label)
    }
}

/// `(session id, modes, config options, resume notice)`.
type SessionOpened = (
    SessionId,
    Option<SessionModeState>,
    Vec<SessionConfigOption>,
    Option<String>,
);

impl ProjectEntry {
    fn session_title(&self) -> Option<&str> {
        self.transcript
            .controls()
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
    }
}

/// What the launch task hands back to the pane.
enum LaunchOutcome {
    Ready {
        connection: AcpConnection,
        session_id: SessionId,
        modes: Option<SessionModeState>,
        config_options: Vec<SessionConfigOption>,
        notice: Option<String>,
    },
    Auth {
        connection: AcpConnection,
        methods: Vec<AuthMethodRow>,
        detail: String,
    },
}

/// Take the connection's event stream and forward it into the pane, one
/// notify per batch.
fn forward_events(
    connection: &mut AcpConnection,
    tag_name: &str,
    cx: &mut Context<AgentPane>,
) -> Task<()> {
    let mut events = connection.take_events();
    let tag_owned = tag_name.to_string();
    cx.spawn(async move |this, cx| {
        let mut batch: Vec<AcpEvent> = Vec::with_capacity(64);
        loop {
            let received = events.recv_many(&mut batch, 64).await;
            if received == 0 {
                break;
            }
            let drained = std::mem::take(&mut batch);
            if this
                .update(cx, |pane, cx| {
                    pane.on_agent_events(&tag_owned, drained, cx)
                })
                .is_err()
            {
                break;
            }
        }
    })
}

/// Create the session on an established connection.
async fn create_session(
    connection: &AcpConnection,
    cwd: &std::path::Path,
    additional: &[PathBuf],
) -> anyhow::Result<(SessionId, Option<SessionModeState>, Vec<SessionConfigOption>)> {
    let (session_id, modes, config_options) = connection
        .requester
        .new_session(cwd.to_path_buf(), additional.to_vec())
        .await?;
    Ok((session_id, modes, config_options))
}

/// Flatten a select-valued config option for the chip UI.
fn select_chip(option: &SessionConfigOption) -> Option<SelectChip> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let values: Vec<(String, String)> = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .map(|option| (option.value.0.to_string(), option.name.clone()))
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| {
                group
                    .options
                    .iter()
                    .map(|option| (option.value.0.to_string(), option.name.clone()))
            })
            .collect(),
        _ => Vec::new(),
    };
    let current = select.current_value.0.to_string();
    let current_label = values
        .iter()
        .find(|(value, _)| *value == current)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| current.clone());
    Some(SelectChip {
        id: option.id.0.to_string(),
        name: option.name.clone(),
        is_model: matches!(option.category, Some(SessionConfigOptionCategory::Model)),
        is_mode: matches!(option.category, Some(SessionConfigOptionCategory::Mode)),
        current,
        current_label,
        values,
    })
}

fn permission_title(tool_call: &ToolCallUpdate) -> String {
    tool_call
        .fields
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Tool call".to_string())
}

/// Keep only the last `max` lines of a growing block.
fn cap_lines(text: &mut String, max: usize) {
    let separators = text.bytes().filter(|byte| *byte == b'\n').count();
    let mut remaining = separators + 1;
    if remaining <= max {
        return;
    }
    let mut cut = 0;
    while remaining > max {
        let Some(next) = text[cut..].find('\n') else {
            break;
        };
        cut += next + 1;
        remaining -= 1;
    }
    text.drain(..cut);
}

/// The last `max` lines of a block, for the collapsed view.
fn tail_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_string();
    }
    lines[lines.len() - max..].join("\n")
}

fn status_of(status: ToolStatus) -> (&'static str, u32) {
    match status {
        ToolStatus::Pending => ("Pending", TEXT_FAINT),
        ToolStatus::Running => ("Running", TEXT_MUTED),
        ToolStatus::Completed => ("Done", SUCCESS),
        ToolStatus::Failed => ("Failed", DANGER),
    }
}

fn status_icon(status: ToolStatus) -> IconName {
    match status {
        ToolStatus::Pending => IconName::LoaderCircle,
        ToolStatus::Running => IconName::LoaderCircle,
        ToolStatus::Completed => IconName::CircleCheck,
        ToolStatus::Failed => IconName::CircleX,
    }
}

/// One line of a rendered file diff.
enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// A minimal line diff: the common prefix and suffix are context, the middle
/// is removals followed by additions. Enough for the small edits an agent
/// makes, without pulling in a diff library.
fn diff_lines(old: Option<&str>, new: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.unwrap_or_default().lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut lines = Vec::new();
    for line in &old_lines[..prefix] {
        lines.push(DiffLine::Context((*line).to_string()));
    }
    for line in &old_lines[prefix..old_lines.len() - suffix] {
        lines.push(DiffLine::Removed((*line).to_string()));
    }
    for line in &new_lines[prefix..new_lines.len() - suffix] {
        lines.push(DiffLine::Added((*line).to_string()));
    }
    for line in &old_lines[old_lines.len() - suffix..] {
        lines.push(DiffLine::Context((*line).to_string()));
    }
    lines
}

/// Whether this is the first agent message after the last prompt, which is
/// where the agent header (name + model) is drawn.
fn is_first_agent_message(transcript: &Transcript, index: usize) -> bool {
    transcript.entries()[..index].iter().rev().all(|entry| {
        !matches!(
            entry.kind,
            EntryKind::UserMessage { .. } | EntryKind::AgentText { .. }
        )
    })
}

fn centred_hint(text: impl Into<String>) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .text_sm()
        .text_color(rgb(TEXT_MUTED))
        .child(div().max_w(px(320.)).child(text.into()))
        .into_any_element()
}

fn card(children: Vec<AnyElement>) -> AnyElement {
    let mut element = div()
        .my_2()
        .mx_3()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .bg(rgb(CARD_BG))
        .border_1()
        .border_color(rgb(HAIRLINE))
        .rounded_lg();
    for child in children {
        element = element.child(child);
    }
    element.into_any_element()
}

/// A small ghost icon button, matching the navbar's footer rows.
fn icon_button(id: impl Into<ElementId>, icon: IconName, tooltip: &str) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .icon(icon)
        .tooltip(tooltip.to_string())
}

impl AgentPane {
    fn render_transcript(&self, weak: &WeakEntity<Self>, _cx: &mut Context<Self>) -> AnyElement {
        let row_weak = weak.clone();
        MessageScroller::new(
            ElementId::Name("agent-transcript".into()),
            self.scroller.clone(),
            move |index, _window, cx| {
                row_weak
                    .read_with(cx, |pane, _cx| pane.render_row(&row_weak, index))
                    .unwrap_or_else(|_| div().into_any_element())
            },
        )
        .scrollbar(true)
        .jump_button(true)
        .with_jump_button_label("Jump to latest")
        .into_any_element()
    }

    fn render_row(&self, weak: &WeakEntity<Self>, index: usize) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return div().into_any_element();
        };
        let Some(row) = entry.transcript.entry(index) else {
            return div().into_any_element();
        };
        let entry_id = row.id;
        let expanded = entry.expanded.contains(&entry_id);
        match &row.kind {
            EntryKind::UserMessage { text } => div()
                .w_full()
                .flex()
                .justify_end()
                .py_1()
                .child(
                    div()
                        .max_w(px(320.))
                        .px_3()
                        .py_2()
                        .bg(rgb(CARD_BG))
                        .rounded_lg()
                        .child(TextView::markdown(format!("user-{entry_id}"), text.clone())),
                )
                .into_any_element(),
            EntryKind::AgentText { text } => {
                let mut element = div().w_full().flex().flex_col().gap_1().py_1();
                if is_first_agent_message(&entry.transcript, index) {
                    element = element.child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child(format!(
                                "{}{}",
                                self.agent.display_name(),
                                self.model_label()
                                    .map(|model| format!(" · {model}"))
                                    .unwrap_or_default()
                            )),
                    );
                }
                element
                    .child(
                        div()
                            .w_full()
                            .child(TextView::markdown(format!("agent-{entry_id}"), text.clone())),
                    )
                    .into_any_element()
            }
            EntryKind::Thought { text } => {
                // A thought that is still streaming shows itself: the user
                // watches the reasoning arrive instead of a collapsed header.
                // Once it ends it collapses back to the one-line record.
                let live = entry.transcript.live_entry() == Some(index);
                let mut element = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_1()
                    .pl_3()
                    .border_l_1()
                    .border_color(rgb(if live { TEXT_FAINT } else { HAIRLINE }))
                    .child(div().flex().items_center().gap_1().child(if live {
                        // Nothing to collapse yet: the reasoning is still
                        // arriving, so the header is a plain live label.
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                gpui_component::spinner::Spinner::new()
                                    .with_size(px(10.))
                                    .color(rgb(TEXT_FAINT).into()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("Thinking…"),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .id(format!("thought-toggle-{entry_id}"))
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx: &mut App| {
                                    weak.update(cx, |pane, cx| pane.toggle_expanded(entry_id, cx))
                                        .ok();
                                }
                            })
                            .child(icon(
                                if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                },
                                TEXT_FAINT,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("Thinking"),
                            )
                            .into_any_element()
                    }));
                if expanded || live {
                    element = element.child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child(TextView::markdown(
                                format!("thought-{entry_id}"),
                                text.clone(),
                            )),
                    );
                }
                element.into_any_element()
            }
            EntryKind::ToolCall {
                title,
                kind,
                status,
                output,
                diffs,
                terminal_id,
                ..
            } => {
                let (status_label, status_color) = status_of(*status);
                let terminal = terminal_id
                    .as_deref()
                    .and_then(|id| entry.terminals.get(id));
                let mut element = div()
                    .w_full()
                    .my_1()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .bg(rgb(CARD_BG))
                    .border_1()
                    .border_color(rgb(HAIRLINE))
                    .rounded_md()
                    .child(
                        div()
                            .id(format!("tool-toggle-{entry_id}"))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx: &mut App| {
                                    weak.update(cx, |pane, cx| pane.toggle_expanded(entry_id, cx))
                                        .ok();
                                }
                            })
                            .child(icon(status_icon(*status), status_color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(rgb(TEXT_STRONG))
                                    .truncate()
                                    .child(if title.trim().is_empty() {
                                        kind.clone()
                                    } else {
                                        title.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(status_color))
                                    .child(status_label),
                            )
                            .child(icon(
                                if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                },
                                TEXT_FAINT,
                            )),
                    );
                if expanded {
                    if let Some(output) = output.as_deref().filter(|text| !text.is_empty()) {
                        let truncated = output.lines().count() > TOOL_MAX_LINES;
                        element = element.child(block(&tail_lines(output, TOOL_MAX_LINES), None));
                        if truncated {
                            element = element.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child("Earlier output was truncated"),
                            );
                        }
                    }
                    for diff in diffs {
                        element = element.child(render_diff(diff));
                    }
                }
                if let Some(terminal) = terminal {
                    element = element.child(render_terminal(
                        weak,
                        entry_id,
                        terminal_id.as_deref().unwrap_or_default(),
                        &terminal.output,
                        terminal.exit_code,
                        terminal.truncated,
                        expanded,
                    ));
                }
                element.into_any_element()
            }
            EntryKind::Plan { rows } => {
                let mut element = div()
                    .w_full()
                    .my_1()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .bg(rgb(PANEL_BG))
                    .rounded_md()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(rgb(TEXT_MUTED))
                            .child("Plan"),
                    );
                for row in rows {
                    element = element.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(row.status.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(row.text.clone()),
                            ),
                    );
                }
                element.into_any_element()
            }
            EntryKind::Permission {
                title,
                options,
                decision,
            } => render_permission(weak, entry_id, title, options, decision.as_ref()),
            EntryKind::Notice { level, text } => div()
                .w_full()
                .py_1()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(rgb(match level {
                    NoticeLevel::Info => TEXT_MUTED,
                    NoticeLevel::Error => DANGER,
                }))
                .child(text.clone())
                .into_any_element(),
            EntryKind::Error { title, detail } => {
                render_error(weak, title, detail, true)
            }
            EntryKind::AuthRequired { methods } => render_auth(weak, methods),
            EntryKind::Usage { used, size, cost } => {
                let mut label = format!("{used}/{size} tokens");
                if let Some(cost) = cost {
                    label.push_str(&format!(" · ${cost:.4}"));
                }
                div()
                    .w_full()
                    .py_1()
                    .flex()
                    .justify_end()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(label)
                    .into_any_element()
            }
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let project = self.active_project();
        // The pane runs the agent in the run's checkout, not the project
        // directory, while a coding run is active (§6.3).
        let worktree = self
            .active_entry()
            .is_some_and(|entry| entry.project.checkout.is_some());
        let title = project
            .and_then(|_| self.active_entry())
            .and_then(|entry| entry.session_title())
            .map(|title| title.to_string())
            .or_else(|| project.map(|project| project.label.clone()))
            .unwrap_or_else(|| "Agent".to_string());
        let model = self.model_label();
        let ready = self
            .active_entry()
            .map(|entry| entry.has_session())
            .unwrap_or(false);
        let busy = self.is_busy();
        div()
            .flex_none()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(HAIRLINE))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(TEXT_STRONG))
                    .truncate()
                    .child(title),
            )
            .when(worktree, |this| {
                this.child(
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(rgb(PANEL_HOVER))
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child("worktree"),
                )
            })
            .when_some(model, |this, model| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .truncate()
                        .child(model),
                )
            })
            .child(
                Button::new("agent-new-session")
                    .ghost()
                    .compact()
                    .icon(IconName::Plus)
                    .disabled(!ready || busy)
                    .tooltip(if busy {
                        "Stop the running turn first"
                    } else {
                        "Start a new session"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.new_session(cx))),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let Some(entry) = self.active_entry() else {
            return centred_hint("Select a project with a directory to use the agent.");
        };
        match &entry.state {
            PaneState::NoDirectory { candidates } => render_no_directory(candidates),
            PaneState::Launching { command, cwd } => {
                let name = self.agent.display_name().to_string();
                render_launching(&name, command, cwd)
            }
            PaneState::Failed { title, detail } => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(render_error(&weak, title, detail, true))
                .child(div().flex_1().min_h_0().child(
                    self.render_transcript(&weak, cx),
                ))
                .into_any_element(),
            PaneState::Ready(_) | PaneState::AuthRequired(_) if entry.transcript.is_empty() => {
                let project = entry.project.label.clone();
                let dirs = entry
                    .stored_path
                    .clone()
                    .unwrap_or_else(|| "no directory".to_string());
                centred_hint(format!("Ask the agent about {project}\n{dirs}"))
            }
            PaneState::Ready(_) | PaneState::AuthRequired(_) => {
                self.render_transcript(&weak, cx)
            }
        }
    }

    fn render_prompt(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let ready = self
            .active_entry()
            .map(|entry| entry.has_session())
            .unwrap_or(false);
        let busy = self.is_busy();
        let queue_len = self.queue_len();
        let text = self.prompt.read(cx).value().trim().to_string();
        let can_send = ready && !text.is_empty() && queue_len < QUEUE_CAP;
        let slash_open = self.slash_open(cx);
        // The session's cwd is the run's worktree while one is active: label
        // it, so where the agent's edits land is never a guess (§6.3).
        let cwd = self
            .active_entry()
            .and_then(|entry| entry.stored_path.clone());
        let worktree = self
            .active_entry()
            .and_then(|entry| entry.project.checkout.as_ref())
            .map(|checkout| checkout.worktree.display().to_string());
        let cwd = match (cwd, worktree) {
            (Some(cwd), Some(worktree)) if cwd == worktree => Some(format!("worktree · {cwd}")),
            (cwd, _) => cwd,
        };
        let usage = self
            .active_entry()
            .and_then(|entry| entry.transcript.latest_usage());

        div()
            .flex_none()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(rgb(HAIRLINE))
            .key_context(AGENT_PANE_CONTEXT)
            .on_action(cx.listener(|this, _: &SlashUp, _, cx| {
                if this.overlay.is_some() {
                    this.move_overlay_cursor(-1, cx);
                } else {
                    this.move_slash(-1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SlashDown, _, cx| {
                if this.overlay.is_some() {
                    this.move_overlay_cursor(1, cx);
                } else {
                    this.move_slash(1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DismissOverlay, _, cx| {
                if this.dismiss_overlay(cx) {
                    cx.stop_propagation();
                }
            }))
            // The live "something is happening" line, so a slow first token
            // never looks like a dead pane.
            .when_some(self.render_activity(), |this, activity| {
                this.child(activity)
            })
            .child(self.render_controls(busy, can_send, queue_len, cx))
            .when(slash_open, |this| this.child(self.render_slash(cx)))
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(HAIRLINE))
                    .bg(rgb(CARD_BG))
                    .child(
                        Textarea::new(&self.prompt)
                            .h(px(88.))
                            .appearance(false)
                            .disabled(!ready)
                            .aria_label("Message the agent"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    // Fixed height: the overlay card hangs off the prompt
                    // bottom by exact pixel math (see render_overlay), so
                    // this row must not size to content.
                    .h(px(22.))
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .when_some(cwd, |this, cwd| {
                        this.child(div().min_w_0().truncate().child(cwd))
                    })
                    .child(div().flex_1())
                    .when(usage.is_some(), |this| {
                        let (used, size, cost) = usage.unwrap_or((0, 0, None));
                        let mut label = format!("{used}/{size}");
                        if let Some(cost) = cost {
                            label.push_str(&format!(" · ${cost:.4}"));
                        }
                        this.child(div().child(label))
                    }),
            )
            .into_any_element()
    }

    /// The turn's live status: a spinner plus what the agent is doing right
    /// now, so a slow first token never looks like a dead pane. `None` when the
    /// pane is idle, so holding still costs nothing.
    fn render_activity(&self) -> Option<AnyElement> {
        if !self.is_busy() {
            return None;
        }
        let entry = self.active_entry()?;
        // The turn paused on the user rather than on the agent: no spinner,
        // because nothing is running until the card is answered.
        let (label, spinning) = if entry.transcript.has_pending_permission() {
            ("Waiting for your approval".to_string(), false)
        } else {
            let label = match entry.transcript.activity() {
                Some(Activity::Thinking) => "Thinking…".to_string(),
                Some(Activity::Writing) => "Writing…".to_string(),
                Some(Activity::Tool { title }) => format!("Running {title}…"),
                // The turn has started but no content has arrived yet.
                None => "Waiting for the agent…".to_string(),
            };
            (label, true)
        };
        let mut row = div().flex_none().h_flex().items_center().gap_2().px_1();
        row = row.child(if spinning {
            gpui_component::spinner::Spinner::new()
                .with_size(px(12.))
                .color(rgb(TEXT_MUTED).into())
                .into_any_element()
        } else {
            div().text_xs().text_color(rgb(TEXT_FAINT)).child("●").into_any_element()
        });
        Some(
            row.child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(label))
                .into_any_element(),
        )
    }

    fn render_controls(
        &self,
        busy: bool,
        can_send: bool,
        queue_len: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chips = self.select_chips();
        let models: Vec<SelectChip> = chips
            .iter()
            .filter(|chip| chip.is_model)
            .cloned()
            .collect();
        let others: Vec<SelectChip> = chips
            .iter()
            .filter(|chip| !chip.is_model && !chip.is_mode)
            .cloned()
            .collect();
        // Session mode comes from session `modes` when the agent sends them;
        // opencode reports it as a `mode`-category config option instead, so
        // that chip backs the same dedicated button.
        let mode_label: Option<String> = self
            .active_entry()
            .and_then(|entry| entry.transcript.controls().mode.clone())
            .filter(|mode| !mode.available_modes.is_empty())
            .map(|mode| {
                let current = mode.current_mode_id.0.to_string();
                mode.available_modes
                    .iter()
                    .find(|available| available.id.0.to_string() == current)
                    .map(|available| available.name.clone())
                    .unwrap_or_else(|| current.clone())
            })
            .or_else(|| {
                chips
                    .iter()
                    .find(|chip| chip.is_mode)
                    .map(|chip| chip.current_label.clone())
            });

        div()
            .flex()
            .items_center()
            .gap_1()
            // Fixed height: the overlay card hangs off the prompt bottom by
            // exact pixel math (see render_overlay), so this row must not
            // size to content. 32px fits the tallest children (compact
            // default-size icon/send buttons).
            .h(px(32.))
            .children(models.into_iter().map(|chip| {
                Button::new(SharedString::from(format!("agent-model-{}", chip.id)))
                    .ghost()
                    .compact()
                    .with_size(gpui_component::Size::XSmall)
                    .text_color(rgb(TEXT_MUTED))
                    .icon(IconName::ChevronsUpDown)
                    .label(chip.current_label.clone())
                    .tooltip(format!("Model · {}", chip.name))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_overlay(Overlay::Model, window, cx)
                    }))
                    .into_any_element()
            }))
            .when_some(mode_label, |this, label| {
                this.child(
                    Button::new("agent-mode")
                        .ghost()
                        .compact()
                        .with_size(gpui_component::Size::XSmall)
                        .text_color(rgb(TEXT_MUTED))
                        .icon(IconName::ChevronsUpDown)
                        .label(label)
                        .tooltip("Session mode")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_overlay(Overlay::Mode, window, cx)
                        })),
                )
            })
            .children(others.into_iter().map(|chip| {
                let config_id = chip.id.clone();
                Button::new(SharedString::from(format!("agent-config-{config_id}")))
                    .ghost()
                    .compact()
                    .with_size(gpui_component::Size::Small)
                    .icon(IconName::ChevronsUpDown)
                    .label(chip.current_label.clone())
                    .tooltip(chip.name.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_overlay(Overlay::Config(config_id.clone()), window, cx)
                    }))
                    .into_any_element()
            }))
            .child(div().flex_1())
            .when(queue_len > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .rounded_md()
                        .bg(rgb(PANEL_HOVER))
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(format!("Queued · {queue_len}"))
                        .child(icon_button(
                            "agent-queue-clear",
                            IconName::Close,
                            "Clear the queue",
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.clear_queue(cx)))),
                )
            })
            .child(
                icon_button(
                    "agent-attach-task",
                    IconName::FileText,
                    "Insert the selected task",
                )
                .on_click(cx.listener(|_this, _, _, cx| {
                    cx.emit(AgentPaneEvent::AttachTaskRequested);
                })),
            )
            .child(if busy {
                icon_button("agent-stop", IconName::Pause, "Stop the turn")
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                    .into_any_element()
            } else {
                Button::new("agent-send")
                    .primary()
                    .compact()
                    .icon(IconName::ArrowUp)
                    .disabled(!can_send)
                    .tooltip(if queue_len >= QUEUE_CAP {
                        "Queue full"
                    } else {
                        "Send"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_slash(&self, cx: &mut Context<Self>) -> AnyElement {
        let matches = self.matching_commands(cx);
        let Some(entry) = self.active_entry() else {
            return div().into_any_element();
        };
        let commands = entry.transcript.controls().commands.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(HAIRLINE))
            .rounded_md();
        if matches.is_empty() {
            return div().into_any_element();
        }
        for (index, name) in matches.iter().enumerate() {
            let Some(command) = commands.iter().find(|command| &command.name == name) else {
                continue;
            };
            let highlighted = index == self.slash_index;
            list = list.child(
                div()
                    .id(SharedString::from(format!("agent-slash-{index}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(highlighted, |this| this.bg(rgb(PANEL_HOVER)))
                    .hover(|this| this.bg(rgb(PANEL_HOVER)))
                    .on_click({
                        let name = command.name.clone();
                        cx.listener(move |this, _, window, cx| {
                            this.accept_command(name.clone(), window, cx)
                        })
                    })
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_xs()
                            .text_color(rgb(TEXT_STRONG))
                            .child(format!("/{}", command.name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child(command.description.clone()),
                    ),
            );
        }
        list.into_any_element()
    }

    /// Floating card above the controls row: a fuzzy filter for the model
    /// list, then tight option rows. Rendered last in the pane (so it paints
    /// on top) and hung off the prompt bottom by exact pixel math — the rows
    /// below the buttons have fixed heights, so it floats just above the
    /// selector buttons spanning the pane width: bottom pad 8 + footer 22 +
    /// gap 8 + prompt box 88 + its border 2 + gap 8 + controls row 32 = 168,
    /// plus a tiny margin. Closes on outside click or Escape.
    fn render_overlay(&self, overlay: Overlay, cx: &mut Context<Self>) -> AnyElement {
        let Some((title, all_values, current)) = self.overlay_rows(&overlay, cx) else {
            return div().into_any_element();
        };
        let hidden = all_values.len().saturating_sub(OVERLAY_MAX_ROWS);
        let values: Vec<(String, String)> =
            all_values.into_iter().take(OVERLAY_MAX_ROWS).collect();
        let cursor = self
            .overlay_cursor
            .min(values.len().saturating_sub(1));
        let show_filter = overlay == Overlay::Model;

        let mut list = div()
            .absolute()
            .bottom(px(168.))
            .left(px(8.))
            .right(px(8.))
            .mb_1()
            .flex()
            .flex_col()
            .gap_0()
            .p_1()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(HAIRLINE))
            .rounded_md()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if let Some(closed) = this.overlay.take() {
                    this.overlay_outside_closed = Some((closed, std::time::Instant::now()));
                }
                cx.notify();
            }))
            .child(
                div()
                    .px_2()
                    .py_0p5()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(TEXT_MUTED))
                    .child(title),
            );
        if show_filter {
            list = list.child(
                div()
                    .px_1()
                    .pb_1()
                    .child(Input::new(&self.overlay_query).with_size(gpui_component::Size::Small)),
            );
        }
        if values.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child("No matches"),
            );
        }
        for (index, (value, label)) in values.into_iter().enumerate() {
            let selected = value == current;
            let highlighted = index == cursor;
            list = list.child(
                div()
                    .id(SharedString::from(format!("agent-option-{value}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .when(highlighted, |this| this.bg(rgb(PANEL_HOVER)))
                    .hover(|this| this.bg(rgb(PANEL_HOVER)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Clicks select immediately; the row's overlay value
                        // is fixed at render time.
                        let overlay = this.overlay.clone();
                        if let Some(overlay) = overlay {
                            this.select_overlay_value(overlay, value.clone(), cx);
                            this.focus_prompt(window, cx);
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(TEXT_STRONG))
                            .child(label),
                    )
                    .when(selected, |this| this.child(icon(IconName::Check, SUCCESS))),
            );
        }
        if hidden > 0 {
            list = list.child(
                div()
                    .px_2()
                    .py_0p5()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(format!("{hidden} more — keep typing to narrow")),
            );
        }
        list.into_any_element()
    }
}

/// Subsequence fuzzy match with a simple rank: consecutive prefix matches
/// score best, plain subsequence matches after that. Shared shape with the
/// project picker's filter, for narrowing the model list by typing.
fn fuzzy_rank(query: &str, name: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_lowercase();
    let name_lower = name.to_lowercase();
    if let Some(prefix) = name_lower.strip_prefix(&query) {
        return Some(prefix.len());
    }
    let mut search = name_lower.char_indices().peekable();
    let mut matched: Vec<usize> = Vec::new();
    for q in query.chars() {
        loop {
            match search.next() {
                Some((index, c)) if c == q => {
                    matched.push(index);
                    break;
                }
                Some(_) => continue,
                None => return None,
            }
        }
    }
    let spread = matched.last().copied().unwrap_or(0) - matched.first().copied().unwrap_or(0);
    Some(name_lower.len() + spread)
}

fn icon(name: IconName, color: u32) -> AnyElement {
    div()
        .flex_none()
        .text_color(rgb(color))
        .child(name)
        .into_any_element()
}

/// A monospace block with an optional scroll cap.
fn block(text: &str, max_lines: Option<usize>) -> AnyElement {
    let body = match max_lines {
        Some(max) => tail_lines(text, max),
        None => text.to_string(),
    };
    div()
        .w_full()
        .p_2()
        .bg(rgb(PANEL_BG))
        .rounded_md()
        .font_family(MONO_FONT)
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(body)
        .into_any_element()
}

fn render_diff(diff: &acp_client::thread::FileDiff) -> AnyElement {
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(icon(IconName::FileText, TEXT_MUTED))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(diff.path.clone()),
                ),
        );
    let lines = diff_lines(diff.old_text.as_deref(), &diff.new_text);
    let shown = if lines.len() > 200 {
        &lines[lines.len() - 200..]
    } else {
        &lines[..]
    };
    let mut body = div()
        .w_full()
        .p_2()
        .bg(rgb(PANEL_BG))
        .rounded_md()
        .flex()
        .flex_col()
        .font_family(MONO_FONT)
        .text_xs();
    for line in shown {
        let (prefix, text, tint, color) = match line {
            DiffLine::Context(text) => (" ", text, None, TEXT_MUTED),
            DiffLine::Added(text) => ("+", text, Some(DIFF_ADD_BG), SUCCESS),
            DiffLine::Removed(text) => ("-", text, Some(DIFF_DEL_BG), DANGER),
        };
        body = body.child(
            div()
                .w_full()
                .when_some(tint, |this, tint| this.bg(rgb(tint)))
                .text_color(rgb(color))
                .child(format!("{prefix}{text}")),
        );
    }
    element = element.child(body);
    element.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_terminal(
    weak: &WeakEntity<AgentPane>,
    entry_id: u64,
    terminal_id: &str,
    output: &str,
    exit_code: Option<Option<u32>>,
    truncated: bool,
    expanded: bool,
) -> AnyElement {
    let status = match exit_code {
        Some(Some(code)) if code == 0 => format!("exit {code}"),
        Some(Some(code)) => format!("exit {code}"),
        Some(None) => "finished".to_string(),
        None => "running".to_string(),
    };
    let status_color = match exit_code {
        Some(Some(0)) => SUCCESS,
        Some(_) => DANGER,
        None => TEXT_MUTED,
    };
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(IconName::SquareTerminal, TEXT_MUTED))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(terminal_id.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(status_color))
                        .child(status),
                )
                .when(output.lines().count() > TERMINAL_TAIL_LINES, |this| {
                    this.child(
                        div()
                            .id(format!("terminal-toggle-{entry_id}"))
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx: &mut App| {
                                    weak.update(cx, |pane, cx| pane.toggle_expanded(entry_id, cx))
                                        .ok();
                                }
                            })
                            .child(if expanded { "Show less" } else { "Show all" }),
                    )
                }),
        )
        .child(block(
            output,
            if expanded {
                Some(TERMINAL_MAX_LINES)
            } else {
                Some(TERMINAL_TAIL_LINES)
            },
        ));
    if truncated {
        element = element.child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child("Earlier output was truncated"),
        );
    }
    element.into_any_element()
}

fn render_permission(
    weak: &WeakEntity<AgentPane>,
    entry_id: u64,
    title: &str,
    options: &[PermissionChoice],
    decision: Option<&PermissionRecord>,
) -> AnyElement {
    if let Some(decision) = decision {
        let mut label = if decision.is_allow() {
            format!("Allowed · {}", decision.name)
        } else {
            format!("Rejected · {}", decision.name)
        };
        if decision.automatic {
            label.push_str(" (automatic)");
        }
        return div()
            .w_full()
            .py_1()
            .px_2()
            .my_1()
            .bg(rgb(PANEL_BG))
            .rounded_md()
            .text_xs()
            .text_color(rgb(if decision.is_allow() { TEXT_MUTED } else { DANGER }))
            .child(format!("{label} · {title}"))
            .into_any_element();
    }

    let mut card_element = div()
        .w_full()
        .my_1()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .bg(rgb(CARD_BG))
        .border_1()
        .border_color(rgb(HAIRLINE))
        .rounded_lg()
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(rgb(TEXT_MUTED))
                .child("Permission requested"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(TEXT_STRONG))
                .child(title.to_string()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(options.iter().enumerate().map(|(index, option)| {
                    let option = option.clone();
                    let allowed = option.is_allow();
                    let mut button = Button::new(SharedString::from(format!(
                        "permission-{entry_id}-{index}"
                    )))
                    .compact()
                    .label(option.name.clone());
                    button = if allowed {
                        button.primary()
                    } else {
                        button.ghost()
                    };
                    button
                        .tooltip(if option.is_sticky() {
                            "Remembered by the agent"
                        } else {
                            "This time only"
                        })
                        .on_click({
                            let weak = weak.clone();
                            move |_, _, cx: &mut App| {
                                weak.update(cx, |pane, cx| {
                                    pane.answer_permission(entry_id, option.clone(), cx)
                                })
                                .ok();
                            }
                        })
                        .into_any_element()
                })),
        );
    card_element = card_element
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("permission-always-allow-{entry_id}")))
                        .ghost()
                        .compact()
                        .with_size(gpui_component::Size::Small)
                        .label("Always allow")
                        .tooltip("Remember allow for this tool in this project")
                        .on_click({
                            let weak = weak.clone();
                            move |_, _, cx: &mut App| {
                                weak.update(cx, |pane, cx| {
                                    pane.remember_permission(entry_id, true, cx)
                                })
                                .ok();
                            }
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("permission-always-deny-{entry_id}")))
                        .ghost()
                        .compact()
                        .with_size(gpui_component::Size::Small)
                        .label("Always deny")
                        .tooltip("Remember deny for this tool in this project")
                        .on_click({
                            let weak = weak.clone();
                            move |_, _, cx: &mut App| {
                                weak.update(cx, |pane, cx| {
                                    pane.remember_permission(entry_id, false, cx)
                                })
                                .ok();
                            }
                        }),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child("Nothing proceeds until you choose."),
        );
    card_element.into_any_element()
}

fn render_error(
    weak: &WeakEntity<AgentPane>,
    title: &str,
    detail: &str,
    retryable: bool,
) -> AnyElement {
    let mut children = vec![div()
        .flex()
        .items_center()
        .gap_2()
        .child(icon(IconName::CircleX, DANGER))
        .child(
            div()
                .min_w_0()
                .text_sm()
                .font_semibold()
                .text_color(rgb(TEXT_STRONG))
                .child(title.to_string()),
        )
        .child(div().flex_1())
        .when(!detail.trim().is_empty(), |this| {
            let detail = detail.to_string();
            this.child(
                Button::new("agent-error-copy")
                    .ghost()
                    .compact()
                    .icon(IconName::Copy)
                    .tooltip("Copy the error text")
                    .on_click(move |_, _, cx: &mut App| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(detail.clone()));
                    }),
            )
        })
        .into_any_element()];
    if !detail.trim().is_empty() {
        children.push(render_error_detail(detail));
    }
    if retryable {
        children.push(
            div()
                .child(
                    Button::new("agent-retry")
                        .compact()
                        .label("Retry")
                        .on_click({
                            let weak = weak.clone();
                            move |_, _, cx: &mut App| {
                                weak.update(cx, |pane, cx| pane.retry(cx)).ok();
                            }
                        }),
                )
                .into_any_element(),
        );
    }
    card(children)
}

/// The error payload as a selectable, mono block so it can be copy-pasted. The
/// text view handles selection and double/triple-click; the row's copy button
/// copies the full (untruncated) detail.
fn render_error_detail(detail: &str) -> AnyElement {
    let body = tail_lines(detail, ERROR_TAIL_LINES);
    div()
        .w_full()
        .p_2()
        .bg(rgb(PANEL_BG))
        .rounded_md()
        .font_family(MONO_FONT)
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(
            TextView::markdown("agent-error-detail", body)
                .selectable(true)
                .max_lines(ERROR_TAIL_LINES),
        )
        .into_any_element()
}

fn render_auth(weak: &WeakEntity<AgentPane>, methods: &[AuthMethodRow]) -> AnyElement {
    let mut children = vec![
        div()
            .text_sm()
            .font_semibold()
            .text_color(rgb(TEXT_STRONG))
            .child("Sign in to continue")
            .into_any_element(),
    ];
    for (index, method) in methods.iter().enumerate() {
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_STRONG))
                            .child(method.name.clone()),
                    )
                    .when_some(method.description.clone(), |this, description| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(TEXT_MUTED))
                                .child(description),
                        )
                    }),
            );
        if method.is_terminal() {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child("Run this method's command in a terminal, then retry"),
            );
        } else {
            let method_id = method.id.clone();
            row = row.child(
                Button::new(SharedString::from(format!("auth-{index}")))
                    .compact()
                    .label("Sign in")
                    .on_click({
                        let weak = weak.clone();
                        move |_, _, cx: &mut App| {
                            weak.update(cx, |pane, cx| {
                                pane.sign_in(method_id.clone(), cx)
                            })
                            .ok();
                        }
                    }),
            );
        }
        children.push(row.into_any_element());
    }
    card(children)
}

fn render_no_directory(candidates: &[PathBuf]) -> AnyElement {
    let detail = if candidates.is_empty() {
        "This project has no directories configured.".to_string()
    } else {
        let list: Vec<String> = candidates
            .iter()
            .map(|path| format!("{} (missing)", path.display()))
            .collect();
        list.join("\n")
    };
    let mut children = vec![
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(IconName::CircleX, DANGER))
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(TEXT_STRONG))
                    .child("No project directory found"),
            )
            .into_any_element(),
        block(&detail, None),
    ];
    children.push(
        div()
            .text_xs()
            .text_color(rgb(TEXT_MUTED))
            .child("Add an existing directory to this tag, then reopen the pane.")
            .into_any_element(),
    );
    card(children)
}

fn render_launching(name: &str, command: &str, cwd: &std::path::Path) -> AnyElement {
    card(vec![
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(gpui_component::spinner::Spinner::new().color(rgb(TEXT_MUTED).into()))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT_STRONG))
                    .child(format!("Starting {name}…")),
            )
            .into_any_element(),
        block(
            &format!("{command}\n{}", cwd.display()),
            None,
        ),
    ])
}

impl Focusable for AgentPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<AgentPaneEvent> for AgentPane {}

impl Render for AgentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = self.render_body(cx);
        let prompt = self.render_prompt(window, cx);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .relative()
            .bg(rgb(APP_BG))
            .border_l_1()
            .border_color(rgb(HAIRLINE))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .child(prompt)
            // The open dropdown renders last so GPUI paints it above the
            // transcript and prompt (paint order follows tree order; there
            // is no z-index). It takes no layout space.
            .when_some(self.overlay.clone(), |this, overlay| {
                this.child(self.render_overlay(overlay, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_context_includes_title_and_description() {
        let context = task_context_text(
            "Ship the pane",
            Some("Needs a transcript"),
            &["work".to_string()],
        );
        assert!(context.starts_with("Task: Ship the pane"));
        assert!(context.contains("Needs a transcript"));
        assert!(context.contains("Tags: work"));
    }

    #[test]
    fn task_context_skips_empty_description() {
        assert_eq!(
            task_context_text("Only a title", Some("   "), &[]),
            "Task: Only a title"
        );
    }

    /// opencode's session mode arrives as a `mode`-category config option
    /// (not session `modes`): it must map to a mode chip backing the
    /// dedicated mode button, while the `model` category maps to a model
    /// chip. Shapes mirror the live `session/new` payload.
    #[test]
    fn mode_and_model_categories_map_to_their_chips() {
        let mode_option: SessionConfigOption = serde_json::from_value(serde_json::json!({
            "id": "mode",
            "name": "Session Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "build",
            "options": [
                {"value": "build", "name": "build"},
                {"value": "plan", "name": "plan"}
            ]
        }))
        .expect("mode option");
        let mode_chip = select_chip(&mode_option).expect("mode chip");
        assert!(mode_chip.is_mode);
        assert!(!mode_chip.is_model);
        assert_eq!(mode_chip.current_label, "build");
        assert_eq!(mode_chip.values.len(), 2);

        let model_option: SessionConfigOption = serde_json::from_value(serde_json::json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": "opencode/big-pickle",
            "options": [{"value": "opencode/big-pickle", "name": "Big Pickle"}]
        }))
        .expect("model option");
        let model_chip = select_chip(&model_option).expect("model chip");
        assert!(model_chip.is_model);
        assert!(!model_chip.is_mode);
    }

    #[test]
    fn cap_lines_keeps_the_tail() {
        let mut text = (0..10)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cap_lines(&mut text, 3);
        assert_eq!(text, "7\n8\n9");
    }

    #[test]
    fn diff_lines_marks_additions_and_removals() {
        let lines = diff_lines(Some("a\nb\nc"), "a\nB\nc");
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| match line {
                DiffLine::Context(text) => format!(" {text}"),
                DiffLine::Added(text) => format!("+{text}"),
                DiffLine::Removed(text) => format!("-{text}"),
            })
            .collect();
        assert_eq!(rendered, vec![" a", "-b", "+B", " c"]);
    }

    #[test]
    fn diff_lines_for_new_files_are_all_additions() {
        let lines = diff_lines(None, "one\ntwo");
        let added = lines
            .iter()
            .filter(|line| matches!(line, DiffLine::Added(_)))
            .count();
        assert_eq!(added, 2);
    }

    #[test]
    fn tail_lines_bounds_the_block() {
        let text = (0..500)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&text, 2);
        assert_eq!(tail, "498\n499");
    }

    /// A scratch tree with a repo, a sibling directory, and a worktree inside
    /// the repo, so `resolve()` runs against real directories.
    fn checkout_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let scratch = std::env::temp_dir().join(format!(
            "todo2-agent-pane-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        let repo_dir = scratch.join("repo");
        let sibling = scratch.join("docs");
        for dir in [&repo_dir, &sibling] {
            std::fs::create_dir_all(dir).expect("create dir");
        }
        // Resolution canonicalizes (`/var` is a symlink on macOS), so the
        // expected paths are canonical from the start.
        let root = scratch.canonicalize().unwrap_or(scratch);
        let repo_dir = root.join("repo");
        let sibling = root.join("docs");
        let worktree = repo_dir.join("worktrees").join("123-add-login");
        std::fs::create_dir_all(&worktree).expect("create worktree");
        (root, repo_dir, sibling, worktree)
    }

    fn project_with(
        repo_dir: PathBuf,
        sibling: PathBuf,
        checkout: Option<RunCheckout>,
    ) -> AgentProject {
        AgentProject {
            tag_id: 7,
            tag_name: "project:repo".to_string(),
            label: "repo".to_string(),
            candidates: vec![repo_dir, sibling],
            checkout,
        }
    }

    /// A run's checkout makes the worktree the primary root, drops the repo it
    /// replaced from the secondary roots so the agent cannot edit the user's
    /// checkout, keeps the project's other directories, and keys the session
    /// off the worktree — a different session from the interview at the
    /// project directory (decisions 11 and 21, §6.3).
    #[test]
    fn a_checkout_makes_the_worktree_the_primary_root() {
        let (root, repo_dir, sibling, worktree) = checkout_fixture("checkout");
        let checkout = RunCheckout {
            tag_id: 7,
            worktree: worktree.clone(),
            repo_dir: repo_dir.clone(),
            target_dir: Some(repo_dir.join("target")),
        };
        let (cwd, additional) = project_with(repo_dir.clone(), sibling.clone(), Some(checkout))
            .resolve()
            .expect("a directory resolves");
        assert_eq!(cwd, worktree);
        assert_eq!(additional, vec![sibling.clone()]);

        // Without the run, the project directory is the session's cwd again.
        let (cwd, additional) = project_with(repo_dir.clone(), sibling.clone(), None)
            .resolve()
            .expect("a directory resolves");
        assert_eq!(cwd, repo_dir);
        assert_eq!(additional, vec![sibling]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A worktree removed by hand must not keep the session pointed at a
    /// directory that is gone: the project directory takes over again.
    #[test]
    fn a_vanished_worktree_falls_back_to_the_project_directory() {
        let (root, repo_dir, sibling, worktree) = checkout_fixture("vanished");
        let checkout = RunCheckout {
            tag_id: 7,
            worktree: worktree.clone(),
            repo_dir: repo_dir.clone(),
            target_dir: Some(repo_dir.join("target")),
        };
        std::fs::remove_dir_all(&worktree).expect("remove the checkout");
        let (cwd, _) = project_with(repo_dir.clone(), sibling, Some(checkout))
            .resolve()
            .expect("a directory resolves");
        assert_eq!(cwd, repo_dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The isolated build cache is the absence of `CARGO_TARGET_DIR`, so the
    /// session builds where cargo would anyway (spec §6.4).
    #[test]
    fn an_isolated_project_points_a_session_at_no_build_directory() {
        let checkout = RunCheckout {
            tag_id: 7,
            worktree: PathBuf::from("/tmp/worktrees/123-add-login"),
            repo_dir: PathBuf::from("/tmp/repo"),
            target_dir: None,
        };
        assert_eq!(checkout_target_dir(Some(&checkout)), None);
        assert_eq!(checkout_target_dir(None), None);

        let shared = RunCheckout {
            target_dir: Some(PathBuf::from("/tmp/repo/target")),
            ..checkout
        };
        assert_eq!(
            checkout_target_dir(Some(&shared)),
            Some(PathBuf::from("/tmp/repo/target"))
        );
    }

    /// The launch wrapper adds `CARGO_TARGET_DIR` on top of the agent's own
    /// spec, leaving everything else (cwd, args, identity) untouched
    /// (decision 12).
    #[test]
    fn a_worktree_session_builds_into_the_runs_cache() {
        struct FixedAgent;

        impl AgentServer for FixedAgent {
            fn id(&self) -> &'static str {
                "fixed"
            }

            fn display_name(&self) -> &'static str {
                "Fixed"
            }

            fn program(&self) -> &'static str {
                "fixed"
            }

            fn args(&self) -> &'static [&'static str] {
                &["acp"]
            }

            fn spawn_spec(&self, cwd: &std::path::Path) -> Result<SpawnSpec, AcpError> {
                Ok(SpawnSpec {
                    program: self.program().into(),
                    args: self.args().iter().map(|arg| arg.to_string()).collect(),
                    cwd: cwd.to_path_buf(),
                    env: std::collections::BTreeMap::new(),
                })
            }
        }

        let agent = EnvAgent {
            inner: Arc::new(FixedAgent),
            name: "CARGO_TARGET_DIR",
            value: "/tmp/run-target".to_string(),
        };
        let spec = agent
            .spawn_spec(std::path::Path::new("/tmp/worktree"))
            .expect("spawn spec");
        assert_eq!(agent.id(), "fixed");
        assert_eq!(spec.cwd, PathBuf::from("/tmp/worktree"));
        assert_eq!(spec.command_line(), "fixed acp");
        assert_eq!(
            spec.env.get("CARGO_TARGET_DIR").map(String::as_str),
            Some("/tmp/run-target")
        );
    }
}
