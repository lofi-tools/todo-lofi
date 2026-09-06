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
        app::{AppState, ComboPickerState, ModelPickerState, Overlay, ProviderExplorerPhase, ProviderExplorerState, Selection, SelectionPoint, SelectionTarget, SidePanelTab, ToolCall, ToolStatus},
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
use unicode_width::UnicodeWidthChar;

const TICK_RATE: Duration = Duration::from_millis(16); // ~62 FPS

/// A single agent run, with enough state to transparently retry on another
/// combo entry when the current one errors before producing any output.
struct AgentRun {
    stream: AgentStream,
    /// The prompt being run (re-sent to the retry agent).
    prompt: String,
    /// Concrete (provider, model) the current stream is running on.
    provider: String,
    model: String,
    /// Whether any output event has been emitted yet.
    produced_output: bool,
}

/// If `event` is a provider error from a run that hasn't produced output yet,
/// retry the run on the next combo entry in the list. Returns true when the
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
    let Some(next) = runtime.next_fallback_entry(&run.provider, &run.model) else {
        return false;
    };
    runtime.record_failure(&run.provider, &run.model);
    match runtime.fallback_to(&next.provider, &next.model) {
        Ok(()) => {
            state.push_system(format!(
                "{} failed ({}) — falling back to {}",
                crate::providers::display_model_id(&run.provider, &run.model),
                msg,
                crate::providers::display_model_id(&next.provider, &next.model),
            ));
            state.effective_model = Some(crate::providers::display_model_id(
                &next.provider,
                &next.model,
            ));
            run.provider = next.provider;
            run.model = next.model;
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
    let (init_provider, init_model) = runtime.current();
    let mut state = AppState::new(
        &crate::providers::display_model_id(&init_provider, &init_model),
        Some(runtime.subscribe_subagents()),
    );
    let (eff_provider, eff_model) = runtime.effective();
    state.effective_model = Some(crate::providers::display_model_id(&eff_provider, &eff_model));
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
                // Drain pending ask_user requests from the runtime.
                runtime.drain_ask_user(&mut state.ask_user_buffer);
                while let Some(req) = state.ask_user_buffer.pop() {
                    let summaries: Vec<String> = req
                        .questions
                        .iter()
                        .map(|q| q["question"].as_str().unwrap_or("?").to_string())
                        .collect();
                    let answers: Vec<String> = (0..req.questions.len()).map(|_| String::new()).collect();
                    state.overlay = Overlay::AskUser(crate::tui::app::AskUserPending {
                        request_id: req.request_id,
                        questions: req.questions,
                        question_summaries: summaries,
                        answers,
                        focused_question: 0,
                        current_input: String::new(),
                        cursor_pos: 0,
                    });
                    state.dirty = true;
                }
                // Drain a completed `/provider` fetch (if one arrived).
                poll_provider_fetch(&mut state);
                // Drain pending events (cap at 50 to prevent infinite loop on resize storms)
                let mut event_count = 0u32;
                while event_count < 50 && event::poll(Duration::ZERO)? {
                    event_count += 1;
                    match event::read()? {
                        Event::Key(key) => {
                            if let Some(prompt) =
                                handle_key(&mut state, key, config, &cancel_token, &runtime)
                            {
                                state.is_streaming = true;
                                state.stream_start = Some(Instant::now());
                                state.scroll.scroll_to_bottom();
                                let (effective_provider, effective_model) = runtime.effective();
                                agent_run = Some(AgentRun {
                                    stream: runtime.agent().run_stream(&prompt),
                                    prompt: prompt.clone(),
                                    provider: effective_provider,
                                    model: effective_model,
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
                            state.selection = None;
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
    // Copy the active selection (Cmd+C on macOS, Ctrl+Shift+C elsewhere). This
    // works in every mode, including while the side panel is focused.
    if is_copy_shortcut(key.modifiers, key.code) {
        copy_selection(state);
        return None;
    }

    // Model picker gets full key handling while open.
    if matches!(state.overlay, Overlay::ModelPicker(_)) {
        return handle_model_picker_key(state, key, runtime);
    }
    // Combo picker gets full key handling while open.
    if matches!(state.overlay, Overlay::ComboPicker(_)) {
        return handle_combo_picker_key(state, key, runtime);
    }
    if matches!(state.overlay, Overlay::ProviderExplorer(_)) {
        return handle_provider_explorer_key(state, key, runtime);
    }
    // AskUser overlay gets full key handling while open.
    if matches!(state.overlay, Overlay::AskUser(_)) {
        return handle_ask_user_key(state, key, runtime);
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
            state.selection = None;
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

        // Select all — macOS Cmd+A with the cursor in the input box selects
        // the input text only (text-field convention), even when the input is
        // empty (a no-op selection). The output document is only selected when
        // the input is read-only (streaming), since the cursor isn't there.
        (KeyModifiers::SUPER, KeyCode::Char('a')) => {
            if state.is_streaming {
                select_all_output(state);
            } else {
                select_all_input(state);
            }
            state.dirty = true;
        }
        // Ctrl+A — readline start-of-line in the input; while streaming the
        // input is read-only so it selects the output document instead.
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            if state.is_streaming {
                select_all_output(state);
            } else {
                state.cursor_pos = line_start(&state.input, state.cursor_pos);
                state.selection = None;
                state.refresh_command_selector();
            }
            state.dirty = true;
        }
        // Ctrl+E — readline end-of-line in the input
        (KeyModifiers::CONTROL, KeyCode::Char('e')) if !state.is_streaming => {
            state.cursor_pos = line_end(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }
        // Ctrl+Shift+A — always select all output (exact modifier mask).
        (_, KeyCode::Char('a')) if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
            select_all_output(state);
            state.dirty = true;
        }

        // readline editing
        // Ctrl+K — delete from the cursor to the end of the line
        (KeyModifiers::CONTROL, KeyCode::Char('k')) if !state.is_streaming => {
            let end = line_end(&state.input, state.cursor_pos);
            if end > state.cursor_pos {
                state.input.replace_range(state.cursor_pos..end, "");
            }
            state.selection = None;
            state.refresh_command_selector();
        }
        // Ctrl+U — delete from the start of the line to the cursor
        (KeyModifiers::CONTROL, KeyCode::Char('u')) if !state.is_streaming => {
            let start = line_start(&state.input, state.cursor_pos);
            if start < state.cursor_pos {
                state.input.replace_range(start..state.cursor_pos, "");
                state.cursor_pos = start;
            }
            state.selection = None;
            state.refresh_command_selector();
        }
        // Ctrl+W / Alt+Backspace — delete the word before the cursor
        (KeyModifiers::CONTROL, KeyCode::Char('w')) if !state.is_streaming => {
            let start = word_start_before(&state.input, state.cursor_pos);
            if start < state.cursor_pos {
                state.input.replace_range(start..state.cursor_pos, "");
                state.cursor_pos = start;
            }
            state.selection = None;
            state.refresh_command_selector();
        }
        (KeyModifiers::ALT, KeyCode::Backspace) if !state.is_streaming => {
            let start = word_start_before(&state.input, state.cursor_pos);
            if start < state.cursor_pos {
                state.input.replace_range(start..state.cursor_pos, "");
                state.cursor_pos = start;
            }
            state.selection = None;
            state.refresh_command_selector();
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

        // Shift+PageUp/PageDown/Home/End and Shift+Cmd+Up/Down — extend the
        // output selection (the output is what these keys scroll).
        (KeyModifiers::SHIFT, KeyCode::PageUp) => output_selection_page(state, false),
        (KeyModifiers::SHIFT, KeyCode::PageDown) => output_selection_page(state, true),
        (KeyModifiers::SHIFT, KeyCode::Home) => output_selection_top(state),
        (KeyModifiers::SHIFT, KeyCode::End) => output_selection_bottom(state),
        (_, KeyCode::Up) if key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::SUPER) => {
            output_selection_top(state);
        }
        (_, KeyCode::Down) if key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::SUPER) => {
            output_selection_bottom(state);
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
            state.selection = None;
            state.refresh_command_selector();
        }
        (KeyModifiers::SHIFT, KeyCode::Enter) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
            state.selection = None;
            state.refresh_command_selector();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('j')) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
            state.selection = None;
            state.refresh_command_selector();
        }

        // Enter — submit input
        (_, KeyCode::Enter) if !state.is_streaming => {
            let input_text = state.input.trim().to_string();
            if input_text.is_empty() {
                // Empty submission while in interview input mode: reject the
                // empty target with the shared wording.
                if state.pending_interview_target.is_some() {
                    state.push_system(crate::interview::EMPTY_TARGET_MESSAGE);
                    state.dirty = true;
                }
                return None;
            }

            state.input.clear();
            state.selection = None;
            state.cursor_pos = 0;
            state.command_selector = None;

            if state.pending_interview_target.take().is_some() {
                // Interview input mode: the submitted text is the interview target.
                // The templated prompt is both sent to the agent and shown in
                // the conversation, so the user sees exactly what the agent got.
                let prompt = crate::interview::build_interview_prompt(&input_text);
                state.push_user(&prompt);
                return Some(prompt);
            }

            if input_text.starts_with('/') {
                if let Some(prompt) = handle_slash_command(state, &input_text, config, runtime) {
                    return Some(prompt);
                }
                return None;
            }

            state.input_history.push(input_text.clone());
            state.history_index = None;
            state.push_user(&input_text);
            return Some(input_text);
        }

        // Cmd+Backspace — delete from the start of the line to the cursor
        (KeyModifiers::SUPER, KeyCode::Backspace) if !state.is_streaming => {
            let start = line_start(&state.input, state.cursor_pos);
            if start < state.cursor_pos {
                state.input.replace_range(start..state.cursor_pos, "");
                state.cursor_pos = start;
            }
            state.selection = None;
            state.refresh_command_selector();
        }

        // Backspace — remove the grapheme cluster before the cursor
        (_, KeyCode::Backspace) if !state.is_streaming && state.cursor_pos > 0 => {
            let cluster_start = prev_grapheme_boundary(&state.input, state.cursor_pos);
            if cluster_start < state.cursor_pos {
                state.input.replace_range(cluster_start..state.cursor_pos, "");
                state.cursor_pos = cluster_start;
            }
            state.selection = None;
            state.refresh_command_selector();
        }

        // Delete — remove the grapheme cluster at the cursor
        (_, KeyCode::Delete) if !state.is_streaming && state.cursor_pos < state.input.len() => {
            let cluster_end = next_grapheme_boundary(&state.input, state.cursor_pos);
            if cluster_end > state.cursor_pos {
                state.input.replace_range(state.cursor_pos..cluster_end, "");
            }
            state.selection = None;
            state.refresh_command_selector();
        }

        // Ctrl/Alt+Left — move back by word (readline)
        (KeyModifiers::CONTROL, KeyCode::Left) | (KeyModifiers::ALT, KeyCode::Left)
            if !state.is_streaming =>
        {
            state.cursor_pos = word_start_before(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Ctrl/Alt+Right — move forward by word (readline)
        (KeyModifiers::CONTROL, KeyCode::Right) | (KeyModifiers::ALT, KeyCode::Right)
            if !state.is_streaming =>
        {
            state.cursor_pos = word_end_after(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Shift+Left/Right — extend the output selection by one char when the
        // selection already lives in the output (matches the output-first
        // convention of the other Shift+arrow arms)
        (_, KeyCode::Left)
            if matches!(
                state.selection,
                Some(Selection { target: SelectionTarget::Output, .. })
            ) =>
        {
            let pos = output_pos_backward(state, output_active_pos(state));
            extend_output_selection(state, pos);
            state.dirty = true;
        }
        (_, KeyCode::Right)
            if matches!(
                state.selection,
                Some(Selection { target: SelectionTarget::Output, .. })
            ) =>
        {
            let pos = output_pos_forward(state, output_active_pos(state));
            extend_output_selection(state, pos);
            state.dirty = true;
        }

        // Shift+Left/Right — extend the input selection by one grapheme
        (KeyModifiers::SHIFT, KeyCode::Left) if !state.is_streaming => {
            let new_pos = prev_grapheme_boundary(&state.input, state.cursor_pos);
            apply_input_cursor_move(state, new_pos, true);
        }
        (KeyModifiers::SHIFT, KeyCode::Right) if !state.is_streaming => {
            let new_pos = next_grapheme_boundary(&state.input, state.cursor_pos);
            apply_input_cursor_move(state, new_pos, true);
        }

        // Shift+Cmd+Left/Right — extend the input selection by line (exact masks)
        (_, KeyCode::Left)
            if !state.is_streaming
                && key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::SUPER) =>
        {
            apply_input_cursor_move(state, line_start(&state.input, state.cursor_pos), true);
        }
        (_, KeyCode::Right)
            if !state.is_streaming
                && key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::SUPER) =>
        {
            apply_input_cursor_move(state, line_end(&state.input, state.cursor_pos), true);
        }

        // Shift+Alt / Shift+Ctrl+Left/Right — extend the input selection by word
        (_, KeyCode::Left)
            if !state.is_streaming
                && (key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::ALT)
                    || key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::CONTROL)) =>
        {
            apply_input_cursor_move(state, word_start_before(&state.input, state.cursor_pos), true);
        }
        (_, KeyCode::Right)
            if !state.is_streaming
                && (key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::ALT)
                    || key.modifiers == (KeyModifiers::SHIFT | KeyModifiers::CONTROL)) =>
        {
            apply_input_cursor_move(state, word_end_after(&state.input, state.cursor_pos), true);
        }

        // Cmd+Left — jump to the start of the line
        (KeyModifiers::SUPER, KeyCode::Left) if !state.is_streaming => {
            state.cursor_pos = line_start(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Cmd+Right — jump to the end of the line
        (KeyModifiers::SUPER, KeyCode::Right) if !state.is_streaming => {
            state.cursor_pos = line_end(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Left arrow — move back one grapheme cluster
        (_, KeyCode::Left) if !state.is_streaming && state.cursor_pos > 0 => {
            state.cursor_pos = prev_grapheme_boundary(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Right arrow — move forward one grapheme cluster
        (_, KeyCode::Right) if !state.is_streaming && state.cursor_pos < state.input.len() => {
            state.cursor_pos = next_grapheme_boundary(&state.input, state.cursor_pos);
            state.selection = None;
            state.refresh_command_selector();
        }

        // Shift+Up/Down — extend the output selection one row
        (KeyModifiers::SHIFT, KeyCode::Up) => {
            let pos = output_pos_row(state, output_active_pos(state), false);
            extend_output_selection(state, pos);
            state.dirty = true;
        }
        (KeyModifiers::SHIFT, KeyCode::Down) => {
            let pos = output_pos_row(state, output_active_pos(state), true);
            extend_output_selection(state, pos);
            state.dirty = true;
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
                state.selection = None;
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
                state.selection = None;
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
        (_, KeyCode::Esc) => {
            // Leave interview input mode without submitting a target.
            if state.pending_interview_target.take().is_some() {
                state.dirty = true;
            }
            // Clear any active selection.
            if state.selection.is_some() {
                state.selection = None;
                state.dirty = true;
            }
        }

        // Character input
        (_, KeyCode::Char(c)) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += c.len_utf8();
            state.selection = None;
            state.refresh_command_selector();
        }

        _ => {}
    }

    None
}

/// Whether `key` is the copy shortcut: Cmd+C on macOS, Ctrl+Shift+C elsewhere.
fn is_copy_shortcut(modifiers: KeyModifiers, code: KeyCode) -> bool {
    if code != KeyCode::Char('c') {
        return false;
    }
    // Compare the full modifier bitmask: an or-pattern like
    // `(CONTROL | SHIFT, _)` would match Ctrl+C or Shift+C, not the combo.
    modifiers == KeyModifiers::SUPER
        || modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

/// Copy the current selection to the clipboard, if there is one.
fn copy_selection(state: &AppState) {
    if let Some(text) = state.selection_text() {
        copy_to_clipboard(&text);
    }
}

/// Copy `text` to the system clipboard. Prefers the platform clipboard tool
/// (`pbcopy` / `wl-copy` / `xclip` / `clip`), which works in every terminal,
/// and falls back to the OSC 52 terminal sequence when no tool is available
/// (some terminals ignore OSC 52 entirely).
fn copy_to_clipboard(text: &str) {
    let commands: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    for (command, args) in commands {
        if copy_via_command(command, args, text) {
            return;
        }
    }
    write_osc52_clipboard(text);
}

/// Write `text` to the system clipboard through `command`'s stdin, returning
/// true when the process exited successfully.
fn copy_via_command(command: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = std::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        return false;
    }
    // Closing stdin sends EOF so the tool finishes writing.
    drop(stdin);
    child.wait().map(|status| status.success()).unwrap_or(false)
}

/// Write `text` to the terminal clipboard via the OSC 52 escape sequence
/// (supported by iTerm2, kitty, WezTerm, Alacritty, VSCode, tmux, ...).
fn write_osc52_clipboard(text: &str) {
    use std::io::Write;
    let _ = write_osc52(&mut std::io::stdout(), text);
    let _ = std::io::stdout().flush();
}

/// The OSC 52 clipboard control sequence for `text`.
fn write_osc52(writer: &mut impl std::io::Write, text: &str) -> std::io::Result<()> {
    let encoded = base64_encode(text.as_bytes());
    writer.write_all(format!("\x1b]52;c;{encoded}\x1b\\").as_bytes())
}

/// Minimal standard base64 encoder (RFC 4648) for OSC 52 payloads.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Handle mouse events: the scroll wheel scrolls the focused area; a left
/// click-drag selects text in the output or input box (a plain click in the
/// input box also moves the cursor to the clicked character).
fn handle_mouse(state: &mut AppState, mouse: MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};

    // Scroll wheel — routes to the focused scrollable area (even while streaming).
    match mouse.kind {
        MouseEventKind::ScrollUp if state.side_panel_focused => {
            state.side_panel_scroll.scroll_up(3);
            state.dirty = true;
            return;
        }
        MouseEventKind::ScrollDown if state.side_panel_focused => {
            state.side_panel_scroll.scroll_down(3);
            state.dirty = true;
            return;
        }
        // Wheel over the input box scrolls the input when its content exceeds
        // the box; otherwise the output scrolls as before.
        MouseEventKind::ScrollUp
            if mouse_is_over_input(state, mouse.row, mouse.column)
                && input_overflow_rows(state).is_some_and(|overflow| overflow > 0) =>
        {
            state.input_scroll = state.input_scroll.saturating_sub(3);
            state.dirty = true;
            return;
        }
        MouseEventKind::ScrollDown
            if mouse_is_over_input(state, mouse.row, mouse.column)
                && input_overflow_rows(state).is_some_and(|overflow| overflow > 0) =>
        {
            state.input_scroll = state.input_scroll.saturating_add(3);
            state.dirty = true;
            return;
        }
        MouseEventKind::ScrollUp => {
            state.scroll.scroll_up(3);
            state.dirty = true;
            return;
        }
        MouseEventKind::ScrollDown => {
            state.scroll.scroll_down(3);
            state.dirty = true;
            return;
        }
        _ => {}
    }

    if state.overlay != Overlay::None {
        return;
    }

    match mouse.kind {
        // Left button down — start a selection in the widget under the cursor.
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(pos) = output_click_pos(state, mouse.row, mouse.column) {
                state.selection = Some(Selection {
                    target: SelectionTarget::Output,
                    anchor: pos,
                    active: pos,
                    dragging: true,
                });
                state.dirty = true;
            } else if let Some(pos) = input_click_pos(state, mouse.row, mouse.column) {
                if !state.is_streaming && pos != state.cursor_pos {
                    state.cursor_pos = pos;
                }
                state.selection = Some(Selection {
                    target: SelectionTarget::Input,
                    anchor: SelectionPoint::Input(pos),
                    active: SelectionPoint::Input(pos),
                    dragging: true,
                });
                state.refresh_command_selector();
                state.dirty = true;
            } else {
                // Click outside both boxes clears any selection.
                state.selection = None;
                state.dirty = true;
            }
        }
        // Drag — extend the active endpoint of the current selection.
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some(target) = state.selection.as_ref().map(|s| s.target) else {
                return;
            };
            match target {
                SelectionTarget::Output => {
                    if let Some(pos) = output_click_pos(state, mouse.row, mouse.column) {
                        if let Some(sel) = state.selection.as_mut() {
                            sel.active = pos;
                        }
                        state.dirty = true;
                    }
                }
                SelectionTarget::Input => {
                    if let Some(pos) = input_click_pos(state, mouse.row, mouse.column) {
                        if let Some(sel) = state.selection.as_mut() {
                            sel.active = SelectionPoint::Input(pos);
                        }
                        if !state.is_streaming && pos != state.cursor_pos {
                            state.cursor_pos = pos;
                        }
                        state.dirty = true;
                    }
                }
            }
        }
        // Release — the selection stays (until the next click) so it can be copied.
        // Any-button match: some terminals encode releases without the button.
        MouseEventKind::Up(_) => {
            if let Some(sel) = state.selection.as_mut() {
                sel.dragging = false;
            }
        }
        _ => {}
    }
}

/// Whether `c` starts a word (readline-style: alphanumeric runs).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The grapheme-cluster boundary immediately before `pos`: the byte offset
/// where the previous grapheme cluster starts. A combining-mark cluster
/// (e.g. `e` + `◌́` = `é`) is treated as a single unit, so the cursor never
/// lands between a base char and its combining marks.
fn prev_grapheme_boundary(input: &str, pos: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let pos = pos.min(input.len());
    // The cluster that strictly contains `pos` (start < pos). When `pos` is
    // already on a boundary, the previous cluster's start is the answer.
    let mut prev_end = 0;
    for (start, end) in input
        .grapheme_indices(true)
        .map(|(s, cluster)| (s, s + cluster.len()))
    {
        if end <= pos {
            prev_end = start;
            continue;
        }
        break;
    }
    prev_end
}

/// The grapheme-cluster boundary immediately after `pos`: the byte offset
/// where the next grapheme cluster starts (or `input.len()` at the end).
/// See [`prev_grapheme_boundary`] for why clusters, not codepoints.
fn next_grapheme_boundary(input: &str, pos: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let pos = pos.min(input.len());
    for (_, end) in input
        .grapheme_indices(true)
        .map(|(s, cluster)| (s, s + cluster.len()))
    {
        if end > pos {
            return end;
        }
    }
    input.len()
}

/// The start of the word at or immediately before `pos` (readline `Alt+B`).
/// From the middle of a word this lands on that word's start. Walks grapheme
/// clusters so combining marks stay attached to their base char.
fn word_start_before(input: &str, pos: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let pos = pos.min(input.len());
    // Cluster start byte offsets strictly before `pos`, newest first.
    let starts: Vec<usize> = input[..pos]
        .grapheme_indices(true)
        .map(|(s, _)| s)
        .collect();
    // Two-phase backward walk: skip the non-word gap, then walk to the start
    // of the first word encountered. If `pos` is already mid-word, the gap
    // phase is empty and the word phase starts immediately.
    let mut prev = pos;
    let mut in_word = false;
    for &s in starts.iter().rev() {
        let is_word = cluster_is_word_char(&input[s..prev]);
        if !is_word {
            if in_word {
                // Just stepped out of the word's left edge.
                return prev;
            }
            // Still in the leading gap.
            prev = s;
            continue;
        }
        in_word = true;
        prev = s;
    }
    prev
}

/// The end of the word at or immediately after `pos` (readline `Alt+F`). From
/// the middle of a word this lands on that word's end. Walks grapheme
/// clusters so combining marks stay attached to their base char.
fn word_end_after(input: &str, pos: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let pos = pos.min(input.len());
    // If the cluster at `pos` is a word char, consume the word rightward.
    if let Some((_, first_cluster)) = input[pos..].grapheme_indices(true).next()
        && cluster_is_word_char(first_cluster)
    {
        let mut end = pos + first_cluster.len();
        for cluster in input[end..].graphemes(true) {
            if !cluster_is_word_char(cluster) {
                break;
            }
            end += cluster.len();
        }
        return end;
    }
    // Otherwise skip the non-word gap, then the next word.
    let mut end = pos;
    for cluster in input[pos..].graphemes(true) {
        if cluster_is_word_char(cluster) {
            // Consume the word rightward.
            end += cluster.len();
            for cluster2 in input[end..].graphemes(true) {
                if !cluster_is_word_char(cluster2) {
                    break;
                }
                end += cluster2.len();
            }
            return end;
        }
        end += cluster.len();
    }
    end
}

/// Whether `cluster` starts a word: its first codepoint is alphanumeric.
/// Combining marks are zero-width and classified by the base they attach to,
/// so checking the cluster's first char is the right test.
fn cluster_is_word_char(cluster: &str) -> bool {
    match cluster.chars().next() {
        Some(c) => is_word_char(c),
        None => false,
    }
}

/// Byte offset of the start of the logical line containing `pos` (the byte
/// right after the previous `\n`, or 0).
fn line_start(input: &str, pos: usize) -> usize {
    let pos = input.floor_char_boundary(pos.min(input.len()));
    input[..pos]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Byte offset just past the end of the logical line containing `pos` (the
/// next `\n`, or the end of the input).
fn line_end(input: &str, pos: usize) -> usize {
    let pos = input.floor_char_boundary(pos.min(input.len()));
    match input[pos..].find('\n') {
        Some(i) => pos + i,
        None => input.len(),
    }
}

/// Whether the mouse position is inside the input box.
fn mouse_is_over_input(state: &AppState, row: u16, col: u16) -> bool {
    matches!(
        state.input_area,
        Some((x, y, w, h)) if row >= y && row < y + h && col >= x && col < x + w
    )
}

/// How many content rows the input overflows its box by (0 when it fits).
fn input_overflow_rows(state: &AppState) -> Option<usize> {
    let (_, _, w, h) = state.input_area?;
    if w < 4 || h < 2 {
        return None;
    }
    let prompt = if state.is_streaming { "  " } else { "> " };
    let usable = (w as usize).saturating_sub(2).saturating_sub(prompt.len());
    let rows = input::layout(&state.input, usable);
    Some(rows.len().saturating_sub(h as usize - 2))
}

/// Map a click position to an input-box byte offset, if it lands inside the box.
fn input_click_pos(state: &AppState, row: u16, col: u16) -> Option<usize> {
    let (x, y, w, h) = state.input_area?;
    if w < 4 || h < 2 {
        return None;
    }
    // The input is drawn inside a 1-cell rounded border.
    let inner = Rect {
        x: x + 1,
        y: y + 1,
        width: w.saturating_sub(2),
        height: h.saturating_sub(2),
    };
    if row < inner.y || row >= inner.bottom() || col < inner.x || col >= inner.right() {
        return None;
    }
    let prompt = if state.is_streaming { "  " } else { "> " };
    let usable = (inner.width as usize).saturating_sub(prompt.len());
    let content_row = (row - inner.y) as usize + state.input_scroll as usize;
    let content_col = (col - inner.x) as usize;
    Some(input::char_pos_at_click(
        &state.input,
        prompt,
        usable,
        content_row,
        content_col,
    ))
}

/// Map a click position to an output `(row, grapheme index)` pair, if it
/// lands inside the messages box. The row is clamped to the available
/// content and the grapheme index is the cluster the click fell on (clamped
/// to the row's grapheme count when past the end).
fn output_click_pos(state: &AppState, row: u16, col: u16) -> Option<SelectionPoint> {
    let (x, y, w, h) = state.messages_area?;
    if row < y || row >= y + h || col < x || col >= x + w {
        return None;
    }
    let total = state.virtual_list.total_height();
    if total == 0 {
        return None;
    }
    let offset = state.virtual_list.effective_offset();
    let idx = ((offset as u32 + (row - y) as u32).min(total as u32 - 1)) as usize;
    let text = state.virtual_list.row_text(idx);
    let col_in = (col - x) as usize;
    // Walk grapheme clusters accumulating display width; a cluster's width is
    // the width of its first codepoint with a width (zero-width combining
    // marks contribute nothing). This lands the click on a grapheme boundary.
    use unicode_segmentation::UnicodeSegmentation;
    let mut width = 0usize;
    for (grapheme_idx, grapheme) in text.graphemes(true).enumerate() {
        let gw = grapheme
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>();
        if width + gw > col_in {
            return Some(SelectionPoint::Output(idx, grapheme_idx));
        }
        width += gw;
    }
    let count = text.graphemes(true).count();
    Some(SelectionPoint::Output(idx, count))
}

/// The active endpoint of the current output selection, or (0, 0) when there
/// is none (keyboard selection starts from the top).
fn output_active_pos(state: &AppState) -> SelectionPoint {
    match state.selection.as_ref() {
        Some(sel) if sel.target == SelectionTarget::Output => sel.active,
        _ => SelectionPoint::Output(0, 0),
    }
}

/// Set the active endpoint of an output selection, starting a new selection
/// anchored at (0, 0) when none exists yet.
fn extend_output_selection(state: &mut AppState, active: SelectionPoint) {
    match state.selection.as_mut() {
        Some(sel) if sel.target == SelectionTarget::Output => {
            sel.active = active;
        }
        _ => {
            state.selection = Some(Selection {
                target: SelectionTarget::Output,
                anchor: SelectionPoint::Output(0, 0),
                active,
                dragging: false,
            });
        }
    }
}

/// Move an output position one grapheme forward, wrapping to the next row's
/// start at the end of a row.
fn output_pos_forward(state: &AppState, pos: SelectionPoint) -> SelectionPoint {
    let SelectionPoint::Output(row, col) = pos else {
        return pos;
    };
    let count = state.virtual_list.row_grapheme_count(row);
    if col < count {
        SelectionPoint::Output(row, col + 1)
    } else if row + 1 < state.virtual_list.total_height() as usize {
        SelectionPoint::Output(row + 1, 0)
    } else {
        pos
    }
}

/// Move an output position one grapheme backward, wrapping to the previous
/// row's end at the start of a row.
fn output_pos_backward(state: &AppState, pos: SelectionPoint) -> SelectionPoint {
    let SelectionPoint::Output(row, col) = pos else {
        return pos;
    };
    if col > 0 {
        SelectionPoint::Output(row, col - 1)
    } else if row > 0 {
        let prev_count = state.virtual_list.row_grapheme_count(row - 1);
        SelectionPoint::Output(row - 1, prev_count)
    } else {
        pos
    }
}

/// Move an output position down/up one row, keeping the column clamped to the
/// target row's grapheme count.
fn output_pos_row(state: &AppState, pos: SelectionPoint, down: bool) -> SelectionPoint {
    let SelectionPoint::Output(row, col) = pos else {
        return pos;
    };
    let target = if down { row + 1 } else { row.saturating_sub(1) };
    if target == row || target >= state.virtual_list.total_height() as usize {
        return pos;
    }
    let target_count = state.virtual_list.row_grapheme_count(target);
    let col = col.min(target_count);
    SelectionPoint::Output(target, col)
}

/// Select all output content (rows 0..total, full row lengths).
fn select_all_output(state: &mut AppState) {
    let total = state.virtual_list.total_height() as usize;
    if total == 0 {
        state.selection = None;
        return;
    }
    let last_count = state.virtual_list.row_grapheme_count(total - 1);
    state.selection = Some(Selection {
        target: SelectionTarget::Output,
        anchor: SelectionPoint::Output(0, 0),
        active: SelectionPoint::Output(total - 1, last_count),
        dragging: false,
    });
}

/// Select all input text (for copying the whole message).
fn select_all_input(state: &mut AppState) {
    let len = state.input.len();
    state.selection = Some(Selection {
        target: SelectionTarget::Input,
        anchor: SelectionPoint::Input(0),
        active: SelectionPoint::Input(len),
        dragging: false,
    });
}

/// Move the input cursor and extend/replace the input selection: with `extend`
/// (Shift held) the selection grows from its existing anchor; otherwise any
/// selection is cleared.
fn apply_input_cursor_move(state: &mut AppState, new_pos: usize, extend: bool) {
    let old_pos = state.cursor_pos;
    state.cursor_pos = new_pos;
    if extend {
        match state.selection.as_mut() {
            Some(sel) if sel.target == SelectionTarget::Input => {
                sel.active = SelectionPoint::Input(new_pos);
            }
            _ => {
                state.selection = Some(Selection {
                    target: SelectionTarget::Input,
                    anchor: SelectionPoint::Input(old_pos),
                    active: SelectionPoint::Input(new_pos),
                    dragging: false,
                });
            }
        }
    } else {
        state.selection = None;
    }
    state.refresh_command_selector();
}

/// Extend the output selection to the top of the content.
fn output_selection_top(state: &mut AppState) {
    extend_output_selection(state, SelectionPoint::Output(0, 0));
    state.dirty = true;
}

/// Extend the output selection to the bottom of the content.
fn output_selection_bottom(state: &mut AppState) {
    let total = state.virtual_list.total_height() as usize;
    if total > 0 {
        let last_count = state.virtual_list.row_grapheme_count(total - 1);
        extend_output_selection(state, SelectionPoint::Output(total - 1, last_count));
    }
    state.dirty = true;
}

/// Extend the output selection by roughly one page up or down.
fn output_selection_page(state: &mut AppState, down: bool) {
    let total = state.virtual_list.total_height() as usize;
    if total == 0 {
        return;
    }
    let page = state
        .messages_area
        .map(|(_, _, _, h)| (h.saturating_sub(2) as usize).max(1))
        .unwrap_or(5);
    let SelectionPoint::Output(row, col) = output_active_pos(state) else {
        return;
    };
    let target_row = if down {
        (row + page).min(total - 1)
    } else {
        row.saturating_sub(page)
    };
    let target_count = state.virtual_list.row_grapheme_count(target_row);
    let col = col.min(target_count);
    extend_output_selection(state, SelectionPoint::Output(target_row, col));
    state.dirty = true;
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
                    state.model = label.clone();
                    state.effective_model = Some(crate::providers::display_model_id(
                        &runtime.effective().0,
                        &runtime.effective().1,
                    ));
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

/// Handle keys while the `/combos` fuzzy picker is open: typing filters the
/// list live, Enter switches the runtime to the selected combo, Esc closes.
fn handle_combo_picker_key(
    state: &mut AppState,
    key: KeyEvent,
    runtime: &Arc<AgentRuntime>,
) -> Option<String> {
    let Overlay::ComboPicker(picker) = &mut state.overlay else {
        return None;
    };
    match key.code {
        // Typing filters the list (live fuzzy).
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            picker.query.push(c);
            picker.selected = 0;
            state.dirty = true;
        }
        KeyCode::Backspace => {
            picker.query.pop();
            picker.selected = 0;
            state.dirty = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            picker.selected = picker.selected.saturating_sub(1);
            state.dirty = true;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let len = picker.filtered().len();
            if picker.selected + 1 < len {
                picker.selected += 1;
            }
            state.dirty = true;
        }
        KeyCode::Enter => {
            let filtered = picker.filtered();
            if filtered.is_empty() {
                return None;
            }
            let idx = picker.selected.min(filtered.len() - 1);
            let combo = filtered[idx].name.clone();
            state.overlay = Overlay::None;
            let label = crate::providers::display_model_id("combos", &combo);
            match runtime.switch("combos", &combo) {
                Ok(()) => {
                    state.model = label.clone();
                    state.effective_model = Some(crate::providers::display_model_id(
                        &runtime.effective().0,
                        &runtime.effective().1,
                    ));
                    state.push_system(format!("Switched to {label}"));
                }
                Err(e) => state.push_system(format!("Failed to switch to {label}: {e}")),
            }
            state.dirty = true;
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            state.overlay = Overlay::None;
            state.dirty = true;
        }
        _ => {}
    }
    None
}

/// Open the `/provider` model explorer overlay and kick off the fetch of the
/// current provider's full model list.
fn open_provider_explorer(state: &mut AppState, runtime: &Arc<AgentRuntime>) {
    let providers = runtime.explorer_providers();
    if providers.is_empty() {
        state.push_system("No providers configured.");
        return;
    }
    let (current_provider, _model) = runtime.current();
    state.overlay = Overlay::ProviderExplorer(ProviderExplorerState {
        phase: ProviderExplorerPhase::Providers,
        query: String::new(),
        selected: 0,
        providers,
        provider: None,
        current_provider: Some(current_provider),
        all_models: Vec::new(),
        loading: false,
        error: String::new(),
        free_only: false,
    });
    state.dirty = true;
}

/// Kick off a `/models` fetch for `provider` (served from the runtime's
/// in-memory cache when it was already fetched this run) and update the
/// explorer when it lands.
fn start_model_fetch(state: &mut AppState, runtime: &Arc<AgentRuntime>, provider: &str) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.provider_fetch_rx = Some(rx);
    // Cancel any prior in-flight fetch (browsing another provider).
    if let Some(prev) = state._provider_fetch_task.take()
        && !prev.is_finished()
    {
        prev.abort();
    }
    let provider_name = provider.to_string();
    let runtime = Arc::clone(runtime);
    state._provider_fetch_task = Some(tokio::spawn(async move {
        let result = runtime.fetch_models_cached(&provider_name).await;
        let _ = tx.send((provider_name, result));
    }));
}

/// Drain the `/provider` fetch result, if one arrived, and update the overlay.
fn poll_provider_fetch(state: &mut AppState) {
    use tokio::sync::oneshot::error::TryRecvError;
    let Some(rx) = state.provider_fetch_rx.as_mut() else {
        return;
    };
    let (provider, result) = match rx.try_recv() {
        Ok(v) => v,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Closed) => {
            state.provider_fetch_rx = None;
            return;
        }
    };
    state.provider_fetch_rx = None;
    let Overlay::ProviderExplorer(p) = &mut state.overlay else {
        return;
    };
    // Only the Models phase consumes fetch results, and only for the provider
    // it is browsing (a stale result from a provider left via Esc is ignored).
    if p.phase != ProviderExplorerPhase::Models
        || p.provider.as_ref().map(|pp| pp.name.as_str()) != Some(provider.as_str())
    {
        return;
    }
    p.loading = false;
    match result {
        Ok(models) => {
            p.all_models = models;
            p.selected = 0;
        }
        Err(e) => {
            p.error = e;
        }
    }
    state.dirty = true;
}

/// Handle keys while the `/provider` explorer is open. In the Providers
/// phase, typing filters the provider list, Enter browses the selected
/// provider's models, Esc closes. In the Models phase, typing filters the
/// model list (free models highlighted), `f` toggles the free-models-only
/// filter, Enter switches the runtime to the selected model, Esc goes back
/// to the provider list. All of it is keyboard-only.
fn handle_provider_explorer_key(
    state: &mut AppState,
    key: KeyEvent,
    runtime: &Arc<AgentRuntime>,
) -> Option<String> {
    let Overlay::ProviderExplorer(p) = &mut state.overlay else {
        return None;
    };
    match p.phase {
        ProviderExplorerPhase::Providers => match key.code {
            // Typing filters the provider list (live fuzzy).
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                p.query.push(c);
                p.selected = 0;
                state.dirty = true;
            }
            KeyCode::Backspace => {
                p.query.pop();
                p.selected = 0;
                state.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                p.selected = p.selected.saturating_sub(1);
                state.dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = p.filtered_providers().len();
                if p.selected + 1 < len {
                    p.selected += 1;
                }
                state.dirty = true;
            }
            // Enter: move to the Models phase for the selected provider.
            KeyCode::Enter => {
                let filtered = p.filtered_providers();
                if filtered.is_empty() {
                    return None;
                }
                let idx = p.selected.min(filtered.len() - 1);
                let provider = filtered[idx].clone();
                p.phase = ProviderExplorerPhase::Models;
                p.query.clear();
                p.selected = 0;
                p.provider = Some(provider.clone());
                p.all_models.clear();
                p.loading = false;
                p.error = String::new();
                p.free_only = false;
                state.dirty = true;
                // Show the cached model list if already fetched this run;
                // otherwise kick off the (cached) fetch.
                match runtime.cached_models(&provider.name) {
                    Some(Ok(models)) => p.all_models = models,
                    Some(Err(e)) => p.error = e,
                    None => {
                        p.loading = true;
                        start_model_fetch(state, runtime, &provider.name);
                    }
                }
            }
            KeyCode::Esc => {
                state.overlay = Overlay::None;
                state.dirty = true;
            }
            _ => {}
        },
        ProviderExplorerPhase::Models => match key.code {
            // `f` toggles the free-models-only filter (typed into the query
            // everywhere else).
            KeyCode::Char('f') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                p.free_only = !p.free_only;
                p.selected = 0;
                state.dirty = true;
            }
            // Typing filters the model list (live fuzzy).
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                p.query.push(c);
                p.selected = 0;
                state.dirty = true;
            }
            KeyCode::Backspace => {
                p.query.pop();
                p.selected = 0;
                state.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                p.selected = p.selected.saturating_sub(1);
                state.dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = p.filtered_models().len();
                if p.selected + 1 < len {
                    p.selected += 1;
                }
                state.dirty = true;
            }
            KeyCode::Enter => {
                let filtered = p.filtered_models();
                if filtered.is_empty() {
                    return None;
                }
                let idx = p.selected.min(filtered.len() - 1);
                let model = filtered[idx].to_string();
                let provider = p
                    .provider
                    .as_ref()
                    .map(|pp| pp.name.clone())
                    .unwrap_or_default();
                state.overlay = Overlay::None;
                // Abort the in-flight fetch (if any) since we're closing.
                if let Some(task) = state._provider_fetch_task.take()
                    && !task.is_finished()
                {
                    task.abort();
                }
                state.provider_fetch_rx = None;
                match runtime.switch(&provider, &model) {
                    Ok(()) => {
                        let label = crate::providers::display_model_id(&provider, &model);
                        state.model = label.clone();
                        state.effective_model = Some(crate::providers::display_model_id(
                            &runtime.effective().0,
                            &runtime.effective().1,
                        ));
                        state.push_system(format!("Switched to {label}"));
                    }
                    Err(e) => {
                        state.push_system(format!("Failed to switch to {provider}/{model}: {e}"))
                    }
                }
                state.dirty = true;
            }
            // Esc goes back to the provider list; the fetch task stays alive
            // so re-entering this provider shows the (cached) result at once.
            KeyCode::Esc => {
                p.phase = ProviderExplorerPhase::Providers;
                p.query.clear();
                p.selected = 0;
                p.provider = None;
                p.all_models.clear();
                p.loading = false;
                p.error = String::new();
                p.free_only = false;
                state.dirty = true;
            }
            _ => {}
        },
    }
    None
}

/// Handle key events while the AskUser overlay is showing.
fn handle_ask_user_key(
    state: &mut AppState,
    key: KeyEvent,
    runtime: &Arc<AgentRuntime>,
) -> Option<String> {
    let Overlay::AskUser(p) = &mut state.overlay else {
        return None;
    };
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            p.current_input.push(c);
            p.cursor_pos = p.current_input.len();
            state.dirty = true;
        }
        KeyCode::Backspace if !p.current_input.is_empty() => {
            p.current_input.pop();
            p.cursor_pos = p.current_input.len();
            state.dirty = true;
        }
        KeyCode::Left if p.cursor_pos > 0 => {
            p.cursor_pos -= 1;
            state.dirty = true;
        }
        KeyCode::Right if p.cursor_pos < p.current_input.len() => {
            p.cursor_pos += 1;
            state.dirty = true;
        }
        KeyCode::Up if p.focused_question > 0 => {
            // Save current answer before moving up.
            if p.focused_question < p.answers.len() {
                p.answers[p.focused_question] = p.current_input.clone();
            }
            p.focused_question -= 1;
            p.current_input = p.answers[p.focused_question].clone();
            p.cursor_pos = p.current_input.len();
            state.dirty = true;
        }
        KeyCode::Down if p.focused_question + 1 < p.questions.len() => {
            // Save current answer before moving down.
            p.answers[p.focused_question] = p.current_input.clone();
            p.focused_question += 1;
            p.current_input = p.answers[p.focused_question].clone();
            p.cursor_pos = p.current_input.len();
            state.dirty = true;
        }
        KeyCode::Enter => {
            // Save current answer and submit all answers.
            p.answers[p.focused_question] = p.current_input.clone();
            let request_id = p.request_id;
            let answers: Vec<Option<crate::providers::AskUserAnswerValue>> = p
                .answers
                .iter()
                .map(|a| {
                    if a.is_empty() {
                        None // Skipped
                    } else {
                        Some(crate::providers::AskUserAnswerValue::OtherText(a.clone()))
                    }
                })
                .collect();
            let answer = crate::providers::AskUserAnswer {
                request_id,
                answers,
            };
            runtime.send_ask_user_answer(answer);
            state.overlay = Overlay::None;
            state.dirty = true;
            // Return a placeholder prompt so the event loop doesn't try to
            // send a chat message — the agent run is already streaming.
            return Some(String::new());
        }
        KeyCode::Esc => {
            // Cancel the interview: send all-skipped answers.
            let request_id = p.request_id;
            let answers: Vec<Option<crate::providers::AskUserAnswerValue>> = vec![
                None;
                p.questions.len()
            ];
            let answer = crate::providers::AskUserAnswer {
                request_id,
                answers,
            };
            runtime.send_ask_user_answer(answer);
            state.overlay = Overlay::None;
            state.dirty = true;
            return Some(String::new());
        }
        _ => {}
    }
    None
}

fn handle_agent_event(state: &mut AppState, runtime: &Arc<AgentRuntime>, event: AgentEvent) {
    match event {
        AgentEvent::TextDelta(text) => {
            state.append_text(&text);
        }
        AgentEvent::ThinkingDelta(text) => {
            state.append_thinking(&text);
        }
        AgentEvent::ToolStart {
            name, id: _, input, ..
        } => {
            let summary = tool_input_summary(&name, &input);
            state.active_blocks.push(crate::tui::app::OutputBlock::Tool(ToolCall {
                name,
                input_summary: summary,
                status: ToolStatus::Running,
                output_preview: None,
                started_at: Instant::now(),
                duration_ms: None,
                children: Vec::new(),
                run_id: None,
            }));
            state.tool_count += 1;
            // A `spawn_agents` call may have started before the UI processed
            // its ToolStart — attach any sub-agent activity that arrived in
            // the meantime.
            if state
                .active_blocks
                .last()
                .is_some_and(|b| matches!(b, crate::tui::app::OutputBlock::Tool(t) if t.name == "spawn_agents"))
            {
                drain_pending_subagents(state);
            }
        }
        AgentEvent::ToolEnd {
            name,
            id: _,
            result,
            is_error,
            duration,
            compression: _,
        } => {
            if let Some(crate::tui::app::OutputBlock::Tool(tool)) = state
                .active_blocks
                .iter_mut()
                .rev()
                .find(|b| matches!(b, crate::tui::app::OutputBlock::Tool(t) if t.name == name))
            {
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
                blocks: Vec::new(),
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

/// The index of the last `spawn_agents` tool call in `blocks` matching
/// `pred` (None when no block holds such a call).
fn last_spawn_tool_block(
    blocks: &[crate::tui::app::OutputBlock],
    pred: impl Fn(&ToolCall) -> bool,
) -> Option<usize> {
    blocks.iter().rposition(|b| match b {
        crate::tui::app::OutputBlock::Tool(t) => t.name == "spawn_agents" && pred(t),
        _ => false,
    })
}

/// Find the `spawn_agents` call that owns `run_id`: first any call already
/// bound to that run (active or committed — late-arriving events land here),
/// then the currently-running spawn call to bind to. Tools run sequentially,
/// so at most one spawn call is in flight at a time.
fn find_spawn_parent(state: &AppState, run_id: u64) -> Option<SpawnParentLoc> {
    if let Some(idx) = last_spawn_tool_block(&state.active_blocks, |t| tool_owns_run(t, run_id)) {
        return Some(SpawnParentLoc::Active(idx));
    }
    if let Some(turn) = state.turns.last()
        && let Some(idx) = last_spawn_tool_block(&turn.blocks, |t| tool_owns_run(t, run_id))
    {
        return Some(SpawnParentLoc::Committed(idx));
    }
    last_spawn_tool_block(&state.active_blocks, |t| t.status == ToolStatus::Running)
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
    // `loc` always points at a Tool block (find_spawn_parent only returns
    // indices of `spawn_agents` tool calls).
    let parent = match loc {
        SpawnParentLoc::Active(idx) => match &mut state.active_blocks[idx] {
            crate::tui::app::OutputBlock::Tool(t) => t,
            _ => return,
        },
        SpawnParentLoc::Committed(idx) => {
            match &mut state.turns.last_mut().expect("checked").blocks[idx] {
                crate::tui::app::OutputBlock::Tool(t) => t,
                _ => return,
            }
        }
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
    // Only called right after the `spawn_agents` ToolStart was pushed, so the
    // trailing block is that call; without it there is nothing to attach to
    // yet, so keep the events buffered.
    let Some(crate::tui::app::OutputBlock::Tool(parent)) = state.active_blocks.last_mut() else {
        return;
    };
    let pending = std::mem::take(&mut state.pending_subagent);
    for (run_id, activity) in pending {
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
                && let Some(tool) = header
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
) -> Option<String> {
    let cmd = input
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    match cmd {
        "interview" => {
            let rest = input
                .trim_start_matches('/')
                .strip_prefix("interview")
                .map(str::trim)
                .unwrap_or("");
            if rest.is_empty() {
                // Enter interview input mode: the next message is the target.
                state.pending_interview_target = Some(String::new());
            } else {
                // Inline mode: build the interview prompt and return it. Show
                // the full templated prompt so the user sees what the agent
                // received, not just their raw target text.
                let prompt = crate::interview::build_interview_prompt(rest);
                state.push_user(&prompt);
                return Some(prompt);
            }
        }
        "help" | "h" | "?" => {
            state.overlay = Overlay::Help;
        }
        "clear" => {
            state.turns.clear();
            state.active_blocks.clear();
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
                    blocks: Vec::new(),
                });
            } else {
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: "Nothing to rewind.".into(),
                    blocks: Vec::new(),
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
                blocks: Vec::new(),
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
                                state.model = label.clone();
                                state.effective_model = Some(crate::providers::display_model_id(
                                    &runtime.effective().0,
                                    &runtime.effective().1,
                                ));
                                state.push_system(format!("Switched to {label}"));
                            }
                            Err(e) => state.push_system(format!("Failed to switch to {label}: {e}")),
                        }
                    }
                    Err(e) => state.push_system(format!("{e}")),
                }
            }
        }
        "combos" => {
            let rest = input
                .trim_start_matches('/')
                .strip_prefix("combos")
                .map(str::trim)
                .unwrap_or("");
            if rest.is_empty() {
                // Open the fuzzy picker so a combo can be selected interactively.
                let (cur_provider, cur_model) = runtime.current();
                let current = (cur_provider == "combos").then_some(cur_model);
                state.overlay = Overlay::ComboPicker(ComboPickerState {
                    combos: crate::providers::combos(config),
                    query: String::new(),
                    selected: 0,
                    current,
                });
                state.dirty = true;
            } else {
                match runtime.select_text(&format!("combos/{rest}")) {
                    Ok((provider, model)) => {
                        let label = crate::providers::display_model_id(&provider, &model);
                        match runtime.switch(&provider, &model) {
                            Ok(()) => {
                                state.model = label.clone();
                                state.effective_model = Some(crate::providers::display_model_id(
                                    &runtime.effective().0,
                                    &runtime.effective().1,
                                ));
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
                blocks: Vec::new(),
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
                blocks: Vec::new(),
            });
        }
        "provider" => {
            // Open the model explorer: fetch the full model list from the
            // current provider's `/models` endpoint and present it for
            // fuzzy filtering. Selecting an entry switches live.
            open_provider_explorer(state, runtime);
        }
        _ => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Unknown command: /{cmd}. Type /help for commands."),
                blocks: Vec::new(),
            });
        }
    }
    None
}

/// A short summary of a tool call's input for the tool badge line. Shared with
/// the sub-agent event forwarder in `subagents.rs`.
pub(crate) fn tool_input_summary(name: &str, input: &serde_json::Value) -> String {
    match name {
        // The full command is shown in the tool block — never elided (the
        // renderer wraps it to the available width instead of truncating).
        "Bash" | "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
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

    /// Push a tool call as the trailing active block (as ToolStart does).
    fn push_tool(state: &mut AppState, tool: ToolCall) {
        state.active_blocks.push(crate::tui::app::OutputBlock::Tool(tool));
    }

    /// The tool call in `blocks` at `idx` (panics if that block isn't a tool).
    fn tool_at(blocks: &[crate::tui::app::OutputBlock], idx: usize) -> &ToolCall {
        match &blocks[idx] {
            crate::tui::app::OutputBlock::Tool(t) => t,
            _ => panic!("block {idx} is not a tool call"),
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
        assert!(s.active_blocks.is_empty());

        // The parent ToolStart arrives (as handle_agent_event pushes it) and
        // drains the buffer.
        push_tool(&mut s, spawn_call(ToolStatus::Running, None));
        drain_pending_subagents(&mut s);
        assert!(s.pending_subagent.is_empty());

        let parent = tool_at(&s.active_blocks, 0);
        assert_eq!(parent.run_id, Some(1));
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].name, "[code-reviewer]");
        assert_eq!(parent.children[0].status, ToolStatus::Running);
    }

    #[test]
    fn subagent_events_nest_under_running_parent() {
        let mut s = state();
        push_tool(&mut s, spawn_call(ToolStatus::Running, None));

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

        let parent = tool_at(&s.active_blocks, 0);
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
        push_tool(&mut s, spawn_call(ToolStatus::Running, None));

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

        let parent = tool_at(&s.active_blocks, 0);
        assert_eq!(parent.children.len(), 2);
        assert_eq!(parent.children[0].name, "[researcher-web]");
        assert_eq!(parent.children[1].name, "[code-searcher]");
        // Each header got its own run id, so both are Done.
        assert!(parent.children.iter().all(|c| c.status == ToolStatus::Done));
    }

    #[test]
    fn late_events_attach_to_committed_turn() {
        let mut s = state();
        push_tool(&mut s, spawn_call(ToolStatus::Running, None));
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

        assert!(s.active_blocks.is_empty());
        assert!(s.pending_subagent.is_empty());
        let turn = s.turns.last().expect("committed");
        let parent = tool_at(&turn.blocks, 0);
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

    #[test]
    fn line_navigation_boundaries() {
        let input = "hello\nworld foo"; // 15 bytes, '\n' at 5
        // Middle of the second line.
        assert_eq!(line_start(input, 9), 6);
        assert_eq!(line_end(input, 9), 15);
        // First line.
        assert_eq!(line_start(input, 3), 0);
        assert_eq!(line_end(input, 3), 5);
        // Sitting on the newline: start of line 1, end of line 1.
        assert_eq!(line_start(input, 5), 0);
        assert_eq!(line_end(input, 5), 5);
        // Just past the newline.
        assert_eq!(line_start(input, 6), 6);
        assert_eq!(line_end(input, 6), 15);
        // End of input.
        assert_eq!(line_start(input, 15), 6);
        assert_eq!(line_end(input, 15), 15);
        // Empty input.
        assert_eq!(line_start("", 0), 0);
        assert_eq!(line_end("", 0), 0);
        // Multi-byte content never panics: a position inside 'é' (bytes 1..3)
        // snaps back to a char boundary before slicing.
        // "héllo\nwörld" = 13 bytes, '\n' at 6.
        assert_eq!(line_start("héllo\nwörld", 8), 7);
        assert_eq!(line_end("héllo\nwörld", 2), 6); // floor(2) = 0 → line 1 ends at 6
    }

    #[test]
    fn osc52_sequence_is_well_formed() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "hi").unwrap();
        assert_eq!(buf, b"\x1b]52;c;aGk=\x1b\\");
        let mut buf = Vec::new();
        write_osc52(&mut buf, "").unwrap();
        assert_eq!(buf, b"\x1b]52;c;\x1b\\");
    }

    #[test]
    fn base64_encodes_standard_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // Non-ASCII bytes round-trip through the same alphabet.
        assert_eq!(base64_encode("—".as_bytes()), "4oCU");
    }

    #[test]
    fn copy_shortcut_matches_platform_conventions() {
        use crossterm::event::KeyModifiers as M;
        assert!(is_copy_shortcut(M::SUPER, KeyCode::Char('c'))); // macOS Cmd+C
        assert!(is_copy_shortcut(M::CONTROL | M::SHIFT, KeyCode::Char('c'))); // Ctrl+Shift+C
        // Plain Ctrl+C keeps its existing cancel/quit/clear meaning.
        assert!(!is_copy_shortcut(M::CONTROL, KeyCode::Char('c')));
        assert!(!is_copy_shortcut(M::SUPER, KeyCode::Char('v')));
        assert!(!is_copy_shortcut(KeyModifiers::NONE, KeyCode::Char('c')));
    }

    /// A runtime that builds offline (dummy provider, literal key).
    fn runtime() -> Arc<AgentRuntime> {
        use crate::config::{AppConfig, ProviderConfigEntry};
        let mut config = AppConfig {
            provider: "test".into(),
            model: "test/test-model".into(),
            permissions_mode: "allow_all".into(),
            ..Default::default()
        };
        config.providers.insert(
            "test".into(),
            ProviderConfigEntry {
                base_url: Some("http://127.0.0.1:1".into()),
                api_key: Some("test-key".into()),
                models: vec!["test/test-model".into()],
            },
        );
        Arc::new(AgentRuntime::new(&config).unwrap())
    }

    /// A runtime with a `test` provider and two combos referencing it.
    fn runtime_with_combos() -> (AppConfig, Arc<AgentRuntime>) {
        use crate::config::{ComboEntry, ProviderConfigEntry};
        let mut config = AppConfig {
            provider: "test".into(),
            model: "test/test-model".into(),
            permissions_mode: "allow_all".into(),
            ..Default::default()
        };
        config.providers.insert(
            "test".into(),
            ProviderConfigEntry {
                base_url: Some("http://127.0.0.1:1".into()),
                api_key: Some("test-key".into()),
                models: vec!["test/test-model".into(), "test/test-2".into()],
            },
        );
        config.combos.insert(
            "coding".into(),
            vec![
                ComboEntry {
                    provider: "test".into(),
                    model: "test/test-model".into(),
                },
                ComboEntry {
                    provider: "test".into(),
                    model: "test/test-2".into(),
                },
            ],
        );
        config.combos.insert(
            "writing".into(),
            vec![ComboEntry {
                provider: "test".into(),
                model: "test/test-2".into(),
            }],
        );
        let runtime = Arc::new(AgentRuntime::new(&config).unwrap());
        (config, runtime)
    }

    #[test]
    fn combos_command_opens_fuzzy_picker() {
        let (config, runtime) = runtime_with_combos();

        let mut s = state();
        handle_slash_command(&mut s, "/combos", &config, &runtime);
        let Overlay::ComboPicker(picker) = &s.overlay else {
            panic!("expected ComboPicker overlay, got {:?}", s.overlay);
        };
        assert_eq!(picker.combos.len(), 2);
        // Runtime is on the plain `test` provider: no combo is active.
        assert_eq!(picker.current, None);

        // After switching to a combo, the picker marks it active.
        runtime.switch("combos", "coding").unwrap();
        let mut s = state();
        handle_slash_command(&mut s, "/combos", &config, &runtime);
        let Overlay::ComboPicker(picker) = &s.overlay else {
            panic!("expected ComboPicker overlay, got {:?}", s.overlay);
        };
        assert_eq!(picker.current.as_deref(), Some("coding"));
    }

    #[test]
    fn combo_picker_filters_and_switches_on_enter() {
        let (config, runtime) = runtime_with_combos();
        let mut s = state();
        handle_slash_command(&mut s, "/combos", &config, &runtime);

        // Typing narrows the list; Enter switches to the highlighted combo.
        handle_combo_picker_key(
            &mut s,
            key_for(KeyCode::Char('w'), KeyModifiers::NONE),
            &runtime,
        );
        let Overlay::ComboPicker(picker) = &s.overlay else {
            panic!("expected ComboPicker overlay, got {:?}", s.overlay);
        };
        assert_eq!(picker.filtered().len(), 1);
        assert_eq!(picker.filtered()[0].name, "writing");

        handle_combo_picker_key(&mut s, key_for(KeyCode::Enter, KeyModifiers::NONE), &runtime);
        assert!(matches!(s.overlay, Overlay::None));
        let (provider, model) = runtime.current();
        assert_eq!((provider.as_str(), model.as_str()), ("combos", "writing"));
        assert_eq!(s.model, "combos/writing");
        let last = s.turns.last().unwrap();
        assert!(
            last.content.contains("Switched to combos/writing"),
            "{}",
            last.content
        );

        // Esc closes without switching.
        let mut s = state();
        handle_slash_command(&mut s, "/combos", &config, &runtime);
        handle_combo_picker_key(&mut s, key_for(KeyCode::Esc, KeyModifiers::NONE), &runtime);
        assert!(matches!(s.overlay, Overlay::None));
    }

    #[test]
    fn combos_command_switches_to_named_combo() {
        let (config, runtime) = runtime_with_combos();

        let mut s = state();
        handle_slash_command(&mut s, "/combos coding", &config, &runtime);
        let last = s.turns.last().unwrap();
        assert_eq!(last.role, crate::tui::app::TurnRole::System);
        assert!(last.content.contains("Switched to combos/coding"), "{}", last.content);
        assert_eq!(s.model, "combos/coding");
        assert_eq!(s.effective_model.as_deref(), Some("test/test-model"));
        let (provider, model) = runtime.current();
        assert_eq!((provider.as_str(), model.as_str()), ("combos", "coding"));

        // Unknown combo names surface the error, like /model does.
        let mut s = state();
        handle_slash_command(&mut s, "/combos nope", &config, &runtime);
        let last = s.turns.last().unwrap();
        assert!(!last.content.contains("Switched to"), "{}", last.content);
    }

    #[test]
    fn interview_inline_mode_sends_and_displays_full_prompt() {
        let (config, runtime) = runtime_with_combos();
        let mut s = state();
        let prompt = handle_slash_command(&mut s, "/interview add OAuth support", &config, &runtime)
            .expect("inline /interview returns a prompt");
        // The agent receives the full templated prompt, not the raw command.
        assert!(prompt.starts_with(crate::interview::INTERVIEW_BASE_PROMPT));
        assert!(prompt.ends_with("add OAuth support"));
        assert!(!prompt.contains("/interview"));
        // The TUI shows the same full prompt as the user turn.
        let last = s.turns.last().unwrap();
        assert_eq!(last.role, crate::tui::app::TurnRole::User);
        assert_eq!(last.content, prompt);
    }

    #[test]
    fn plain_message_enter_displays_user_turn() {
        let config = AppConfig::default();
        let cancel = CancellationToken::new();
        let rt = runtime();
        let mut s = state();

        // A normal (non-slash) message: the submitted text is returned as the
        // prompt AND shown as a user turn in the conversation.
        s.input = "hello world".into();
        s.cursor_pos = s.input.len();
        let prompt = handle_key(
            &mut s,
            key_for(KeyCode::Enter, KeyModifiers::NONE),
            &config,
            &cancel,
            &rt,
        )
        .expect("submitting a plain message returns a prompt");
        assert_eq!(prompt, "hello world");
        let last = s.turns.last().unwrap();
        assert_eq!(last.role, crate::tui::app::TurnRole::User);
        assert_eq!(last.content, "hello world");
    }

    #[test]
    fn interview_input_mode_sends_and_displays_full_prompt() {
        let config = AppConfig::default();
        let cancel = CancellationToken::new();
        let rt = runtime();
        let mut s = state();

        // `/interview` with no args enters the highlighted input mode.
        assert!(handle_slash_command(&mut s, "/interview", &config, &rt).is_none());
        assert!(s.pending_interview_target.is_some());
        assert!(!s.is_streaming);

        // Submitting the target returns the full templated prompt to the agent
        // and shows it in the conversation.
        s.input = "add OAuth support".into();
        s.cursor_pos = s.input.len();
        let prompt = handle_key(
            &mut s,
            key_for(KeyCode::Enter, KeyModifiers::NONE),
            &config,
            &cancel,
            &rt,
        )
        .expect("submitting an interview target returns a prompt");
        assert!(prompt.starts_with(crate::interview::INTERVIEW_BASE_PROMPT));
        assert!(prompt.ends_with("add OAuth support"));
        assert!(!prompt.contains("/interview"));
        let last = s.turns.last().unwrap();
        assert_eq!(last.role, crate::tui::app::TurnRole::User);
        assert_eq!(last.content, prompt);
        // The mode resets after submission.
        assert!(s.pending_interview_target.is_none());
    }

    fn key_for(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn handle_editing_key(state: &mut AppState, key: KeyEvent) {
        let config = AppConfig::default();
        let cancel = CancellationToken::new();
        let rt = runtime();
        handle_key(state, key, &config, &cancel, &rt);
    }

    fn left_down(row: u16, col: u16) -> MouseEvent {
        use crossterm::event::{MouseButton, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn left_drag(row: u16, col: u16) -> MouseEvent {
        use crossterm::event::{MouseButton, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn left_up(row: u16, col: u16) -> MouseEvent {
        use crossterm::event::{MouseButton, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_drag_selects_input_text_and_follows_cursor() {
        let mut s = state();
        s.input = "hello world".into();
        s.input_area = Some((0, 10, 24, 4)); // inner box: x=1..23, y=11..13, usable 20

        // Click inside the input, then drag right.
        handle_mouse(&mut s, left_down(11, 1));
        assert_eq!(s.cursor_pos, 0);
        handle_mouse(&mut s, left_drag(11, 7));
        handle_mouse(&mut s, left_up(11, 7));

        let sel = s.selection.expect("selection started");
        assert_eq!(sel.target, SelectionTarget::Input);
        assert!(!sel.dragging);
        assert_eq!(s.cursor_pos, 4); // cursor follows the drag endpoint
        assert_eq!(s.selection_text().as_deref(), Some("hell"));

        // A click outside both boxes clears the old selection.
        s.input_area = None;
        s.messages_area = Some((0, 0, 10, 10));
        handle_mouse(&mut s, left_down(20, 30));
        assert!(s.selection.is_none());
    }

    #[test]
    fn mouse_drag_selects_output_text_across_rows() {
        use crate::tui::virtual_list::VItem;
        use ratatui::prelude::*;

        let mut s = state();
        s.messages_area = Some((2, 0, 20, 10));
        // The app sets the viewport every frame; without it the sticky-bottom
        // offset would clamp every click to the last row.
        s.virtual_list.set_viewport(10);
        s.virtual_list.set_committed(vec![
            VItem::new(Line::from("line one")),
            VItem::new(Line::from("line two")),
        ]);

        handle_mouse(&mut s, left_down(0, 3));
        handle_mouse(&mut s, left_drag(1, 9));
        handle_mouse(&mut s, left_up(1, 9));

        let sel = s.selection.expect("selection started");
        assert_eq!(sel.target, SelectionTarget::Output);
        assert_eq!(s.selection_text().as_deref(), Some("ine one\nline tw"));

        // A click outside both boxes clears the selection.
        handle_mouse(&mut s, left_down(5, 30));
        assert!(s.selection.is_none());
    }

    fn scroll_up(row: u16, col: u16) -> MouseEvent {
        use crossterm::event::MouseEventKind;
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn scroll_down(row: u16, col: u16) -> MouseEvent {
        use crossterm::event::MouseEventKind;
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_over_input_scrolls_input_when_it_overflows() {
        // 12 logical lines over a 4-row box (2 content rows inside the
        // border): the input overflows and the wheel scrolls it.
        let mut s = state();
        s.input = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12".into();
        s.input_area = Some((0, 10, 20, 4)); // inner height 2, usable 14

        // Scrolling down inside the box moves the input scroll.
        handle_mouse(&mut s, scroll_down(11, 5));
        assert_eq!(s.input_scroll, 3);
        handle_mouse(&mut s, scroll_down(11, 5));
        assert_eq!(s.input_scroll, 6);

        // Scrolling back up reduces it, but never below zero.
        handle_mouse(&mut s, scroll_up(11, 5));
        assert_eq!(s.input_scroll, 3);
        handle_mouse(&mut s, scroll_up(11, 5));
        assert_eq!(s.input_scroll, 0);
        handle_mouse(&mut s, scroll_up(11, 5));
        assert_eq!(s.input_scroll, 0);

        // The output scroll is untouched by input-box wheel events.
        assert_eq!(s.scroll.offset, 0);

        // A wheel event outside the input box still scrolls the output (give
        // the output real content so it has somewhere to scroll).
        s.input_area = None;
        s.messages_area = Some((0, 0, 20, 8));
        s.scroll.update_dimensions(100, 8);
        handle_mouse(&mut s, scroll_up(3, 5)); // unsticks sticky-bottom
        let offset_after_up = s.scroll.offset;
        handle_mouse(&mut s, scroll_up(3, 5));
        assert!(s.scroll.offset < offset_after_up);
    }

    #[test]
    fn wheel_over_input_does_not_scroll_when_content_fits() {
        // Short input in a tall box: no overflow, so the wheel falls through
        // to the output scroll as before.
        let mut s = state();
        s.input = "hi".into();
        s.input_area = Some((0, 10, 20, 6));
        s.messages_area = Some((0, 0, 20, 8));
        s.scroll.update_dimensions(100, 8);
        handle_mouse(&mut s, scroll_up(12, 5)); // unsticks sticky-bottom
        let offset_after_up = s.scroll.offset;
        handle_mouse(&mut s, scroll_up(12, 5));
        assert_eq!(s.input_scroll, 0);
        assert!(s.scroll.offset < offset_after_up);
    }

    #[test]
    fn word_navigation_boundaries() {
        // "one two three  four": words at bytes 0..3, 4..7, 8..13, 15..19.
        let input = "one two three  four";
        // From the end, word_start_before lands at the start of "four" (15).
        assert_eq!(word_start_before(input, 17), 15);
        // From the middle of a word, it lands at that word's start.
        assert_eq!(word_start_before(input, 5), 4);
        // From the start of a word, it lands at the previous word's start.
        assert_eq!(word_start_before(input, 4), 0);
        // From a gap, it lands at the previous word's start.
        assert_eq!(word_start_before(input, 13), 8);
        // word_end_after from a word start lands at that word's end.
        assert_eq!(word_end_after(input, 0), 3);
        assert_eq!(word_end_after(input, 4), 7);
        assert_eq!(word_end_after(input, 8), 13);
        // From inside a word, lands at that word's end.
        assert_eq!(word_end_after(input, 5), 7);
        // At the very end there is no next word.
        assert_eq!(word_end_after(input, 19), 19);
        // Empty input is stable.
        assert_eq!(word_start_before("", 0), 0);
        assert_eq!(word_end_after("", 0), 0);
    }

    #[test]
    fn output_position_movement_wraps_rows() {
        use crate::tui::virtual_list::VItem;
        use ratatui::prelude::*;

        let mut s = state();
        s.virtual_list.set_committed(vec![
            VItem::new(Line::from("ab")),
            VItem::new(Line::from("cd")),
        ]);

        // Forward across a row boundary wraps to the next row's start.
        let p = output_pos_forward(&s, SelectionPoint::Output(0, 1));
        assert_eq!(p, SelectionPoint::Output(0, 2));
        let p = output_pos_forward(&s, SelectionPoint::Output(0, 2));
        assert_eq!(p, SelectionPoint::Output(1, 0));
        // Backward wraps to the previous row's end.
        let p = output_pos_backward(&s, SelectionPoint::Output(1, 0));
        assert_eq!(p, SelectionPoint::Output(0, 2));
        let p = output_pos_backward(&s, SelectionPoint::Output(0, 1));
        assert_eq!(p, SelectionPoint::Output(0, 0));
        // Clamping at the document edges.
        assert_eq!(
            output_pos_forward(&s, SelectionPoint::Output(1, 2)),
            SelectionPoint::Output(1, 2)
        );
        assert_eq!(
            output_pos_backward(&s, SelectionPoint::Output(0, 0)),
            SelectionPoint::Output(0, 0)
        );
        // Row movement keeps the column, clamped to the target row's length.
        assert_eq!(
            output_pos_row(&s, SelectionPoint::Output(0, 1), true),
            SelectionPoint::Output(1, 1)
        );
        assert_eq!(
            output_pos_row(&s, SelectionPoint::Output(1, 0), false),
            SelectionPoint::Output(0, 0)
        );
    }

    #[test]
    fn select_all_covers_whole_document() {
        use crate::tui::virtual_list::VItem;
        use ratatui::prelude::*;

        let mut s = state();
        s.virtual_list.set_committed(vec![
            VItem::new(Line::from("hello")),
            VItem::new(Line::from("world")),
        ]);
        select_all_output(&mut s);
        assert_eq!(s.selection_text().as_deref(), Some("hello\nworld"));

        // Empty document clears rather than building an empty selection.
        let mut s = state();
        select_all_output(&mut s);
        assert!(s.selection.is_none());

        // Input select-all covers the whole input.
        let mut s = state();
        s.input = "hi there".into();
        select_all_input(&mut s);
        assert_eq!(s.selection_text().as_deref(), Some("hi there"));
    }

    #[test]
    fn cmd_a_targets_input_only_when_cursor_is_there() {
        use crossterm::event::KeyModifiers as M;

        // Cursor in the input box (not streaming): Cmd+A selects the input
        // text, even when the input is empty (an empty selection, not the
        // output document).
        let mut s = state();
        s.input = "hello".into();
        handle_editing_key(&mut s, key_for(KeyCode::Char('a'), M::SUPER));
        assert_eq!(s.selection_text().as_deref(), Some("hello"));
        assert!(matches!(s.selection, Some(Selection { target: SelectionTarget::Input, .. })));

        // Empty input: Cmd+A stays in the input (zero-width selection, which
        // yields no text) rather than grabbing the output document.
        let mut s = state();
        handle_editing_key(&mut s, key_for(KeyCode::Char('a'), M::SUPER));
        assert!(matches!(s.selection, Some(Selection { target: SelectionTarget::Input, .. })));
        assert_eq!(s.selection_text(), None);

        // Streaming: the input is read-only (cursor isn't there), so Cmd+A
        // selects the output document.
        let mut s = state();
        s.is_streaming = true;
        s.virtual_list.set_committed(vec![crate::tui::virtual_list::VItem::new(
            ratatui::prelude::Line::from("streamed output"),
        )]);
        handle_editing_key(&mut s, key_for(KeyCode::Char('a'), M::SUPER));
        assert!(matches!(s.selection, Some(Selection { target: SelectionTarget::Output, .. })));
        assert_eq!(s.selection_text().as_deref(), Some("streamed output"));
    }

    #[test]
    fn shift_arrows_extend_the_right_target() {
        use crate::tui::virtual_list::VItem;
        use ratatui::prelude::*;
        use crossterm::event::KeyModifiers as M;

        let mut s = state();
        s.input = "hello".into();
        s.virtual_list.set_committed(vec![
            VItem::new(Line::from("world")),
        ]);

        // Shift+Right with no selection extends the *input* selection.
        handle_editing_key(&mut s, key_for(KeyCode::Right, M::SHIFT));
        assert_eq!(s.selection_text().as_deref(), Some("h"));
        assert!(matches!(s.selection, Some(Selection { target: SelectionTarget::Input, .. })));

        // Once a selection lives in the output, Shift+Right extends it there.
        s.selection = Some(Selection {
            target: SelectionTarget::Output,
            anchor: SelectionPoint::Output(0, 0),
            active: SelectionPoint::Output(0, 1),
            dragging: false,
        });
        handle_editing_key(&mut s, key_for(KeyCode::Right, M::SHIFT));
        assert_eq!(s.selection_text().as_deref(), Some("wo")); // bytes 0..2
        handle_editing_key(&mut s, key_for(KeyCode::Left, M::SHIFT));
        assert_eq!(s.selection_text().as_deref(), Some("w")); // bytes 0..1
    }

    #[test]
    fn ctrl_a_e_k_u_w_are_readline() {
        use crossterm::event::KeyModifiers as M;

        // Ctrl+A jumps to the start of the current line.
        let mut s = state();
        s.input = "hello\nworld".into();
        s.cursor_pos = 9;
        handle_editing_key(&mut s, key_for(KeyCode::Char('a'), M::CONTROL));
        assert_eq!(s.cursor_pos, 6);

        // Ctrl+E jumps to the end of the current line.
        handle_editing_key(&mut s, key_for(KeyCode::Char('e'), M::CONTROL));
        assert_eq!(s.cursor_pos, 11);

        // Ctrl+U deletes back to the start of the line (byte 6 here, the
        // char after the newline), leaving the rest of the line intact.
        s.cursor_pos = 9;
        handle_editing_key(&mut s, key_for(KeyCode::Char('u'), M::CONTROL));
        assert_eq!(s.input, "hello\nld");
        assert_eq!(s.cursor_pos, 6);

        // Ctrl+W deletes the word before the cursor.
        let mut s = state();
        s.input = "foo bar baz".into();
        s.cursor_pos = 11;
        handle_editing_key(&mut s, key_for(KeyCode::Char('w'), M::CONTROL));
        assert_eq!(s.input, "foo bar ");
        assert_eq!(s.cursor_pos, 8);

        // Ctrl+K deletes to the end of the line.
        let mut s = state();
        s.input = "foo bar baz".into();
        s.cursor_pos = 4;
        handle_editing_key(&mut s, key_for(KeyCode::Char('k'), M::CONTROL));
        assert_eq!(s.input, "foo ");
        assert_eq!(s.cursor_pos, 4);
    }

    #[test]
    fn grapheme_boundaries_skip_combining_marks() {
        // `e` + U+0301 (combining acute) + `b`: one grapheme cluster `é` (bytes
        // 0..3), then `b` (byte 3). Cursor movement must treat `é` as a unit.
        let input = "e\u{0301}b";
        // Moving forward from 0 lands past the whole cluster (byte 3), never
        // between `e` and the combining mark (byte 1).
        assert_eq!(next_grapheme_boundary(input, 0), 3);
        assert_eq!(next_grapheme_boundary(input, 3), input.len());
        // Moving backward from the end lands on the `b` cluster's start (byte 3).
        assert_eq!(prev_grapheme_boundary(input, input.len()), 3);
        // Moving backward again lands on the `é` cluster's start (byte 0).
        assert_eq!(prev_grapheme_boundary(input, 3), 0);
        // Moving backward from inside the `é` cluster snaps to its start too.
        assert_eq!(prev_grapheme_boundary(input, 1), 0);
        assert_eq!(prev_grapheme_boundary(input, 2), 0);
        // ASCII-only input behaves as before (1 byte = 1 grapheme).
        assert_eq!(next_grapheme_boundary("abc", 1), 2);
        assert_eq!(prev_grapheme_boundary("abc", 2), 1);
        assert_eq!(prev_grapheme_boundary("abc", 3), 2);
    }

    #[test]
    fn grapheme_backspace_removes_whole_cluster() {
        // Backspace on `e` + combining acute removes the whole `é` cluster, not
        // just the combining mark.
        use crossterm::event::KeyModifiers as M;
        let mut s = state();
        s.input = "xe\u{0301}y".into();
        s.cursor_pos = 4; // after the `é` cluster (bytes 0=x, 1..4=é, 4=y)
        handle_editing_key(&mut s, key_for(KeyCode::Backspace, M::NONE));
        assert_eq!(s.input, "xy");
        assert_eq!(s.cursor_pos, 1);
    }

    #[test]
    fn grapheme_arrows_move_by_cluster() {
        use crossterm::event::KeyModifiers as M;
        let mut s = state();
        // `ab` + `é` (combining) + `c`: bytes 0=a,1=b,2..5=é,5=c.
        s.input = "abe\u{0301}c".into();
        s.cursor_pos = 2; // between `b` and `é`
        // Right arrow skips the entire `é` cluster to byte 5.
        handle_editing_key(&mut s, key_for(KeyCode::Right, M::NONE));
        assert_eq!(s.cursor_pos, 5);
        // Left arrow returns to the cluster's start (byte 2), never mid-cluster.
        handle_editing_key(&mut s, key_for(KeyCode::Left, M::NONE));
        assert_eq!(s.cursor_pos, 2);
    }

    #[test]
    fn grapheme_word_move_treats_cluster_as_unit() {
        // A combining mark sticks to its base char: `foé bar` (é = e+◌́).
        // bytes: f0 o1 é2..5 (space)5 b6 a7 r8.
        let input = "foe\u{0301} bar";
        // From the end, the previous word starts at byte 6 ("bar").
        assert_eq!(word_start_before(input, input.len()), 6);
        // From inside `foé`, the word starts at 0.
        assert_eq!(word_start_before(input, 3), 0);
        // word_end_after from byte 0 stops at byte 5 (after `foé`, before space).
        assert_eq!(word_end_after(input, 0), 5);
        // From the space, the next word ends at byte 9.
        assert_eq!(word_end_after(input, 5), 9);
    }
}
