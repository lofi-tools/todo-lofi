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
    let mut state = AppState::new(
        &config.model,
        // session_id,
        // &config.effort
    );
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
            });
            state.tool_count += 1;
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

fn tool_input_summary(name: &str, input: &serde_json::Value) -> String {
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
        _ => truncate(&serde_json::to_string(input).unwrap_or_default(), 60),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
