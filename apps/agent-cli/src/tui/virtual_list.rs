//! Virtualized list: only renders visible items with height caching.
//!
//! O(visible_height) per frame instead of O(total_lines).

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// A single pre-built line in the virtual list.
#[derive(Clone)]
pub struct VItem {
    pub line: Line<'static>,
}

impl VItem {
    pub fn new(line: Line<'static>) -> Self {
        Self { line }
    }

    /// Height is always 1 row (lines are pre-wrapped).
    pub fn height(&self) -> u16 {
        1
    }
}

/// Virtualized list that only renders visible items.
pub struct VirtualList {
    /// Pre-built committed items (not rebuilt per frame).
    committed_items: Vec<VItem>,
    /// Streaming items (rebuilt every frame — only a few lines).
    streaming_items: Vec<VItem>,
    /// Scroll offset in rows from the top.
    pub scroll_offset: u16,
    /// Viewport height in rows.
    pub viewport_height: u16,
    /// Auto-scroll to bottom on new content.
    pub sticky_bottom: bool,
    /// Width used when items were last built (for invalidation on resize).
    last_width: u16,
}

impl VirtualList {
    pub fn new() -> Self {
        Self {
            committed_items: Vec::new(),
            streaming_items: Vec::new(),
            scroll_offset: 0,
            viewport_height: 0,
            sticky_bottom: true,
            last_width: 0,
        }
    }

    /// Total number of rows.
    pub fn total_height(&self) -> u16 {
        (self.committed_items.len() + self.streaming_items.len()) as u16
    }

    /// Maximum valid scroll offset.
    fn max_offset(&self) -> u16 {
        self.total_height().saturating_sub(self.viewport_height)
    }

    /// Effective scroll offset (respects sticky_bottom).
    pub fn effective_offset(&self) -> u16 {
        if self.sticky_bottom {
            self.max_offset()
        } else {
            self.scroll_offset.min(self.max_offset())
        }
    }

    /// Set committed items (pre-built, cached).
    pub fn set_committed(&mut self, items: Vec<VItem>) {
        self.committed_items = items;
    }

    /// The rendered text of the item at `idx` (its spans joined in order), or
    /// an empty string when `idx` is out of range. Used for selection
    /// hit-testing and copying.
    pub fn row_text(&self, idx: usize) -> String {
        let item = if idx < self.committed_items.len() {
            self.committed_items.get(idx)
        } else {
            self.streaming_items.get(idx - self.committed_items.len())
        };
        item.map(|it| {
            it.line
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .unwrap_or_default()
    }

    /// Set streaming items (rebuilt every frame).
    pub fn set_streaming(&mut self, items: Vec<VItem>) {
        self.streaming_items = items;
    }

    /// Set viewport height.
    pub fn set_viewport(&mut self, height: u16) {
        self.viewport_height = height;
    }

    /// Check if width changed (needs rebuild).
    pub fn width_changed(&self, new_width: u16) -> bool {
        self.last_width != new_width
    }

    /// Record the width used for building.
    pub fn set_width(&mut self, width: u16) {
        self.last_width = width;
    }

    /// Scroll up by n rows.
    pub fn scroll_up(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by n rows.
    pub fn scroll_down(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.scroll_offset = (self.scroll_offset + n).min(self.max_offset());
        if self.scroll_offset >= self.max_offset() {
            self.sticky_bottom = true;
        }
    }

    /// Page up.
    pub fn page_up(&mut self) {
        self.scroll_up(self.viewport_height.saturating_sub(2));
    }

    /// Page down.
    pub fn page_down(&mut self) {
        self.scroll_down(self.viewport_height.saturating_sub(2));
    }

    /// Scroll to bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.sticky_bottom = true;
        self.scroll_offset = self.max_offset();
    }

    /// Render only visible items directly to the buffer. `selection` is the
    /// normalized output selection `(start_row, start_col, end_row, end_col)`;
    /// the selected bytes of overlapping rows get the REVERSED modifier.
    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        selection: Option<(u16, usize, u16, usize)>,
    ) {
        let offset = self.effective_offset();
        let total = self.total_height();
        let committed_len = self.committed_items.len() as u16;

        if area.height == 0 {
            return;
        }

        // Clear the entire message area first to prevent stale content bleed-through.
        // This is critical: without it, lines from previous scroll positions persist
        // in the buffer and corrupt the display.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }

        if total == 0 {
            return;
        }

        let mut screen_y = area.y;
        let end_y = area.y + area.height;

        // Walk through all items, only render visible ones
        for idx in 0..total {
            if screen_y >= end_y {
                break; // Past viewport
            }

            if idx < offset {
                continue; // Above viewport
            }

            // Get the item
            let item = if idx < committed_len {
                &self.committed_items[idx as usize]
            } else {
                let stream_idx = (idx - committed_len) as usize;
                if stream_idx < self.streaming_items.len() {
                    &self.streaming_items[stream_idx]
                } else {
                    continue;
                }
            };

            // Render this single line at screen_y
            let line_area = Rect {
                x: area.x,
                y: screen_y,
                width: area.width,
                height: 1,
            };

            // Highlight the selected bytes of this row, if it overlaps.
            let line = if let Some((start_row, start_col, end_row, end_col)) = selection {
                if idx >= start_row && idx <= end_row {
                    let row_start_col = if idx == start_row { start_col } else { 0 };
                    let row_end_col = if idx == end_row { end_col } else { usize::MAX };
                    highlight_line(&item.line, row_start_col, row_end_col)
                } else {
                    item.line.clone()
                }
            } else {
                item.line.clone()
            };

            // Render line directly to buffer
            let para = Paragraph::new(vec![line]);
            para.render(line_area, buf);

            screen_y += 1;
        }
    }
}

// ─── A copy of `line` with bytes `[start..end)` (in the joined span text)
/// styled with the REVERSED modifier; everything else keeps its style.
///
/// `start`/`end` are byte offsets into the row's joined span text. They come
/// from the selection machinery, which snaps them to char boundaries in the
/// *joined* text — but an individual span can still receive an offset that
/// lands mid-codepoint when a multi-byte char straddles a span boundary or
/// when streaming wraps a span at a non-char-aligned byte. Flooring each edge
/// to the span's own char boundary keeps slicing panic-free and matches the
/// visual intent (the selection snaps to the nearest codepoint start).
fn highlight_line(line: &Line<'static>, start: usize, end: usize) -> Line<'static> {
    let mut spans_out = Vec::new();
    let mut pos = 0usize;
    for span in &line.spans {
        let text = span.content.to_string();
        let span_end = pos + text.len();
        // Map the row-text byte offsets into this span's local byte range,
        // clamped to the span bounds, then snap to char boundaries so we
        // never slice mid-codepoint.
        let a = start.saturating_sub(pos).min(text.len());
        let b = end.saturating_sub(pos).min(text.len());
        let a = text.floor_char_boundary(a);
        let b = text.floor_char_boundary(b);
        if a >= b {
            // Fully outside the selection (or zero-width overlap).
            spans_out.push(span.clone());
        } else {
            if a > 0 {
                spans_out.push(Span::styled(text[..a].to_string(), span.style));
            }
            spans_out.push(Span::styled(
                text[a..b].to_string(),
                span.style.add_modifier(Modifier::REVERSED),
            ));
            if b < text.len() {
                spans_out.push(Span::styled(text[b..].to_string(), span.style));
            }
        }
        pos = span_end;
    }
    Line::from(spans_out)
}

impl Default for VirtualList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_text_joins_spans_in_order() {
        let mut list = VirtualList::new();
        list.set_committed(vec![VItem::new(Line::from(vec![
            Span::raw("ab"),
            Span::styled("cd", Style::default().fg(Color::Red)),
        ]))]);
        list.set_streaming(vec![VItem::new(Line::from("ef"))]);
        assert_eq!(list.row_text(0), "abcd");
        assert_eq!(list.row_text(1), "ef");
        assert_eq!(list.row_text(2), ""); // out of range
    }

    #[test]
    fn highlight_line_marks_only_selected_bytes() {
        let line = Line::from(vec![Span::raw("hello"), Span::raw(" world")]);
        let out = highlight_line(&line, 2, 8);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["he", "llo", " wo", "rld"]);
        assert!(out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(out.spans[2].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[3].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn highlight_line_handles_zero_width_and_whole_line() {
        let line = Line::from(vec![Span::raw("hello"), Span::raw(" world")]);
        // Zero-width selection changes nothing.
        let unchanged = highlight_line(&line, 5, 5);
        let texts: Vec<String> = unchanged
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(texts, vec!["hello", " world"]);
        // Whole-line selection marks every span.
        let all = highlight_line(&line, 0, 11);
        assert!(all
            .spans
            .iter()
            .all(|s| s.style.add_modifier.contains(Modifier::REVERSED)));
    }

    #[test]
    fn highlight_line_does_not_panic_on_mid_codepoint_selection() {
        // Regression: a multi-byte char (▊ is 3 bytes) with a selection byte
        // index landing inside it panicked on str slicing. Snapping to char
        // boundaries keeps slicing panic-free.
        //
        // `▊ab▊` joined bytes: ▊=0..3, a=3..4, b=4..5, ▊=5..8.
        let line = Line::from(vec![Span::raw("▊ab"), Span::raw("▊")]);
        // start=1 is inside the first ▊ (floors to 0); end=3 is the ▊/a
        // boundary. Selection on span 0 covers bytes [0..3) = ▊.
        let out = highlight_line(&line, 1, 3);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["▊", "ab", "▊"]);
        assert!(out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[2].style.add_modifier.contains(Modifier::REVERSED));

        // End index mid-char in a single-span line must not panic either.
        // `a▊b`: a=0..1, ▊=1..4, b=4..5.
        let single = Line::from(vec![Span::raw("a▊b")]);
        let out = highlight_line(&single, 0, 3);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        // end=3 floors to 1 (▊ start) → `a` highlighted, `▊b` not.
        assert_eq!(texts, vec!["a", "▊b"]);
        assert!(out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn highlight_line_reproduces_original_panic_case() {
        // The shipped panic was: `end byte index 3 is not a char boundary;
        // it is inside '▊' (bytes 2..5 of string)`. That is a span whose text
        // is `<2 bytes><▊>` and a selection end of 3 landing inside ▊. Without
        // flooring, `text[0..3]` / `text[3..]` panicked. Must not panic now.
        let line = Line::from(vec![Span::raw("ab▊cd")]); // a=0,b=1,▊=2..5,c=5,d=6
        let out = highlight_line(&line, 0, 3);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        // end=3 floors to 2 (▊ start) → `ab` highlighted, `▊cd` not.
        assert_eq!(texts, vec!["ab", "▊cd"]);
        assert!(out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
    }
}
