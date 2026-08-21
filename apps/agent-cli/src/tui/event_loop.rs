// use crate::tui::Terminal;
// use cersei::{Agent, AgentStream};
// use std::sync::Arc;

//! Main TUI event loop — multiplexes agent streaming, terminal input, mouse, and paste.

// use crate::config::AppConfig;
use crate::{
    config::AppConfig,
    providers::AgentRuntime,
    tui::{
        Terminal,
        app::{AppState, ModelPickerState, Overlay, SidePanelTab, ToolCall, ToolStatus},
        layout,
        theme::Theme,
        widgets::{footer, header, input, messages, overlay, side_panel, status},
    },
};
use cersei::events::{AgentEvent, AgentStream};
// use cersei::memory::manager::MemoryManager;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::prelude::Rect;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const TICK_RATE: Duration = Duration::from_millis(16); // ~62 FPS

/// A single agent run, with enough state to transparently retry on another
/// provider when the current one errors before producing any output.
struct AgentRun {
    stream: AgentStream,
    /// The prompt being run (re-sent to the retry agent).
    prompt: String,
    /// Provider the current stream is running on.
    provider: String,
    /// Whether any output event has been emitted yet.
    produced_output: bool,
}

/// If `event` is a provider error from a run that hasn't produced output yet,
/// retry the run on the next provider in priority order. Returns true when the
/// error was handled by a fallback (and the event should be swallowed).
fn try_fallback(
    state: &mut AppState,
    runtime: &Arc<AgentRuntime>,
    run: &mut AgentRun,
    event: &AgentEvent,
) -> bool {
    let AgentEvent::Error(msg) = event else {
        // Only actual model/tool output blocks a retry — lifecycle events like
        // TurnStart fire before the provider call fails.
        run.produced_output |= matches!(
            event,
            AgentEvent::TextDelta(_)
                | AgentEvent::ThinkingDelta(_)
                | AgentEvent::ToolStart { .. }
                | AgentEvent::ToolEnd { .. }
        );
        return false;
    };
    if run.produced_output || !runtime.fallback_enabled() {
        return false;
    }
    let Some(next) = runtime.next_fallback_provider(&run.provider) else {
        return false;
    };
    runtime.record_failure(&run.provider);
    match runtime.fallback_to(&next) {
        Ok(()) => {
            state.push_system(format!(
                "{} failed ({}) — falling back to {next}",
                run.provider, msg
            ));
            run.provider = next;
            run.stream = runtime.agent().run_stream(&run.prompt);
            true
        }
        Err(_) => false,
    }
}

pub async fn run(
    terminal: &mut Terminal,
    runtime: Arc<AgentRuntime>,
    config: &AppConfig,
    // _memory_manager: &MemoryManager,
    // session_id: &str,
    cancel_token: CancellationToken,
    // shared_mode: crate::permissions::SharedPermissionMode,
    // mut permission_rx: tokio::sync::mpsc::Receiver<crate::permissions::TuiPermissionRequest>,
) -> anyhow::Result<()> {
    // let theme = Theme::from_name(&config.theme);
    let theme = Theme::enterprise();
    let mut state = AppState::new(&config.model, Some(runtime.subscribe_subagents()));
    // state.set_shared_mode(shared_mode);
    let mut agent_run: Option<AgentRun> = None;

    // Initial render
    draw(terminal, &mut state, &theme)?;

    loop {
        if state.should_quit {
            break;
        }

        tokio::select! {
            // ── Permission requests from agent ──────────────────────────
            // Some(perm_req) = permission_rx.recv() => {
            //     state.overlay = Overlay::Permission(crate::tui::app::PermissionOverlay {
            //         tool_name: perm_req.tool_name,
            //         description: perm_req.description,
            //         selected: 0,
            //     });
            //     state.pending_permission_tx = Some(perm_req.response_tx);
            //     state.dirty = true;
            // }

            // ── Agent stream events ─────────────────────────────────────
            event = poll_agent_run(&mut agent_run) => {
                match event {
                    Some(agent_event) => {
                        let fell_back = if let Some(run) = agent_run.as_mut() {
                            try_fallback(&mut state, &runtime, run, &agent_event)
                        } else {
                            false
                        };
                        if !fell_back {
                            handle_agent_event(&mut state, &runtime, agent_event);
                        }
                    }
                    None => {
                        if state.is_streaming {
                            state.commit_turn();
                            state.is_streaming = false;
                        }
                        agent_run = None;
                    }
                }
                state.dirty = true;
            }

            // ── Sub-agent activity (nested tool calls under spawn_agents) ──
            activity = poll_subagent(&mut state.subagent_rx) => {
                if let Some(activity) = activity {
                    handle_subagent_event(&mut state, activity);
                    state.dirty = true;
                }
            }

            // ── Terminal events + tick ───────────────────────────────────
            _ = tokio::time::sleep(TICK_RATE) => {
                // Drain pending events (cap at 50 to prevent infinite loop on resize storms)
                let mut event_count = 0u32;
                while event_count < 50 && event::poll(Duration::ZERO)? {
                    event_count += 1;
                    match event::read()? {
                        Event::Key(key) => {
                            if let Some(prompt) =
                                handle_key(&mut state, key, config, &cancel_token, &runtime)
                            {
                                state.push_user(&prompt);
                                state.is_streaming = true;
                                state.stream_start = Some(Instant::now());
                                state.scroll.scroll_to_bottom();
                                agent_run = Some(AgentRun {
                                    stream: runtime.agent().run_stream(&prompt),
                                    prompt: prompt.clone(),
                                    provider: runtime.current().0,
                                    produced_output: false,
                                });
                            }
                            state.dirty = true;
                        }
                        Event::Mouse(mouse) => {
                            handle_mouse(&mut state, mouse);
                        }
                        Event::Paste(text) if !state.is_streaming => {
                            state.input.insert_str(state.cursor_pos, &text);
                            state.cursor_pos += text.len();
                            state.refresh_command_selector();
                            state.dirty = true;
                        }
                        Event::Resize(_, _) => {
                            // Re-push keyboard enhancement after resize
                            // (some terminals drop the protocol on resize)
                            let _ = crossterm::execute!(
                                std::io::stdout(),
                                crossterm::event::PushKeyboardEnhancementFlags(
                                    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                                )
                            );
                            state.dirty = true;
                        }
                        _ => {}
                    }
                }
                state.frame_count += 1;
            }
        }

        if state.dirty || state.is_streaming {
            draw(terminal, &mut state, &theme)?;
            state.dirty = false;
        }
    }

    Ok(())
}

async fn poll_agent_run(run: &mut Option<AgentRun>) -> Option<AgentEvent> {
    match run {
        Some(r) => r.stream.next().await,
        None => std::future::pending().await,
    }
}

/// Next sub-agent activity event, or None once the channel is closed (the
/// runtime was dropped — there is nothing left to poll).
async fn poll_subagent(
    rx: &mut Option<tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity>>,
) -> Option<crate::subagents::SubAgentActivity> {
    let Some(receiver) = rx else {
        std::future::pending().await
    };
    match receiver.recv().await {
        Ok(activity) => Some(activity),
        // Lagged: events were dropped while the UI was busy — keep polling.
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            // The runtime was dropped — nothing left to poll.
            *rx = None;
            None
        }
    }
}

fn draw(terminal: &mut Terminal, state: &mut AppState, theme: &Theme) -> anyhow::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        // Measure the input box against its true width: with the side panel
        // open the input area is narrower than the terminal.
        let provisional = layout::compute(area, 1, state.side_panel_open);
        let input_h = input::desired_height(&state.input, provisional.main.input.width);
        let layout = layout::compute(area, input_h, state.side_panel_open);

        header::render(f, layout.main.header, state, theme);
        messages::render(f, layout.main.messages, state, theme);
        status::render(f, layout.main.status, state, theme);
        input::render(f, layout.main.input, &mut *state, theme);
        input::render_command_selector(f, layout.main.input, state, theme);
        footer::render(
            f,
            layout.main.footer,
            state.is_streaming,
            state.side_panel_open,
            state.side_panel_focused,
            theme,
        );

        // Side panel
        if let Some(panel_area) = layout.side_panel {
            side_panel::render(f, panel_area, state, theme);
        }

        // Overlay on top
        overlay::render(f, state, theme);
    })?;
    Ok(())
}

/// Handle a key event. Returns Some(prompt) if the user submitted input.
fn handle_key(
    state: &mut AppState,
    key: KeyEvent,
    config: &AppConfig,
    cancel_token: &CancellationToken,
    runtime: &Arc<AgentRuntime>,
) -> Option<String> {
    // Model picker gets full key handling while open.
    if matches!(state.overlay, Overlay::ModelPicker(_)) {
        return handle_model_picker_key(state, key, runtime);
    }

    // Fuzzy command selector gets key handling while open.
    if state.command_selector.is_some()
        && handle_command_selector_key(state, key, config, runtime)
    {
        return None;
    }

    // ── Side panel focused: j/k scroll, Tab switches tabs, Esc returns focus ──
    if state.side_panel_focused {
        match key.code {
            KeyCode::Char('j') => {
                state.side_panel_scroll.scroll_down(1);
            }
            KeyCode::Char('k') => {
                state.side_panel_scroll.scroll_up(1);
            }
            KeyCode::Char('d') => {
                state.side_panel_scroll.page_down();
            }
            KeyCode::Char('u') => {
                state.side_panel_scroll.page_up();
            }
            KeyCode::Char('g') => {
                state
                    .side_panel_scroll
                    .scroll_up(state.side_panel_scroll.content_height);
            }
            KeyCode::Char('G') => {
                state.side_panel_scroll.scroll_to_bottom();
            }
            KeyCode::Tab => {
                state.side_panel_tab = state.side_panel_tab.toggle();
            }
            KeyCode::Char('r') => {
                side_panel::refresh_content(state, &config.working_dir);
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                state.side_panel_focused = false;
            }
            _ => {
                // Global keys still work
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('b')) => {
                        state.side_panel_open = false;
                        state.side_panel_focused = false;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
                        state.should_quit = true;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) if state.is_streaming => {
                        cancel_token.cancel();
                        state.is_streaming = false;
                        state.commit_turn();
                    }
                    _ => {}
                }
            }
        }
        return None;
    }

    match (key.modifiers, key.code) {
        // Ctrl+D — exit
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            state.should_quit = true;
        }

        // Ctrl+C — cancel or quit
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if state.is_streaming {
                cancel_token.cancel();
                state.is_streaming = false;
                state.commit_turn();
            } else if state.input.is_empty() {
                state.should_quit = true;
            } else {
                state.input.clear();
                state.cursor_pos = 0;
                state.refresh_command_selector();
            }
        }

        // Ctrl+B — toggle side panel (and focus it)
        (KeyModifiers::CONTROL, KeyCode::Char('b')) => {
            if state.side_panel_open && !state.side_panel_focused {
                // Panel open but not focused — focus it
                state.side_panel_focused = true;
            } else if state.side_panel_focused {
                // Focused — close panel
                state.side_panel_open = false;
                state.side_panel_focused = false;
            } else {
                // Closed — open and focus
                state.side_panel_open = true;
                state.side_panel_focused = true;
                side_panel::refresh_content(state, &config.working_dir);
            }
        }

        // // Shift+Tab — cycle permission mode
        // (KeyModifiers::SHIFT, KeyCode::BackTab) => {
        //     state.cycle_permission_mode();
        // }

        // Tab — switch side panel tabs (when panel open but not focused)
        (_, KeyCode::Tab) if state.side_panel_open => {
            state.side_panel_tab = state.side_panel_tab.toggle();
        }

        // Scroll messages
        (_, KeyCode::PageUp) => {
            state.scroll.page_up();
        }
        (_, KeyCode::PageDown) => {
            state.scroll.page_down();
        }
        (_, KeyCode::Home) => {
            state.scroll.scroll_up(state.scroll.content_height);
        }
        (_, KeyCode::End) => {
            state.scroll.scroll_to_bottom();
        }

        // Alt+Enter / Ctrl+J / Shift+Enter — insert newline
        (KeyModifiers::ALT, KeyCode::Enter) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
            state.refresh_command_selector();
        }
        (KeyModifiers::SHIFT, KeyCode::Enter) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
            state.refresh_command_selector();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('j')) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
            state.refresh_command_selector();
        }

        // Enter — submit input
        (_, KeyCode::Enter) if !state.is_streaming => {
            let input_text = state.input.trim().to_string();
            if input_text.is_empty() {
                return None;
            }

            state.input.clear();
            state.cursor_pos = 0;
            state.command_selector = None;

            if input_text.starts_with('/') {
                handle_slash_command(state, &input_text, config, runtime);
                return None;
            }

            state.input_history.push(input_text.clone());
            state.history_index = None;
            return Some(input_text);
        }

        // Backspace — remove the char before the cursor
        (_, KeyCode::Backspace) if !state.is_streaming && state.cursor_pos > 0 => {
            if let Some((char_start, _)) =
                state.input[..state.cursor_pos].char_indices().next_back()
            {
                state.input.remove(char_start);
                state.cursor_pos = char_start;
            }
            state.refresh_command_selector();
        }

        // Delete — remove the char at the cursor
        (_, KeyCode::Delete) if !state.is_streaming && state.cursor_pos < state.input.len() => {
            state.input.remove(state.cursor_pos);
            state.refresh_command_selector();
        }

        // Left arrow — move back one char
        (_, KeyCode::Left) if !state.is_streaming && state.cursor_pos > 0 => {
            if let Some((char_start, _)) =
                state.input[..state.cursor_pos].char_indices().next_back()
            {
                state.cursor_pos = char_start;
            }
            state.refresh_command_selector();
        }

        // Right arrow — move forward one char
        (_, KeyCode::Right) if !state.is_streaming && state.cursor_pos < state.input.len() => {
            if let Some(ch) = state.input[state.cursor_pos..].chars().next() {
                state.cursor_pos += ch.len_utf8();
            }
            state.refresh_command_selector();
        }

        // Up arrow — scroll if input empty, else history
        (_, KeyCode::Up) if !state.is_streaming => {
            if state.input.is_empty() && state.history_index.is_none() {
                state.scroll.scroll_up(1);
            } else if !state.input_history.is_empty() {
                let idx = state
                    .history_index
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(state.input_history.len() - 1);
                state.history_index = Some(idx);
                state.input = state.input_history[idx].clone();
                state.cursor_pos = state.input.len();
                state.refresh_command_selector();
            }
        }
        (_, KeyCode::Up) if state.is_streaming => {
            state.scroll.scroll_up(1);
        }

        // Down arrow — scroll if input empty, else history
        (_, KeyCode::Down) if !state.is_streaming => {
            if let Some(idx) = state.history_index {
                if idx + 1 < state.input_history.len() {
                    let new_idx = idx + 1;
                    state.history_index = Some(new_idx);
                    state.input = state.input_history[new_idx].clone();
                    state.cursor_pos = state.input.len();
                } else {
                    state.history_index = None;
                    state.input.clear();
                    state.cursor_pos = 0;
                }
                state.refresh_command_selector();
            } else if state.input.is_empty() {
                state.scroll.scroll_down(1);
            }
        }
        (_, KeyCode::Down) if state.is_streaming => {
            state.scroll.scroll_down(1);
        }

        // Esc
        (_, KeyCode::Esc) if state.overlay != Overlay::None => {
            state.overlay = Overlay::None;
        }

        // Character input
        (_, KeyCode::Char(c)) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += c.len_utf8();
            state.refresh_command_selector();
        }

        _ => {}
    }

    None
}

/// Handle mouse events: a left click inside the input box moves the cursor to
/// the clicked character.
fn handle_mouse(state: &mut AppState, mouse: MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    if state.is_streaming || state.overlay != Overlay::None {
        return;
    }
    let Some((x, y, w, h)) = state.input_area else {
        return;
    };
    if w < 4 || h < 2 {
        return;
    }
    // The input is drawn inside a 1-cell rounded border.
    let inner = Rect {
        x: x + 1,
        y: y + 1,
        width: w.saturating_sub(2),
        height: h.saturating_sub(2),
    };
    let (row, col) = (mouse.row, mouse.column);
    if row < inner.y || row >= inner.bottom() || col < inner.x || col >= inner.right() {
        return;
    }
    let prompt = if state.is_streaming { "  " } else { "> " };
    let usable = (inner.width as usize).saturating_sub(prompt.len());
    let content_row = (row - inner.y) as usize + state.input_scroll as usize;
    let content_col = (col - inner.x) as usize;
    let pos = input::char_pos_at_click(&state.input, prompt, usable, content_row, content_col);
    if pos != state.cursor_pos {
        state.cursor_pos = pos;
        state.refresh_command_selector();
        state.dirty = true;
    }
}

/// Handle keys while the fuzzy `/` command selector is open.
fn handle_command_selector_key(
    state: &mut AppState,
    key: KeyEvent,
    config: &AppConfig,
    runtime: &Arc<AgentRuntime>,
) -> bool {
    let Some(sel) = &mut state.command_selector else {
        return false;
    };
    let len = sel.matches.len();
    if len == 0 {
        state.command_selector = None;
        return false;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            sel.selected = sel.selected.saturating_sub(1);
            state.dirty = true;
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if sel.selected + 1 < len {
                sel.selected += 1;
                state.dirty = true;
            }
            true
        }
        KeyCode::Enter | KeyCode::Tab => {
            // Run the selected command as if typed.
            let name = sel.matches[sel.selected].name.clone();
            let input_text = format!("/{name}");
            state.command_selector = None;
            state.input.clear();
            state.cursor_pos = 0;
            handle_slash_command(state, &input_text, config, runtime);
            state.dirty = true;
            true
        }
        KeyCode::Esc => {
            state.command_selector = None;
            state.dirty = true;
            true
        }
        _ => false,
    }
}

/// Handle keys while the provider/model picker is open.
fn handle_model_picker_key(
    state: &mut AppState,
    key: KeyEvent,
    runtime: &Arc<AgentRuntime>,
) -> Option<String> {
    let Overlay::ModelPicker(picker) = &mut state.overlay else {
        return None;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            picker.selected = picker.selected.saturating_sub(1);
            state.dirty = true;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if picker.selected + 1 < picker.entries.len() {
                picker.selected += 1;
            }
            state.dirty = true;
        }
        KeyCode::Enter => {
            if picker.entries.is_empty() {
                state.overlay = Overlay::None;
                state.dirty = true;
                return None;
            }
            let idx = picker.selected.min(picker.entries.len() - 1);
            let (provider, model) = picker.entries[idx].clone();
            let label = crate::providers::display_model_id(&provider, &model);
            state.overlay = Overlay::None;
            match runtime.switch(&provider, &model) {
                Ok(()) => {
                    state.model = model;
                    state.push_system(format!("Switched to {label}"));
                }
                Err(e) => state.push_system(format!("Failed to switch to {label}: {e}")),
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            state.overlay = Overlay::None;
            state.dirty = true;
        }
        _ => {}
    }
    None
}

fn handle_agent_event(state: &mut AppState, runtime: &Arc<AgentRuntime>, event: AgentEvent) {
    match event {
        AgentEvent::TextDelta(text) => {
            state.streaming_text.push_str(&text);
        }
        AgentEvent::ThinkingDelta(text) => {
            state.streaming_thinking.push_str(&text);
        }
        AgentEvent::ToolStart {
            name, id: _, input, ..
        } => {
            let summary = tool_input_summary(&name, &input);
            state.active_tools.push(ToolCall {
                name,
                input_summary: summary,
                status: ToolStatus::Running,
                output_preview: None,
                started_at: Instant::now(),
                duration_ms: None,
                children: Vec::new(),
                run_id: None,
            });
            state.tool_count += 1;
            // A `spawn_agents` call may have started before the UI processed
            // its ToolStart — attach any sub-agent activity that arrived in
            // the meantime.
            if state.active_tools.last().is_some_and(|t| t.name == "spawn_agents") {
                drain_pending_subagents(state);
            }
        }
        AgentEvent::ToolEnd {
            name,
            id: _,
            result,
            is_error,
            duration,
        } => {
            if let Some(tool) = state.active_tools.iter_mut().rev().find(|t| t.name == name) {
                tool.status = if is_error {
                    ToolStatus::Error
                } else {
                    ToolStatus::Done
                };
                tool.duration_ms = Some(duration.as_millis() as u64);
                tool.output_preview = Some(result.chars().take(200).collect());
            }
        }
        AgentEvent::CostUpdate {
            cumulative_cost,
            input_tokens,
            output_tokens,
            ..
        } => {
            state.input_tokens = input_tokens;
            state.output_tokens = output_tokens;
            // Use reported cost or estimate from model pricing
            state.cost_usd = if cumulative_cost > 0.0 {
                cumulative_cost
            } else {
                cersei::tools::estimate_cost(&state.model, input_tokens, output_tokens)
            };
        }
        AgentEvent::TurnComplete { usage, .. } => {
            state.input_tokens = usage.input_tokens;
            state.output_tokens = usage.output_tokens;
            state.cost_usd = usage.cost_usd.filter(|c| *c > 0.0).unwrap_or_else(|| {
                cersei::tools::estimate_cost(&state.model, usage.input_tokens, usage.output_tokens)
            });
        }
        AgentEvent::TokenWarning { pct_used, .. } => {
            state.context_pct = pct_used;
        }
        AgentEvent::Error(msg) => {
            state.commit_turn();
            state.is_streaming = false;
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Error: {msg}"),
                tools: Vec::new(),
                thinking: None,
            });
        }
        AgentEvent::Complete(_) => {
            state.commit_turn();
            state.is_streaming = false;
            // Render the followup suggestions the agent proposed via the
            // suggest_followups tool.
            let followups = runtime.take_followups();
            if !followups.is_empty() {
                let body = followups
                    .iter()
                    .map(|f| format!("• {} — {}", f.label, f.prompt))
                    .collect::<Vec<_>>()
                    .join("\n");
                state.push_system(format!("Suggested next steps:\n{body}"));
            }
        }
        _ => {}
    }
}

/// Whether `tool` (a `spawn_agents` call) owns the given sub-agent run,
/// directly (its own binding) or via one of its nested sub-agent headers.
fn tool_owns_run(tool: &ToolCall, run_id: u64) -> bool {
    tool.run_id == Some(run_id)
        || tool.children.iter().any(|c| c.run_id == Some(run_id))
}

/// Where the `spawn_agents` call owning a sub-agent run lives.
#[derive(Clone, Copy)]
enum SpawnParentLoc {
    /// In the current turn's active tool calls.
    Active(usize),
    /// In the last committed turn.
    Committed(usize),
}

/// Find the `spawn_agents` call that owns `run_id`: first any call already
/// bound to that run (active or committed — late-arriving events land here),
/// then the currently-running spawn call to bind to. Tools run sequentially,
/// so at most one spawn call is in flight at a time.
fn find_spawn_parent(state: &AppState, run_id: u64) -> Option<SpawnParentLoc> {
    if let Some(idx) = state
        .active_tools
        .iter()
        .rposition(|t| t.name == "spawn_agents" && tool_owns_run(t, run_id))
    {
        return Some(SpawnParentLoc::Active(idx));
    }
    if let Some(turn) = state.turns.last() {
        if let Some(idx) = turn
            .tools
            .iter()
            .rposition(|t| t.name == "spawn_agents" && tool_owns_run(t, run_id))
        {
            return Some(SpawnParentLoc::Committed(idx));
        }
    }
    state
        .active_tools
        .iter()
        .rposition(|t| t.name == "spawn_agents" && t.status == ToolStatus::Running)
        .map(SpawnParentLoc::Active)
}

/// Attach one forwarded sub-agent activity event to its `spawn_agents` parent
/// call. The parent may be in the active tools or the last committed turn
/// (events can arrive after the turn completed).
fn handle_subagent_event(
    state: &mut AppState,
    activity: crate::subagents::SubAgentActivity,
) {
    let run_id = activity.run_id();
    let Some(loc) = find_spawn_parent(state, run_id) else {
        // The parent ToolStart hasn't been processed yet — buffer until it is
        // (drained when the `spawn_agents` ToolStart is handled).
        state.pending_subagent.push((run_id, activity));
        return;
    };
    let parent = match loc {
        SpawnParentLoc::Active(idx) => &mut state.active_tools[idx],
        SpawnParentLoc::Committed(idx) => &mut state.turns.last_mut().expect("checked").tools[idx],
    };
    if parent.run_id.is_none() {
        parent.run_id = Some(run_id);
    }
    attach_subagent_activity(parent, run_id, activity);
}

/// Attach sub-agent activity that arrived before its `spawn_agents` call was
/// created to the just-pushed call. Tools run sequentially, so all buffered
/// events belong to it.
fn drain_pending_subagents(state: &mut AppState) {
    let pending = std::mem::take(&mut state.pending_subagent);
    for (run_id, activity) in pending {
        let parent = state.active_tools.last_mut().expect("call was just pushed");
        if parent.run_id.is_none() {
            parent.run_id = Some(run_id);
        }
        attach_subagent_activity(parent, run_id, activity);
    }
}

/// Apply one activity event to the nested children of a `spawn_agents` call.
fn attach_subagent_activity(
    parent: &mut ToolCall,
    run_id: u64,
    activity: crate::subagents::SubAgentActivity,
) {
    use crate::subagents::SubAgentActivity;
    match activity {
        // Started: the nested header for this sub-agent.
        SubAgentActivity::Started {
            agent_type,
            display_name,
            prompt,
            ..
        } => {
            parent.children.push(ToolCall {
                name: format!("[{agent_type}]"),
                input_summary: format!("{} — {}", display_name, truncate(&prompt, 50)),
                status: ToolStatus::Running,
                output_preview: None,
                started_at: Instant::now(),
                duration_ms: None,
                children: Vec::new(),
                run_id: Some(run_id),
            });
        }
        // ToolStart: a tool call inside this sub-agent.
        SubAgentActivity::ToolStart {
            name, input_summary, ..
        } => {
            if let Some(header) = parent
                .children
                .iter_mut()
                .rev()
                .find(|c| c.run_id == Some(run_id))
            {
                header.children.push(ToolCall {
                    name,
                    input_summary,
                    status: ToolStatus::Running,
                    output_preview: None,
                    started_at: Instant::now(),
                    duration_ms: None,
                    children: Vec::new(),
                    run_id: None,
                });
            }
        }
        // ToolEnd: mark that tool call done.
        SubAgentActivity::ToolEnd {
            name,
            is_error,
            output_preview,
            duration_ms,
            ..
        } => {
            if let Some(header) = parent
                .children
                .iter_mut()
                .rev()
                .find(|c| c.run_id == Some(run_id))
            {
                if let Some(tool) = header
                    .children
                    .iter_mut()
                    .rev()
                    .find(|t| t.name == name && t.status == ToolStatus::Running)
                {
                    tool.status = if is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    tool.duration_ms = Some(duration_ms);
                    tool.output_preview = Some(output_preview);
                }
            }
        }
        // Finished: the sub-agent's final output; the header is done.
        SubAgentActivity::Finished { text, .. } => {
            if let Some(header) = parent
                .children
                .iter_mut()
                .rev()
                .find(|c| c.run_id == Some(run_id))
            {
                header.status = ToolStatus::Done;
                header.duration_ms = Some(header.started_at.elapsed().as_millis() as u64);
                header.output_preview = Some(text);
            }
        }
    }
}

fn handle_slash_command(
    state: &mut AppState,
    input: &str,
    config: &AppConfig,
    runtime: &Arc<AgentRuntime>,
) {
    let cmd = input
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    match cmd {
        "help" | "h" | "?" => {
            state.overlay = Overlay::Help;
        }
        "clear" => {
            state.turns.clear();
            state.streaming_text.clear();
            state.streaming_thinking.clear();
            state.active_tools.clear();
        }
        "exit" | "quit" | "q" => {
            state.should_quit = true;
        }
        "panel" => {
            state.side_panel_open = !state.side_panel_open;
            if state.side_panel_open {
                side_panel::refresh_content(state, &config.working_dir);
            }
        }
        // "graph" => {
        //     use crate::tui::widgets::graph::{GraphOverlayState, MemoryGraphData, MemoryNode};
        //     // Build graph data from available info
        //     let mut data = MemoryGraphData::default();
        //     // Add LSP servers as nodes
        //     for server in cersei::lsp::config::builtin_servers() {
        //         if which::which(&server.command).is_ok() {
        //             data.lsp_servers.push(server.name.clone());
        //         }
        //     }
        //     let graph_state = GraphOverlayState::from_memory_stats(&data);
        //     state.overlay = Overlay::Graph(graph_state);
        // }
        "diff" => {
            state.side_panel_open = true;
            state.side_panel_focused = true;
            state.side_panel_tab = SidePanelTab::GitDiff;
            side_panel::refresh_content(state, &config.working_dir);
        }
        "files" | "tree" => {
            state.side_panel_open = true;
            state.side_panel_focused = true;
            state.side_panel_tab = SidePanelTab::FileTree;
            side_panel::refresh_content(state, &config.working_dir);
        }
        "rewind" => {
            // Remove the last assistant turn (rewind one step)
            if let Some(pos) = state
                .turns
                .iter()
                .rposition(|t| t.role == crate::tui::app::TurnRole::Assistant)
            {
                let removed = state.turns.len() - pos;
                state.turns.truncate(pos);
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: format!(
                        "Rewound {removed} turn(s). You can now re-send your last message."
                    ),
                    tools: Vec::new(),
                    thinking: None,
                });
            } else {
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: "Nothing to rewind.".into(),
                    tools: Vec::new(),
                    thinking: None,
                });
            }
        }
        // "undo" => {
        //     // Undo last file modification via snapshot manager
        //     let snapshots = cersei_tools::file_snapshot::session_snapshots(&state.session_id);
        //     let mut mgr = snapshots.lock();
        //     let files = mgr.modified_files();
        //     if let Some(last_file) = files.last() {
        //         let path = last_file.display().to_string();
        //         if mgr.undo_last(std::path::Path::new(&path)).is_some() {
        //             state.turns.push(crate::tui::app::Turn {
        //                 role: crate::tui::app::TurnRole::System,
        //                 content: format!("Undid last change to {path}"),
        //                 tools: Vec::new(),
        //                 thinking: None,
        //             });
        //         } else {
        //             state.turns.push(crate::tui::app::Turn {
        //                 role: crate::tui::app::TurnRole::System,
        //                 content: "Failed to undo.".into(),
        //                 tools: Vec::new(),
        //                 thinking: None,
        //             });
        //         }
        //     } else {
        //         state.turns.push(crate::tui::app::Turn {
        //             role: crate::tui::app::TurnRole::System,
        //             content: "No file changes to undo.".into(),
        //             tools: Vec::new(),
        //             thinking: None,
        //         });
        //     }
        // }
        "memory" | "mem" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: "Memory is injected into the system prompt automatically.\nUse AGENTS.md or .abstract/instructions.md in your project for persistent instructions.".into(),
                tools: Vec::new(),
                thinking: None,
            });
        }
        // "sessions" | "session" | "ls" => {
        //     state.turns.push(crate::tui::app::Turn {
        //         role: crate::tui::app::TurnRole::System,
        //         content: format!("Current session: {}\nUse `abstract sessions list` from the terminal to manage sessions.", state.session_id),
        //         tools: Vec::new(),
        //         thinking: None,
        //     });
        // }
        "model" => {
            let rest = input
                .trim_start_matches('/')
                .strip_prefix("model")
                .map(str::trim)
                .unwrap_or("");
            if rest.is_empty() {
                // Open the picker.
                let (provider, model) = runtime.current();
                let current = crate::providers::display_model_id(&provider, &model);
                let entries = runtime.entries();
                let selected = entries
                    .iter()
                    .position(|(p, m)| crate::providers::display_model_id(p, m) == current)
                    .unwrap_or(0);
                state.overlay = Overlay::ModelPicker(ModelPickerState {
                    entries,
                    selected,
                    current,
                });
            } else {
                match runtime.select_text(rest) {
                    Ok((provider, model)) => {
                        let label = crate::providers::display_model_id(&provider, &model);
                        match runtime.switch(&provider, &model) {
                            Ok(()) => {
                                state.model = model;
                                state.push_system(format!("Switched to {label}"));
                            }
                            Err(e) => state.push_system(format!("Failed to switch to {label}: {e}")),
                        }
                    }
                    Err(e) => state.push_system(format!("{e}")),
                }
            }
        }
        // "cost" => {
        //     // Estimate cost if provider didn't report it
        //     let cost = if state.cost_usd > 0.0 {
        //         state.cost_usd
        //     } else {
        //         cersei::tools::estimate_cost(&state.model, state.input_tokens, state.output_tokens)
        //     };
        //     state.turns.push(crate::tui::app::Turn {
        //         role: crate::tui::app::TurnRole::System,
        //         content: format!(
        //             "Session cost: ${:.4}\nInput: {} tokens | Output: {} tokens\nTurns: {} | Tools: {}",
        //             cost, state.input_tokens, state.output_tokens,
        //             state.turn_count, state.tool_count
        //         ),
        //         tools: Vec::new(),
        //         thinking: None,
        //     });
        // }
        "compact" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: "Compaction will run automatically at 90% context usage.".into(),
                tools: Vec::new(),
                thinking: None,
            });
        }
        "proxy" => {
            let proxy_url = &config.proxy.url;
            let is_via_proxy = state.model.contains("via proxy");

            // Check for authenticated accounts
            let mut accounts = Vec::new();
            if let Some(home) = dirs::home_dir() {
                let auth_dir = home.join(".cli-proxy-api");
                if auth_dir.exists()
                    && let Ok(entries) = std::fs::read_dir(&auth_dir)
                {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".json")
                            && let Ok(content) = std::fs::read_to_string(entry.path())
                            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                        {
                            let provider = json["type"].as_str().unwrap_or("?");
                            let email = json["email"].as_str().unwrap_or("?");
                            let expired = json["expired"].as_str().unwrap_or("?");
                            accounts
                                .push(format!("  {} ({}) — expires {}", provider, email, expired));
                        }
                    }
                }
            }

            let status = if is_via_proxy { "active" } else { "inactive" };
            let accounts_str = if accounts.is_empty() {
                "  No accounts found in ~/.cli-proxy-api/".to_string()
            } else {
                accounts.join("\n")
            };

            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!(
                    "Proxy: {status}\nURL: {proxy_url}\nAccounts:\n{accounts_str}\n\nConfigure in .abstract/config.toml:\n[proxy]\nenabled = true\nurl = \"http://localhost:8317/v1\""
                ),
                tools: Vec::new(),
                thinking: None,
            });
        }
        _ => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Unknown command: /{cmd}. Type /help for commands."),
                tools: Vec::new(),
                thinking: None,
            });
        }
    }
}

/// A short summary of a tool call's input for the tool badge line. Shared with
/// the sub-agent event forwarder in `subagents.rs`.
pub(crate) fn tool_input_summary(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" | "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 60))
            .unwrap_or_default(),
        "Read" | "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 40))
            .unwrap_or_default(),
        "LSP" => {
            let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("?");
            let file = input.get("file").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{action} {file}")
        }
        "spawn_agents" => match input.get("agents").and_then(|v| v.as_array()) {
            Some(list) => {
                let types: Vec<&str> = list
                    .iter()
                    .filter_map(|a| a.get("agent_type").and_then(|t| t.as_str()))
                    .collect();
                format!("[{}] ({} agent{})", types.join(", "), list.len(), if list.len() == 1 { "" } else { "s" })
            }
            None => truncate(&serde_json::to_string(input).unwrap_or_default(), 60),
        },
        _ => truncate(&serde_json::to_string(input).unwrap_or_default(), 60),
    }
}

/// Truncate `s` to at most `max` bytes on a UTF-8 char boundary (never
/// panics on multi-byte input).
fn truncate(s: &str, max: usize) -> String {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if s.len() <= end {
        s.to_string()
    } else {
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::SubAgentActivity;

    fn state() -> AppState {
        AppState::new("test-model", None)
    }

    fn spawn_call(status: ToolStatus, run_id: Option<u64>) -> ToolCall {
        ToolCall {
            name: "spawn_agents".into(),
            input_summary: "[researcher-web] (1 agent)".into(),
            status,
            output_preview: None,
            started_at: Instant::now(),
            duration_ms: None,
            children: Vec::new(),
            run_id,
        }
    }

    #[test]
    fn subagent_events_buffer_until_parent_tool_start() {
        let mut s = state();
        handle_subagent_event(
            &mut s,
            SubAgentActivity::Started {
                run_id: 1,
                agent_type: "code-reviewer".into(),
                display_name: "Nit Pick Nick".into(),
                prompt: "review the changes".into(),
            },
        );
        // No spawn_agents call yet: the event is buffered, not dropped.
        assert_eq!(s.pending_subagent.len(), 1);
        assert!(s.active_tools.is_empty());

        // The parent ToolStart arrives (as handle_agent_event pushes it) and
        // drains the buffer.
        s.active_tools.push(spawn_call(ToolStatus::Running, None));
        drain_pending_subagents(&mut s);
        assert!(s.pending_subagent.is_empty());

        let parent = &s.active_tools[0];
        assert_eq!(parent.run_id, Some(1));
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].name, "[code-reviewer]");
        assert_eq!(parent.children[0].status, ToolStatus::Running);
    }

    #[test]
    fn subagent_events_nest_under_running_parent() {
        let mut s = state();
        s.active_tools.push(spawn_call(ToolStatus::Running, None));

        let run_id = 7;
        handle_subagent_event(
            &mut s,
            SubAgentActivity::Started {
                run_id,
                agent_type: "researcher-web".into(),
                display_name: "Web Researcher".into(),
                prompt: "find current info".into(),
            },
        );
        handle_subagent_event(
            &mut s,
            SubAgentActivity::ToolStart {
                run_id,
                name: "WebSearch".into(),
                input_summary: "\"rust async\"".into(),
            },
        );
        handle_subagent_event(
            &mut s,
            SubAgentActivity::ToolEnd {
                run_id,
                name: "WebSearch".into(),
                is_error: false,
                output_preview: "3 results".into(),
                duration_ms: 210,
            },
        );
        handle_subagent_event(
            &mut s,
            SubAgentActivity::Finished {
                run_id,
                text: "found it".into(),
            },
        );

        let parent = &s.active_tools[0];
        assert_eq!(parent.run_id, Some(run_id));
        assert_eq!(parent.children.len(), 1);
        let header = &parent.children[0];
        assert_eq!(header.name, "[researcher-web]");
        assert_eq!(header.status, ToolStatus::Done);
        assert_eq!(header.output_preview.as_deref(), Some("found it"));
        assert_eq!(header.children.len(), 1);
        assert_eq!(header.children[0].name, "WebSearch");
        assert_eq!(header.children[0].status, ToolStatus::Done);
        assert_eq!(header.children[0].duration_ms, Some(210));
        assert_eq!(header.children[0].output_preview.as_deref(), Some("3 results"));
    }

    #[test]
    fn multiple_subagents_in_one_spawn_call() {
        let mut s = state();
        s.active_tools.push(spawn_call(ToolStatus::Running, None));

        for (run_id, agent_type) in [(1u64, "researcher-web"), (2, "code-searcher")] {
            handle_subagent_event(
                &mut s,
                SubAgentActivity::Started {
                    run_id,
                    agent_type: agent_type.into(),
                    display_name: agent_type.into(),
                    prompt: "go".into(),
                },
            );
            handle_subagent_event(
                &mut s,
                SubAgentActivity::Finished {
                    run_id,
                    text: format!("{agent_type} done"),
                },
            );
        }

        let parent = &s.active_tools[0];
        assert_eq!(parent.children.len(), 2);
        assert_eq!(parent.children[0].name, "[researcher-web]");
        assert_eq!(parent.children[1].name, "[code-searcher]");
        // Each header got its own run id, so both are Done.
        assert!(parent.children.iter().all(|c| c.status == ToolStatus::Done));
    }

    #[test]
    fn late_events_attach_to_committed_turn() {
        let mut s = state();
        s.active_tools.push(spawn_call(ToolStatus::Running, None));
        let run_id = 3;
        handle_subagent_event(
            &mut s,
            SubAgentActivity::Started {
                run_id,
                agent_type: "researcher-web".into(),
                display_name: "Web Researcher".into(),
                prompt: "go".into(),
            },
        );
        handle_subagent_event(
            &mut s,
            SubAgentActivity::ToolStart {
                run_id,
                name: "WebSearch".into(),
                input_summary: "x".into(),
            },
        );
        // The whole turn completes and commits before the ToolEnd arrives
        // (the UI processed the parent stream ahead of the activity channel).
        s.commit_turn();
        handle_subagent_event(
            &mut s,
            SubAgentActivity::ToolEnd {
                run_id,
                name: "WebSearch".into(),
                is_error: false,
                output_preview: "done".into(),
                duration_ms: 50,
            },
        );

        assert!(s.active_tools.is_empty());
        assert!(s.pending_subagent.is_empty());
        let turn = s.turns.last().expect("committed");
        let parent = &turn.tools[0];
        assert_eq!(parent.children[0].name, "[researcher-web]");
        assert_eq!(parent.children[0].children[0].status, ToolStatus::Done);
    }

    #[test]
    fn spawn_agents_input_summary_lists_agents() {
        let input = serde_json::json!({
            "agents": [
                { "agent_type": "researcher-web", "prompt": "a" },
                { "agent_type": "code-reviewer", "prompt": "b" }
            ]
        });
        assert_eq!(
            tool_input_summary("spawn_agents", &input),
            "[researcher-web, code-reviewer] (2 agents)"
        );
        let single = serde_json::json!({"agents": [{ "agent_type": "code-searcher" }]});
        assert_eq!(tool_input_summary("spawn_agents", &single), "[code-searcher] (1 agent)");
    }

    #[test]
    fn truncate_never_panics_on_multibyte() {
        // Multi-byte chars: the cut lands on a char boundary.
        assert_eq!(truncate("héllo", 2), "h..."); // byte 2 is inside 'é'
        assert_eq!(truncate("héllo", 3), "hé..."); // 3 bytes = h + é
        assert_eq!(truncate("héllo", 4), "hél...");
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("", 5), "");
    }
}
