//! TUI widgets — header, footer, messages, input, status, tool calls, overlays.

pub mod diff_inline {
    //! Inline diff renderer: shows file changes with syntax-highlighted diff coloring.

    use crate::tui::theme::Theme;
    use ratatui::prelude::*;

    const MAX_DIFF_LINES: usize = 15;

    /// Render a tool result as an inline diff if it contains diff-like content.
    /// Returns None if the content doesn't look like a diff.
    pub fn render_diff_output(
        output: &str,
        tool_name: &str,
        theme: &Theme,
    ) -> Option<Vec<Line<'static>>> {
        let is_file_tool = matches!(
            tool_name,
            "Edit" | "Write" | "ApplyPatch" | "edit" | "write"
        );
        if !is_file_tool {
            return None;
        }

        // Check if output contains diff-like content
        let has_diff_markers = output.lines().any(|l| {
            (l.starts_with('+') && !l.starts_with("+++"))
                || (l.starts_with('-') && !l.starts_with("---"))
                || l.starts_with("@@")
                || l.starts_with("diff ")
        });

        if !has_diff_markers && output.lines().count() < 2 {
            return None; // Too short or no diff content — use default rendering
        }

        let mut lines = Vec::new();

        // Header
        let label = if has_diff_markers { "diff" } else { "content" };
        lines.push(Line::from(Span::styled(
            format!("    ┌─ {label}"),
            Style::default().fg(theme.border),
        )));

        // Content lines with diff coloring
        let content_lines: Vec<&str> = output.lines().take(MAX_DIFF_LINES).collect();
        let total = output.lines().count();

        for line in &content_lines {
            let (prefix_style, text_style) = if line.starts_with('+') && !line.starts_with("+++") {
                (
                    Style::default().fg(theme.diff_added),
                    Style::default().fg(theme.diff_added),
                )
            } else if line.starts_with('-') && !line.starts_with("---") {
                (
                    Style::default().fg(theme.diff_removed),
                    Style::default().fg(theme.diff_removed),
                )
            } else if line.starts_with("@@") {
                (
                    Style::default().fg(Color::Cyan),
                    Style::default().fg(Color::Cyan),
                )
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                (
                    Style::default().fg(theme.text_tertiary),
                    Style::default().fg(theme.text_tertiary),
                )
            } else {
                (
                    Style::default().fg(theme.border),
                    Style::default().fg(theme.text_tertiary),
                )
            };

            lines.push(Line::from(vec![
                Span::styled("    │ ", prefix_style),
                Span::styled(line.to_string(), text_style),
            ]));
        }

        if total > MAX_DIFF_LINES {
            lines.push(Line::from(Span::styled(
                format!("    │ ... ({} more lines)", total - MAX_DIFF_LINES),
                Style::default().fg(theme.text_ghost),
            )));
        }

        // Footer
        lines.push(Line::from(Span::styled(
            "    └─".to_string(),
            Style::default().fg(theme.border),
        )));

        Some(lines)
    }
}
pub mod footer {
    //! Footer bar: context-aware keybinding hints

    use crate::tui::theme::Theme;
    use ratatui::{prelude::*, widgets::Paragraph};

    pub fn render(
        f: &mut Frame,
        area: Rect,
        is_streaming: bool,
        side_panel_open: bool,
        side_panel_focused: bool,
        theme: &Theme,
    ) {
        let hints = if side_panel_focused {
            " j/k scroll | d/u page | Tab switch | r refresh | Esc back | ^B close".to_string()
        } else if is_streaming {
            " ^C cancel | PgUp/Dn scroll | ^B panel".to_string()
        } else if side_panel_open {
            " Enter send | ^B focus panel | Shift+Tab mode | /help | ^D exit".to_string()
        } else {
            " Enter send | Opt+Enter newline | ^B panel | Shift+Tab mode | /help | ^D exit"
                .to_string()
        };

        let footer = Paragraph::new(hints).style(theme.dimmed());
        f.render_widget(footer, area);
    }
}
pub mod graph {
    //! Graph visualization: memory nodes + relationships rendered as a node graph.
    //!
    //! Since tui-nodes requires ratatui 0.30 and we're on 0.29, this uses
    //! a custom renderer with Block widgets and canvas lines.

    use crate::tui::theme::Theme;
    use ratatui::{
        prelude::*,
        widgets::{Block, Borders, Clear, Paragraph, Wrap},
    };

    /// A node in the graph visualization.
    #[derive(Debug, Clone)]
    pub struct GraphNode {
        pub id: String,
        pub label: String,
        pub kind: NodeKind,
        pub x: u16,
        pub y: u16,
    }

    /// Edge between two nodes.
    #[derive(Debug, Clone)]
    pub struct GraphEdge {
        pub from: usize,
        pub to: usize,
        pub label: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum NodeKind {
        Memory,
        Topic,
        Session,
        LspServer,
        Tool,
    }

    impl NodeKind {
        pub fn color(&self) -> Color {
            match self {
                Self::Memory => Color::Cyan,
                Self::Topic => Color::Yellow,
                Self::Session => Color::Green,
                Self::LspServer => Color::Magenta,
                Self::Tool => Color::Blue,
            }
        }

        pub fn icon(&self) -> &'static str {
            match self {
                Self::Memory => "M",
                Self::Topic => "T",
                Self::Session => "S",
                Self::LspServer => "L",
                Self::Tool => "⚙",
            }
        }
    }

    /// Graph overlay state.
    #[derive(Debug, Clone)]
    pub struct GraphOverlayState {
        pub nodes: Vec<GraphNode>,
        pub edges: Vec<GraphEdge>,
        pub selected: usize,
        pub pan_x: i16,
        pub pan_y: i16,
    }

    impl Default for GraphOverlayState {
        fn default() -> Self {
            Self::new()
        }
    }

    impl GraphOverlayState {
        pub fn new() -> Self {
            Self {
                nodes: Vec::new(),
                edges: Vec::new(),
                selected: 0,
                pan_x: 0,
                pan_y: 0,
            }
        }

        /// Build from graph stats and memory data.
        pub fn from_memory_stats(stats: &crate::tui::widgets::graph::MemoryGraphData) -> Self {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();

            // Layout nodes in a grid pattern
            let cols = 6u16;
            let node_w = 14u16;
            let node_h = 3u16;

            // Memory nodes
            for (i, mem) in stats.memories.iter().enumerate() {
                let col = (i as u16) % cols;
                let row = (i as u16) / cols;
                nodes.push(GraphNode {
                    id: mem.id.clone(),
                    label: truncate_label(&mem.label, 12),
                    kind: NodeKind::Memory,
                    x: col * (node_w + 2) + 2,
                    y: row * (node_h + 1) + 2,
                });
            }

            let mem_count = nodes.len();

            // Topic nodes (below memories)
            let topic_y_offset = ((mem_count as u16 / cols) + 1) * (node_h + 1) + 3;
            for (i, topic) in stats.topics.iter().enumerate() {
                let col = (i as u16) % cols;
                nodes.push(GraphNode {
                    id: topic.clone(),
                    label: truncate_label(topic, 12),
                    kind: NodeKind::Topic,
                    x: col * (node_w + 2) + 2,
                    y: topic_y_offset,
                });
            }

            // LSP server nodes (right side)
            let lsp_x = (cols) * (node_w + 2) + 4;
            for (i, server) in stats.lsp_servers.iter().enumerate() {
                nodes.push(GraphNode {
                    id: format!("lsp-{server}"),
                    label: truncate_label(server, 12),
                    kind: NodeKind::LspServer,
                    x: lsp_x,
                    y: (i as u16) * (node_h + 1) + 2,
                });
            }

            // Edges: memory → topic (simple relationships)
            for (mem_idx, mem) in stats.memories.iter().enumerate() {
                for topic in &mem.topics {
                    if let Some(topic_idx) = stats.topics.iter().position(|t| t == topic) {
                        edges.push(GraphEdge {
                            from: mem_idx,
                            to: mem_count + topic_idx,
                            label: "tagged".to_string(),
                        });
                    }
                }
            }

            Self {
                nodes,
                edges,
                selected: 0,
                pan_x: 0,
                pan_y: 0,
            }
        }

        pub fn select_next(&mut self) {
            if !self.nodes.is_empty() {
                self.selected = (self.selected + 1) % self.nodes.len();
            }
        }

        pub fn select_prev(&mut self) {
            if !self.nodes.is_empty() {
                self.selected = self.selected.checked_sub(1).unwrap_or(self.nodes.len() - 1);
            }
        }
    }

    /// Data needed to build the graph.
    #[derive(Debug, Clone, Default)]
    pub struct MemoryGraphData {
        pub memories: Vec<MemoryNode>,
        pub topics: Vec<String>,
        pub lsp_servers: Vec<String>,
        pub total_sessions: usize,
    }

    #[derive(Debug, Clone)]
    pub struct MemoryNode {
        pub id: String,
        pub label: String,
        pub topics: Vec<String>,
    }

    /// Render the graph overlay.
    pub fn render(f: &mut Frame, state: &GraphOverlayState, theme: &Theme) {
        let area = f.area();

        // Centered overlay (90% of screen)
        let overlay_area = centered_rect(90, 90, area);

        // Clear background
        f.render_widget(Clear, overlay_area);

        let block = Block::default()
            .title(" Memory Graph (↑↓ select | ←→ pan | Esc close) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .style(Style::default().bg(theme.bg));

        let inner = block.inner(overlay_area);
        f.render_widget(block, overlay_area);

        if state.nodes.is_empty() {
            let msg = Paragraph::new("  No graph data. Use graph memory to populate nodes.")
                .style(Style::default().fg(theme.dim));
            f.render_widget(msg, inner);
            return;
        }

        // Render nodes
        for (i, node) in state.nodes.iter().enumerate() {
            let nx = (node.x as i16 + state.pan_x) as u16;
            let ny = (node.y as i16 + state.pan_y) as u16;

            // Skip if out of bounds
            if nx >= inner.width || ny >= inner.height {
                continue;
            }

            let abs_x = inner.x + nx;
            let abs_y = inner.y + ny;
            let node_width = 14u16.min(inner.right().saturating_sub(abs_x));
            let node_height = 3u16.min(inner.bottom().saturating_sub(abs_y));

            if node_width < 4 || node_height < 1 {
                continue;
            }

            let node_area = Rect::new(abs_x, abs_y, node_width, node_height);

            let is_selected = i == state.selected;
            let border_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(node.kind.color())
            };

            let node_block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style);

            let label = format!("{} {}", node.kind.icon(), node.label);
            let content = Paragraph::new(label)
                .style(Style::default().fg(node.kind.color()))
                .wrap(Wrap { trim: true });

            f.render_widget(node_block, node_area);
            let content_area = Rect::new(
                node_area.x + 1,
                node_area.y + 1,
                node_area.width.saturating_sub(2),
                node_area.height.saturating_sub(2),
            );
            if content_area.width > 0 && content_area.height > 0 {
                f.render_widget(content, content_area);
            }
        }

        // Render edge indicators (simple: show connection lines as dashes between nodes)
        for edge in &state.edges {
            if edge.from >= state.nodes.len() || edge.to >= state.nodes.len() {
                continue;
            }
            let from = &state.nodes[edge.from];
            let to = &state.nodes[edge.to];

            let fx = (from.x as i16 + state.pan_x + 7) as u16; // center of from node
            let fy = (from.y as i16 + state.pan_y + 2) as u16; // bottom of from node
            let _tx = (to.x as i16 + state.pan_x + 7) as u16;
            let _ty = (to.y as i16 + state.pan_y) as u16;

            // Draw a simple vertical connector dot if nodes are vertically aligned
            if fx < inner.width && fy < inner.height && fy > 0 {
                let abs_x = inner.x + fx;
                let abs_y = inner.y + fy;
                if abs_x < inner.right() && abs_y < inner.bottom() {
                    let connector = Paragraph::new("│").style(Style::default().fg(Color::DarkGray));
                    f.render_widget(connector, Rect::new(abs_x, abs_y, 1, 1));
                }
            }
        }

        // Selected node details at the bottom
        if state.selected < state.nodes.len() {
            let selected_node = &state.nodes[state.selected];
            let detail = format!(
                " Selected: {} ({:?}) — {}",
                selected_node.label, selected_node.kind, selected_node.id
            );
            let detail_area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
            let detail_widget =
                Paragraph::new(detail).style(Style::default().fg(theme.fg).bg(theme.bg));
            f.render_widget(detail_widget, detail_area);
        }
    }

    fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(area);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }

    fn truncate_label(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            format!("{}…", &s[..max - 1])
        }
    }
}
pub mod header {
    //! Header bar: model | mode | tokens | cost | session

    use crate::tui::{app::AppState, theme::Theme};
    use ratatui::{prelude::*, widgets::Paragraph};

    pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
        // Estimate cost from tokens if provider didn't report it
        let estimated_cost = if state.cost_usd > 0.0 {
            state.cost_usd
        } else {
            estimate_cost(&state.model, state.input_tokens, state.output_tokens)
        };

        let cost_str = if estimated_cost > 0.0 {
            format!("${:.4}", estimated_cost)
        } else {
            "$0".into()
        };

        let tokens_str = format!(
            "{}in/{}out",
            format_tokens(state.input_tokens),
            format_tokens(state.output_tokens),
        );

        let mode_str = state.permission_mode.label();

        let text = Line::from(vec![
            Span::styled(
                " abstract",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | ", Style::default().fg(theme.dim)),
            Span::styled(&state.model, Style::default().fg(theme.fg)),
            Span::styled(" | ", Style::default().fg(theme.dim)),
            Span::styled(mode_str, mode_style(state.permission_mode, theme)),
            Span::styled(" | ", Style::default().fg(theme.dim)),
            Span::styled(&tokens_str, Style::default().fg(theme.dim)),
            Span::styled(" | ", Style::default().fg(theme.dim)),
            Span::styled(&cost_str, Style::default().fg(theme.fg)),
            Span::styled(" | ", Style::default().fg(theme.dim)),
            // Span::styled(&state.session_id, Style::default().fg(theme.dim)),
        ]);

        let header = Paragraph::new(text).style(theme.header_style());
        f.render_widget(header, area);
    }

    fn mode_style(mode: crate::tui::app::PermissionMode, theme: &Theme) -> Style {
        use crate::tui::app::PermissionMode;
        match mode {
            PermissionMode::Auto => Style::default().fg(theme.success),
            PermissionMode::Plan => Style::default().fg(Color::Cyan),
            PermissionMode::Editor => Style::default().fg(Color::Blue),
            PermissionMode::Bypass => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            PermissionMode::BypassAlert => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
        }
    }

    fn format_tokens(n: u64) -> String {
        if n >= 1_000_000 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else if n >= 1_000 {
            format!("{:.1}K", n as f64 / 1_000.0)
        } else {
            n.to_string()
        }
    }

    /// Estimate USD cost from token counts based on model pricing.
    fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
        // Pricing per 1M tokens (input, output)
        let (input_per_m, output_per_m) = match model {
            m if m.contains("gpt-5.3") => (2.0, 10.0),
            m if m.contains("gpt-5") => (2.0, 10.0),
            m if m.contains("gpt-4o") => (2.50, 10.0),
            m if m.contains("gpt-4-turbo") => (10.0, 30.0),
            m if m.starts_with("o1") => (15.0, 60.0),
            m if m.starts_with("o3") => (10.0, 40.0),
            m if m.contains("opus") => (15.0, 75.0),
            m if m.contains("sonnet") => (3.0, 15.0),
            m if m.contains("haiku") => (0.25, 1.25),
            m if m.contains("gemini-2.0-flash") => (0.075, 0.30),
            m if m.contains("gemini") => (1.25, 5.0),
            _ => (2.0, 10.0), // default estimate
        };

        let input_cost = (input_tokens as f64 / 1_000_000.0) * input_per_m;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_per_m;
        input_cost + output_cost
    }
}
pub mod input {
    //! Input widget: multi-line textarea with wrapping and a rounded border.
    //!
    //! A single layout model ([`layout`]) is the source of truth for rendering,
    //! cursor placement, and mouse hit-testing, so the cursor always lands where
    //! the user expects no matter how the text was edited.

    use crate::tui::{app::AppState, theme::Theme};
    use ratatui::{
        prelude::*,
        widgets::{Block, Borders, BorderType, Paragraph},
    };
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    /// Minimum/maximum number of content lines (excluding the border).
    const MIN_CONTENT_LINES: u16 = 2;
    const MAX_CONTENT_LINES: u16 = 10;
    /// Height consumed by the rounded border (top + bottom).
    const BORDER_LINES: u16 = 2;
    /// Prefix of the first row when editable.
    const PROMPT: &str = "> ";
    /// Prefix of continuation rows and all rows after the first logical line.
    const CONTINUATION: &str = "  ";

    /// One visual row of the editor. `start..end` is the byte range of the
    /// input string that this row renders (excluding its prefix). Rows are
    /// contiguous in display space, skipping the `\n` bytes of the input.
    pub(crate) struct VisualRow {
        /// Whether this is the very first row (it gets the `> ` prompt prefix).
        pub is_first: bool,
        pub start: usize,
        pub end: usize,
    }

    impl VisualRow {
        fn prefix<'a>(&self, prompt: &'a str) -> &'a str {
            if self.is_first {
                prompt
            } else {
                CONTINUATION
            }
        }

        fn prefix_len(&self, prompt: &str) -> usize {
            self.prefix(prompt).len()
        }
    }

    pub fn render(f: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
        let border_color = if state.side_panel_focused {
            theme.dim
        } else {
            theme.border_strong
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.input_bg));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Remember where the editable box is so mouse clicks can be mapped back
        // to a character position (see `char_pos_at_click`).
        state.input_area = Some((area.x, area.y, area.width, area.height));

        let prompt = if state.is_streaming { CONTINUATION } else { PROMPT };
        let width = inner.width as usize;
        if width < 4 {
            state.input_scroll = 0;
            return;
        }
        let usable = width.saturating_sub(prompt.len());

        let rows = layout(&state.input, usable);

        // Scroll so the cursor row is visible.
        let (cursor_row, cursor_col) = cursor_in_rows(&rows, &state.input, state.cursor_pos);
        let scroll = if cursor_row as u16 >= inner.height {
            cursor_row as u16 - inner.height + 1
        } else {
            0
        };
        state.input_scroll = scroll;

        let lines: Vec<Line> = rows
            .iter()
            .map(|row| {
                Line::raw(format!(
                    "{}{}",
                    row.prefix(prompt),
                    &state.input[row.start..row.end]
                ))
            })
            .collect();
        let widget = Paragraph::new(lines)
            .style(Style::default().fg(theme.fg).bg(theme.input_bg))
            .scroll((scroll, 0));
        f.render_widget(widget, inner);

        if !state.is_streaming {
            let cx = inner.x + cursor_col as u16;
            let cy = inner.y + (cursor_row as u16).saturating_sub(scroll);
            f.set_cursor_position((
                cx.min(inner.right().saturating_sub(1)),
                cy.min(inner.bottom().saturating_sub(1)),
            ));
        }
    }

    /// Desired total height for the input area (content + border).
    pub fn desired_height(input: &str, width: u16) -> u16 {
        if width < 4 {
            return MIN_CONTENT_LINES + BORDER_LINES;
        }
        let usable = (width as usize).saturating_sub(4);
        let lines = visual_lines(input, PROMPT, usable);
        (lines.len() as u16).clamp(MIN_CONTENT_LINES, MAX_CONTENT_LINES) + BORDER_LINES
    }

    /// Build the visual rows of `input`, the single source of truth shared by
    /// rendering, cursor placement, and click hit-testing.
    pub(crate) fn layout(input: &str, usable: usize) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        let mut seg_start = 0;
        for (line_index, line) in input.split('\n').enumerate() {
            let seg_end = seg_start + line.len();
            for (chunk_index, (chunk_start, chunk_end)) in wrap_segment(line, usable).into_iter().enumerate()
            {
                rows.push(VisualRow {
                    is_first: line_index == 0 && chunk_index == 0,
                    start: seg_start + chunk_start,
                    end: seg_start + chunk_end,
                });
            }
            seg_start = seg_end + 1; // skip the '\n' byte
        }
        if rows.is_empty() {
            rows.push(VisualRow {
                is_first: true,
                start: 0,
                end: 0,
            });
        }
        rows
    }

    /// Wrap one logical line (no `\n`) into `(start, end)` byte ranges of at
    /// most `usable` display columns, breaking at spaces when possible.
    fn wrap_segment(line: &str, usable: usize) -> Vec<(usize, usize)> {
        if line.is_empty() {
            return vec![(0, 0)];
        }
        if usable == 0 {
            return vec![(0, line.len())];
        }
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < line.len() {
            let mut width = 0usize;
            let mut end = start;
            let mut last_space_end = None;
            for (i, ch) in line[start..].char_indices() {
                let w = ch.width().unwrap_or(0);
                if width + w > usable {
                    break;
                }
                width += w;
                end = start + i + ch.len_utf8();
                if ch == ' ' {
                    last_space_end = Some(end);
                }
            }
            // Break after the last space that fit (keeping it at the end of
            // this row) so the next row starts with a real word.
            if let Some(space_end) = last_space_end {
                if space_end < end && space_end > start {
                    end = space_end;
                }
            }
            if end == start {
                // A single char wider than the whole row: emit it alone so we
                // always make progress.
                let ch = line[start..].chars().next().unwrap();
                end = start + ch.len_utf8();
            }
            chunks.push((start, end));
            start = end;
        }
        chunks
    }

    /// Map a byte position in `input` to its (row, column) in display space.
    fn cursor_in_rows(rows: &[VisualRow], input: &str, pos: usize) -> (usize, usize) {
        let pos = pos.min(input.len());
        let mut result = (0usize, rows.first().map_or(0, |row| row.prefix_len(PROMPT)));
        for (row_index, row) in rows.iter().enumerate() {
            if pos < row.start {
                break;
            }
            if pos <= row.end {
                let content_width = input[row.start..pos].width();
                result = (row_index, row.prefix_len(PROMPT) + content_width);
                // Strictly inside this row, or on an empty row: no later row
                // can also contain `pos`.
                if pos < row.end || row.start == row.end {
                    break;
                }
            }
        }
        result
    }

    /// Map a click at display (row, col) back to a byte position in `input` —
    /// the inverse of [`cursor_in_rows`]. Out-of-range clicks clamp to the
    /// nearest valid position.
    pub(crate) fn char_pos_at_click(
        input: &str,
        prompt: &str,
        usable: usize,
        row: usize,
        col: usize,
    ) -> usize {
        let rows = layout(input, usable);
        if rows.is_empty() {
            return 0;
        }
        let row_index = row.min(rows.len() - 1);
        let target = &rows[row_index];
        let col_in = col.saturating_sub(target.prefix_len(prompt));
        let mut width = 0;
        for (i, ch) in input[target.start..target.end].char_indices() {
            let w = ch.width().unwrap_or(0);
            if width + w > col_in {
                return target.start + i;
            }
            width += w;
        }
        target.end
    }

    /// Build the visual lines as they appear on screen.
    fn visual_lines(input: &str, prompt: &str, usable: usize) -> Vec<String> {
        layout(input, usable)
            .into_iter()
            .map(|row| format!("{}{}", row.prefix(prompt), &input[row.start..row.end]))
            .collect()
    }

    /// Render the fuzzy `/` command selector popup anchored above the input.
    pub fn render_command_selector(
        f: &mut Frame,
        input_area: Rect,
        state: &AppState,
        theme: &Theme,
    ) {
        use crate::tui::app::CommandMatch;
        use ratatui::widgets::{Clear, List, ListItem, ListState};

        let Some(sel) = &state.command_selector else { return };
        if sel.matches.is_empty() {
            return;
        }

        let popup_width = 48.min(input_area.width.saturating_sub(4)).max(16);
        let rows = (sel.matches.len() as u16).min(8);
        let popup_height = rows + BORDER_LINES;
        let x = input_area.x + 1;
        // Place directly above the input; if there isn't room, pin to the top.
        let y = input_area.y.saturating_sub(popup_height);
        let area = Rect {
            x,
            y,
            width: popup_width,
            height: popup_height,
        };

        f.render_widget(Clear, area);

        let mut list_state = ListState::default();
        list_state.select(Some(sel.selected.min(sel.matches.len() - 1)));

        let items: Vec<ListItem> = sel
            .matches
            .iter()
            .map(|m: &CommandMatch| {
                let name = Span::styled(
                    format!("/{}", m.name),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                );
                let desc = Span::styled(
                    format!("  {}", m.description),
                    Style::default().fg(theme.dim),
                );
                ListItem::new(Line::from(vec![name, desc]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border_strong))
                    .style(Style::default().bg(theme.bg_raised)),
            )
            .highlight_style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::REVERSED),
            )
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, area, &mut list_state);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rows(input: &str, usable: usize) -> Vec<(bool, usize, usize)> {
            layout(input, usable)
                .into_iter()
                .map(|row| (row.is_first, row.start, row.end))
                .collect()
        }

        fn cursor(input: &str, pos: usize) -> (usize, usize) {
            cursor_in_rows(&layout(input, 20), input, pos)
        }

        #[test]
        fn wraps_long_words_and_breaks_at_spaces() {
            // Hard break with no space that fits.
            assert_eq!(wrap_segment("hello world", 5), vec![(0, 5), (5, 6), (6, 11)]);
            // The space stays at the end of the wrapped row.
            assert_eq!(wrap_segment("hello world", 6), vec![(0, 6), (6, 11)]);
            // Fits entirely.
            assert_eq!(wrap_segment("hello", 5), vec![(0, 5)]);
            assert_eq!(wrap_segment("", 5), vec![(0, 0)]);
        }

        #[test]
        fn layout_multi_line_and_empty() {
            assert_eq!(rows("ab\ncd", 20), vec![(true, 0, 2), (false, 3, 5)]);
            assert_eq!(rows("", 20), vec![(true, 0, 0)]);
            assert_eq!(rows("ab\n", 20), vec![(true, 0, 2), (false, 3, 3)]);
            assert_eq!(rows("hello world", 6), vec![(true, 0, 6), (false, 6, 11)]);
        }

        #[test]
        fn cursor_position_byte_boundaries() {
            // Cursor before/after the newline.
            assert_eq!(cursor("ab\ncd", 2), (0, 4));
            assert_eq!(cursor("ab\ncd", 3), (1, 2));
            // Multi-byte char: positions stay on char boundaries.
            assert_eq!(cursor("héllo", 1), (0, 3)); // after 'h'
            assert_eq!(cursor("héllo", 6), (0, 7)); // at end (h + é + lll + o)
            // Trailing newline puts the cursor on the empty line.
            assert_eq!(cursor("ab\n", 3), (1, 2));
            assert_eq!(cursor("ab\n", 2), (0, 4));
            // Empty input.
            assert_eq!(cursor("", 0), (0, 2));
        }

        #[test]
        fn cursor_position_wrapped_lines() {
            // Cursor at the wrap point renders at the start of the continuation.
            assert_eq!(cursor("hello world", 6), (1, 2));
            assert_eq!(cursor("hello world", 5), (0, 7)); // after 'hello'
            assert_eq!(cursor("hello world", 11), (1, 7)); // after 'world'
        }

        #[test]
        fn click_maps_to_char_and_round_trips() {
            let input = "hello world";
            let usable = 6;
            // Prompt column -> start of first row.
            assert_eq!(char_pos_at_click(input, "> ", usable, 0, 0), 0);
            assert_eq!(char_pos_at_click(input, "> ", usable, 0, 2), 0);
            // Inside the first row.
            assert_eq!(char_pos_at_click(input, "> ", usable, 0, 7), 5); // before 'o'
            // End of a wrapped row -> start of the continuation.
            assert_eq!(char_pos_at_click(input, "> ", usable, 0, 8), 6);
            assert_eq!(char_pos_at_click(input, "> ", usable, 1, 2), 6);
            // Beyond the text clamps to the end.
            assert_eq!(char_pos_at_click(input, "> ", usable, 0, 30), 6);
            assert_eq!(char_pos_at_click(input, "> ", usable, 5, 30), 11);
            // Multi-byte chars map to byte offsets on char boundaries.
            assert_eq!(char_pos_at_click("héllo", "> ", 20, 0, 3), 1); // after 'h'
            assert_eq!(char_pos_at_click("héllo", "> ", 20, 0, 4), 3); // after 'é'
            // Clicking on the prompt of a later line -> start of that line.
            assert_eq!(char_pos_at_click("ab\ncd", "> ", 20, 1, 0), 3);
        }

        #[test]
        fn visual_lines_match_layout() {
            assert_eq!(visual_lines("hello world", "> ", 6), vec!["> hello ", "  world"]);
            assert_eq!(visual_lines("ab\ncd", "> ", 20), vec!["> ab", "  cd"]);
            assert_eq!(visual_lines("", "> ", 20), vec!["> "]);
        }
    }
}
pub mod messages {
    //! Messages widget: virtualized scrollable conversation.
    //!
    //! Only renders visible items. Committed turns are pre-built once;
    //! streaming content is rebuilt every frame (only a few lines).

    use crate::tui::{
        app::{AppState, TurnRole},
        theme::Theme,
        virtual_list::VItem,
        widgets::tool_call::render_tool_call,
    };
    use ratatui::prelude::*;

    pub fn render(f: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
        let width = area.width.saturating_sub(4);

        // Rebuild committed items only when dirty or width changed
        if state.messages_dirty || state.virtual_list.width_changed(width) {
            let committed = build_committed_lines(&state.turns, theme, width, state.frame_count);
            state.virtual_list.set_committed(committed);
            state.virtual_list.set_width(width);
            state.messages_dirty = false;
        }

        // Streaming items — rebuilt every frame (cheap, only a few lines)
        let streaming = build_streaming_lines(state, theme, width);
        state.virtual_list.set_streaming(streaming);

        // Update viewport
        state.virtual_list.set_viewport(area.height);

        // Sync scroll state
        state
            .scroll
            .update_dimensions(state.virtual_list.total_height(), area.height);
        state.virtual_list.scroll_offset = state.scroll.effective_offset();
        state.virtual_list.sticky_bottom = state.scroll.sticky_bottom;

        // Render visible items to buffer
        state.virtual_list.render(area, f.buffer_mut());
    }

    /// Build lines for all committed turns (cached, not rebuilt per frame).
    fn build_committed_lines(
        turns: &[crate::tui::app::Turn],
        theme: &Theme,
        width: u16,
        frame_count: u64,
    ) -> Vec<VItem> {
        let mut items = Vec::new();

        for turn in turns {
            match turn.role {
                TurnRole::User => {
                    let wrapped = wrap_text(&turn.content, width as usize);
                    for (i, wline) in wrapped.iter().enumerate() {
                        let prefix = if i == 0 { "> " } else { "  " };
                        items.push(VItem::new(Line::from(vec![
                            Span::styled(prefix, theme.accent_style()),
                            Span::styled(wline.clone(), Style::default().fg(theme.user_msg)),
                        ])));
                    }
                    items.push(VItem::new(Line::default()));
                }
                TurnRole::Assistant => {
                    for tool in &turn.tools {
                        for line in render_tool_call(tool, theme, frame_count) {
                            items.push(VItem::new(line));
                        }
                    }
                    if !turn.content.is_empty() {
                        let md_lines = crate::tui::markdown::render_markdown(&turn.content, width);
                        for md_line in md_lines {
                            let mut spans = vec![Span::raw("  ")];
                            spans.extend(md_line.spans);
                            items.push(VItem::new(Line::from(spans)));
                        }
                    }
                    items.push(VItem::new(Line::default()));
                }
                TurnRole::System => {
                    let wrapped = wrap_text(&turn.content, width as usize);
                    for wline in &wrapped {
                        items.push(VItem::new(Line::from(Span::styled(
                            format!("  [system] {}", wline),
                            theme.dimmed(),
                        ))));
                    }
                    items.push(VItem::new(Line::default()));
                }
            }
        }

        items
    }

    /// Build lines for active streaming content (rebuilt every frame — cheap).
    fn build_streaming_lines(state: &AppState, theme: &Theme, width: u16) -> Vec<VItem> {
        let mut items = Vec::new();

        if !state.is_streaming {
            return items;
        }

        // Active tool calls
        for tool in &state.active_tools {
            for line in render_tool_call(tool, theme, state.frame_count) {
                items.push(VItem::new(line));
            }
        }

        // Thinking indicator
        if !state.streaming_thinking.is_empty() {
            items.push(VItem::new(Line::from(Span::styled(
                "  thinking...",
                Style::default().fg(theme.thinking),
            ))));
        }

        // Streaming text
        if !state.streaming_text.is_empty() {
            let md_lines = crate::tui::markdown::render_markdown(&state.streaming_text, width);
            for md_line in md_lines {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(md_line.spans);
                items.push(VItem::new(Line::from(spans)));
            }
        }

        // Cursor blink
        if state.streaming_text.is_empty() && state.active_tools.is_empty() {
            let dot = if state.frame_count % 8 < 4 {
                "▊"
            } else {
                " "
            };
            items.push(VItem::new(Line::from(Span::styled(
                format!("  {dot}"),
                theme.accent_style(),
            ))));
        }

        items
    }

    /// Word-wrap text.
    fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![text.to_string()];
        }
        let mut result = Vec::new();
        for line in text.lines() {
            if line.len() <= max_width {
                result.push(line.to_string());
            } else {
                let mut remaining = line;
                while remaining.len() > max_width {
                    let break_at = remaining[..max_width].rfind(' ').unwrap_or(max_width);
                    let break_at = if break_at == 0 { max_width } else { break_at };
                    result.push(remaining[..break_at].to_string());
                    remaining = remaining[break_at..].trim_start();
                }
                if !remaining.is_empty() {
                    result.push(remaining.to_string());
                }
            }
        }
        if result.is_empty() {
            result.push(String::new());
        }
        result
    }
}
pub mod overlay {
    //! Modal overlays: help, permission, recovery.

    use crate::tui::{
        app::{AppState, ModelPickerState, Overlay, PermissionOverlay, RecoveryOverlay},
        theme::Theme,
    };
    use ratatui::{
        prelude::*,
        widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    };

    pub fn render(f: &mut Frame, state: &AppState, theme: &Theme) {
        match &state.overlay {
            Overlay::None => {}
            Overlay::Help => render_help(f, theme),
            Overlay::Permission(p) => render_permission(f, p, theme),
            Overlay::Recovery(r) => render_recovery(f, r, theme),
            Overlay::ModelPicker(p) => render_model_picker(f, p, theme),
            Overlay::Graph(g) => super::graph::render(f, g, theme),
        }
    }

    fn centered_rect(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - height_pct) / 2),
                Constraint::Percentage(height_pct),
                Constraint::Percentage((100 - height_pct) / 2),
            ])
            .split(area);
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - width_pct) / 2),
                Constraint::Percentage(width_pct),
                Constraint::Percentage((100 - width_pct) / 2),
            ])
            .split(vert[1])[1]
    }

    fn render_help(f: &mut Frame, theme: &Theme) {
        let area = centered_rect(f.area(), 60, 60);
        f.render_widget(Clear, area);

        let help_text = vec![
            Line::from(Span::styled(
                "Commands",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from("  /help        Show this help"),
            Line::from("  /clear       Clear conversation"),
            Line::from("  /cost        Show usage and cost"),
            Line::from("  /model       Switch provider/model"),
            Line::from("  /memory      Memory info"),
            Line::from("  /sessions    Session info"),
            Line::from("  /diff        Open git diff panel"),
            Line::from("  /files       Open file tree panel"),
            Line::from("  /panel       Toggle side panel"),
            Line::from("  /graph       Show memory graph"),
            Line::from("  /undo        Undo last file change"),
            Line::from("  /rewind      Remove last assistant turn"),
            Line::from("  /compact     Context compaction info"),
            Line::from("  /exit        Exit"),
            Line::default(),
            Line::from(Span::styled(
                "Keys",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from("  Enter        Send message"),
            Line::from("  Ctrl+C       Cancel / clear / quit"),
            Line::from("  Ctrl+D       Exit"),
            Line::from("  Ctrl+B       Toggle side panel"),
            Line::from("  Shift+Tab    Cycle permission mode"),
            Line::from("  Tab          Switch panel tabs"),
            Line::from("  PgUp/PgDn    Scroll messages"),
            Line::from("  Ctrl+↑↓     Scroll side panel"),
            Line::from("  Up/Down      Scroll (empty) / history"),
            Line::from("  Esc          Close overlay"),
            Line::default(),
            Line::from(Span::styled(
                "Modes (Shift+Tab)",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from("  Auto         Ask for permissions"),
            Line::from("  Plan         Read-only, no execution"),
            Line::from("  Editor       All except shell commands"),
            Line::from("  Bypass       All permissions bypassed"),
            Line::from("  Bypass+Alert Bypass + notify on shell"),
            Line::default(),
            Line::from(Span::styled("Press Esc to close", theme.dimmed())),
        ];

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .style(Style::default().bg(theme.bg));

        let help = Paragraph::new(help_text)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(help, area);
    }

    fn render_permission(f: &mut Frame, p: &PermissionOverlay, theme: &Theme) {
        // Use larger area to prevent overflow on small terminals
        let area = centered_rect(f.area(), 75, 55);
        f.render_widget(Clear, area);

        let options = ["Allow once", "Allow for session", "Always allow", "Deny"];
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled(
                format!("  Tool: {}", p.tool_name),
                theme.warning_style().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(format!("  {}", p.description), theme.text())),
            Line::default(),
        ];

        for (i, opt) in options.iter().enumerate() {
            let style = if i == p.selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                theme.text()
            };
            let marker = if i == p.selected { ">" } else { " " };
            lines.push(Line::from(Span::styled(
                format!("    {marker} {opt}"),
                style,
            )));
        }

        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  Up/Down select, Enter confirm, Esc deny",
            theme.dimmed(),
        )));
        lines.push(Line::default());

        let block = Block::default()
            .title(" Permission Required ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.warning))
            .style(Style::default().bg(theme.bg));

        let widget = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
        f.render_widget(widget, area);
    }

    fn render_model_picker(f: &mut Frame, p: &ModelPickerState, theme: &Theme) {
        let area = centered_rect(f.area(), 60, 70);
        f.render_widget(Clear, area);

        let mut list_state = ListState::default();
        list_state.select(Some(p.selected.min(p.entries.len().saturating_sub(1))));

        let items: Vec<ListItem> = p
            .entries
            .iter()
            .map(|(provider, model)| {
                let id = crate::providers::display_model_id(provider, model);
                let marker = if id == p.current { " ●" } else { "  " };
                let label = format!("{marker} {id}");
                ListItem::new(label)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Provider / Model (↑↓ select, Enter switch, Esc close) ")
                    .borders(Borders::ALL)
                    .border_style(theme.border_style())
                    .style(Style::default().bg(theme.bg)),
            )
            .highlight_style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">");

        f.render_stateful_widget(list, area, &mut list_state);
    }

    fn render_recovery(f: &mut Frame, r: &RecoveryOverlay, theme: &Theme) {
        let area = centered_rect(f.area(), 75, 55);
        f.render_widget(Clear, area);

        let mut lines = vec![
            Line::from(Span::styled(
                "Provider error",
                theme.error_style().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(&r.error_msg, theme.dimmed())),
            Line::default(),
        ];

        for (i, opt) in r.options.iter().enumerate() {
            let style = if i == r.selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                theme.text()
            };
            let marker = if i == r.selected { ">" } else { " " };
            lines.push(Line::from(Span::styled(format!("  {marker} {opt}"), style)));
        }

        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Up/Down to select, Enter to confirm, Esc to skip",
            theme.dimmed(),
        )));

        let block = Block::default()
            .title(" Provider Error ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.error))
            .style(Style::default().bg(theme.bg));

        let widget = Paragraph::new(lines).block(block);
        f.render_widget(widget, area);
    }
}
pub mod side_panel {
    //! Side panel: tabbed view with git diff and file tree.

    use crate::tui::{
        app::{AppState, SidePanelTab},
        theme::Theme,
    };
    use ratatui::{
        prelude::*,
        widgets::{Block, Borders, Paragraph, Tabs, Wrap},
    };

    pub fn render(f: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
        let border_color = if state.side_panel_focused {
            theme.accent
        } else {
            theme.dim
        };
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.bg));

        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 3 || inner.width < 10 {
            return;
        }

        // Tab bar
        let tabs = Tabs::new(vec!["Git Diff", "Files"])
            .select(match state.side_panel_tab {
                SidePanelTab::GitDiff => 0,
                SidePanelTab::FileTree => 1,
            })
            .style(Style::default().fg(theme.dim))
            .highlight_style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(" | ");

        let tab_area = Rect { height: 1, ..inner };
        f.render_widget(tabs, tab_area);

        let content_area = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };

        // Content based on tab
        let content = match state.side_panel_tab {
            SidePanelTab::GitDiff => render_diff(&state.side_panel_diff, content_area.width),
            SidePanelTab::FileTree => render_tree(&state.side_panel_tree, theme),
        };

        // Update side panel scroll
        let total_lines = content.len() as u16;
        state
            .side_panel_scroll
            .update_dimensions(total_lines, content_area.height);
        let scroll = state.side_panel_scroll.effective_offset();

        let paragraph = Paragraph::new(content)
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, content_area);
    }

    fn render_diff(diff_text: &str, _width: u16) -> Vec<Line<'static>> {
        if diff_text.is_empty() {
            return vec![Line::from(Span::styled(
                "  No changes",
                Style::default().fg(Color::DarkGray),
            ))];
        }

        diff_text
            .lines()
            .map(|line| {
                let style = if line.starts_with("=== ") {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("@@") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(Color::Yellow)
                } else if line.contains("untracked") {
                    Style::default().fg(Color::Magenta)
                } else if line.contains("modified") {
                    Style::default().fg(Color::Yellow)
                } else if line.contains("added") {
                    Style::default().fg(Color::Green)
                } else if line.contains("deleted") {
                    Style::default().fg(Color::Red)
                } else if line.contains("renamed") {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(format!(" {line}"), style))
            })
            .collect()
    }

    fn render_tree(tree_text: &str, theme: &Theme) -> Vec<Line<'static>> {
        if tree_text.is_empty() {
            return vec![Line::from(Span::styled(
                "  No files",
                Style::default().fg(Color::DarkGray),
            ))];
        }

        // Compact view: group by top-level directory, show counts
        let mut dirs: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut root_files: Vec<String> = Vec::new();

        for line in tree_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(slash_pos) = line.find('/') {
                let dir = &line[..slash_pos];
                let rest = &line[slash_pos + 1..];
                dirs.entry(dir.to_string())
                    .or_default()
                    .push(rest.to_string());
            } else {
                root_files.push(line.to_string());
            }
        }

        let mut lines = Vec::new();
        let total: usize = dirs.values().map(|v| v.len()).sum::<usize>() + root_files.len();
        lines.push(Line::from(Span::styled(
            format!(" {} files", total),
            Style::default().fg(theme.text_tertiary),
        )));
        lines.push(Line::default());

        // Directories first (compact: just name + count)
        for (dir, files) in &dirs {
            let file_count = count_recursive(files);
            lines.push(Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(theme.text_ghost)),
                Span::styled(
                    format!("{dir}/"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ({file_count})"),
                    Style::default().fg(theme.text_ghost),
                ),
            ]));
        }

        // Root files
        if !root_files.is_empty() && !dirs.is_empty() {
            lines.push(Line::default());
        }
        for file in &root_files {
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(file.clone(), Style::default().fg(theme.text_secondary)),
            ]));
        }

        lines
    }

    /// Count files recursively in a flat path list.
    fn count_recursive(files: &[String]) -> usize {
        files.len()
    }

    /// Simple tree node for building a file tree.
    #[allow(dead_code)]
    struct TreeNode {
        name: String,
        children: std::collections::BTreeMap<String, TreeNode>,
        is_file: bool,
    }

    #[allow(dead_code)]
    impl TreeNode {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                children: std::collections::BTreeMap::new(),
                is_file: false,
            }
        }

        fn insert(&mut self, path: &str) {
            let parts: Vec<&str> = path.split('/').collect();
            self.insert_parts(&parts);
        }

        fn insert_parts(&mut self, parts: &[&str]) {
            if parts.is_empty() {
                return;
            }
            if parts.len() == 1 {
                let entry = self
                    .children
                    .entry(parts[0].to_string())
                    .or_insert_with(|| TreeNode::new(parts[0]));
                entry.is_file = true;
            } else {
                let entry = self
                    .children
                    .entry(parts[0].to_string())
                    .or_insert_with(|| TreeNode::new(parts[0]));
                entry.insert_parts(&parts[1..]);
            }
        }

        fn render(
            &self,
            lines: &mut Vec<Line<'static>>,
            prefix: &str,
            is_root: bool,
            theme: &Theme,
        ) {
            if is_root {
                // Render children of root directly
                let entries: Vec<(&String, &TreeNode)> = self.children.iter().collect();
                for (i, (name, node)) in entries.iter().enumerate() {
                    let is_last = i == entries.len() - 1;
                    let connector = if is_last { "└── " } else { "├── " };
                    let child_prefix = if is_last { "    " } else { "│   " };

                    if node.is_file && node.children.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!(" {prefix}{connector}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(name.to_string(), Style::default().fg(theme.fg)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!(" {prefix}{connector}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                format!("{name}/"),
                                Style::default()
                                    .fg(theme.accent)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        let new_prefix = format!("{prefix}{child_prefix}");
                        node.render(lines, &new_prefix, false, theme);
                    }
                }
            } else {
                let entries: Vec<(&String, &TreeNode)> = self.children.iter().collect();
                for (i, (name, node)) in entries.iter().enumerate() {
                    let is_last = i == entries.len() - 1;
                    let connector = if is_last { "└── " } else { "├── " };
                    let child_prefix = if is_last { "    " } else { "│   " };

                    if node.is_file && node.children.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!(" {prefix}{connector}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(name.to_string(), Style::default().fg(theme.fg)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!(" {prefix}{connector}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                format!("{name}/"),
                                Style::default()
                                    .fg(theme.accent)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                        let new_prefix = format!("{prefix}{child_prefix}");
                        node.render(lines, &new_prefix, false, theme);
                    }
                }
            }
        }
    }

    /// Refresh side panel content (git status + diff + file tree). Call from event loop.
    pub fn refresh_content(state: &mut AppState, working_dir: &std::path::Path) {
        use std::process::Command;

        // Git status (shows all changes including untracked) — human-readable format
        let raw_status = Command::new("git")
            .args(["status", "--short"])
            .current_dir(working_dir)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        // Convert short codes to readable labels
        let status: String = raw_status
            .lines()
            .map(|line| {
                if line.len() < 4 {
                    return line.to_string();
                }
                let code = &line[..2];
                let file = line[3..].trim();
                match code.trim() {
                    "??" => format!("  untracked  {file}"),
                    "M" | " M" => format!("  modified   {file}"),
                    "MM" => format!("  modified*  {file}"),
                    "A" | " A" => format!("  added      {file}"),
                    "D" | " D" => format!("  deleted    {file}"),
                    "R" => format!("  renamed    {file}"),
                    "C" => format!("  copied     {file}"),
                    _ => format!("  {code} {file}"),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Git diff (tracked modifications)
        let diff = Command::new("git")
            .args(["diff", "--patch"])
            .current_dir(working_dir)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        // Git staged diff
        let cached = Command::new("git")
            .args(["diff", "--cached", "--patch"])
            .current_dir(working_dir)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        let mut parts = Vec::new();
        if !status.is_empty() {
            parts.push(format!("=== Status ===\n{status}"));
        }
        if !cached.is_empty() {
            parts.push(format!("=== Staged ===\n{cached}"));
        }
        if !diff.is_empty() {
            parts.push(format!("=== Unstaged ===\n{diff}"));
        }

        state.side_panel_diff = if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n")
        };

        // File tree
        state.side_panel_tree = Command::new("git")
            .args(["ls-files"])
            .current_dir(working_dir)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
    }
}
pub mod status {
    //! Status bar: streaming indicator, tool count, elapsed time

    use crate::tui::{app::AppState, theme::Theme};
    use ratatui::{prelude::*, widgets::Paragraph};

    pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
        let text = if state.is_streaming {
            let elapsed = state.elapsed_ms();
            let secs = elapsed as f64 / 1000.0;
            let tools = state.active_tools.len();
            let tokens = state.input_tokens + state.output_tokens;
            format!(
                " streaming... | {} tool(s) | {:.1}s | {} tokens",
                tools, secs, tokens
            )
        } else if state.turn_count > 0 {
            format!(
                " {} turn(s) | {} tool call(s) | {} in / {} out tokens",
                state.turn_count, state.tool_count, state.input_tokens, state.output_tokens
            )
        } else {
            " ready".into()
        };

        let style = if state.is_streaming {
            theme.accent_style()
        } else {
            theme.dimmed()
        };

        let status = Paragraph::new(text).style(style);
        f.render_widget(status, area);
    }
}
pub mod tool_call {
    //! Tool call rendering with output preview and inline diffs.

    use crate::tui::app::{ToolCall, ToolStatus};
    use crate::tui::theme::Theme;
    use crate::tui::widgets::diff_inline;
    use ratatui::prelude::*;

    const MAX_OUTPUT_LINES: usize = 5;
    const MAX_FILE_TOOL_LINES: usize = 12;

    /// Render a tool call as lines: badge + optional diff or output preview.
    pub fn render_tool_call(
        tool: &ToolCall,
        theme: &Theme,
        frame_count: u64,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let (icon, icon_style) = match tool.status {
            ToolStatus::Running => {
                let spinner = match (frame_count / 4) % 4 {
                    0 => "⠋",
                    1 => "⠙",
                    2 => "⠹",
                    _ => "⠸",
                };
                (spinner.to_string(), Style::default().fg(theme.accent))
            }
            ToolStatus::Done => ("✓".into(), Style::default().fg(theme.success)),
            ToolStatus::Error => ("✗".into(), Style::default().fg(theme.error)),
        };

        let dur = tool
            .duration_ms
            .map(|d| format!(" ({d}ms)"))
            .unwrap_or_default();

        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), icon_style),
            Span::styled(
                tool.name.clone(),
                Style::default()
                    .fg(theme.tool_badge)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(tool.input_summary.clone(), Style::default().fg(theme.dim)),
            Span::styled(dur, Style::default().fg(theme.dim)),
        ]));

        if let Some(ref output) = tool.output_preview
            && tool.status != ToolStatus::Running
            && !output.is_empty()
        {
            // Try rendering as inline diff for file tools
            if let Some(diff_lines) = diff_inline::render_diff_output(output, &tool.name, theme) {
                lines.extend(diff_lines);
            } else {
                // Default: plain text preview
                let is_file_tool = matches!(tool.name.as_str(), "Edit" | "Write" | "ApplyPatch");
                let max_lines = if is_file_tool {
                    MAX_FILE_TOOL_LINES
                } else {
                    MAX_OUTPUT_LINES
                };

                let preview_lines: Vec<&str> = output.lines().take(max_lines).collect();
                let total = output.lines().count();
                let style = if tool.status == ToolStatus::Error {
                    Style::default().fg(theme.error)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                for pl in &preview_lines {
                    lines.push(Line::from(Span::styled(format!("    {pl}"), style)));
                }
                if total > max_lines {
                    lines.push(Line::from(Span::styled(
                        format!("    ... ({} more lines)", total - max_lines),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }

        lines
    }
}
