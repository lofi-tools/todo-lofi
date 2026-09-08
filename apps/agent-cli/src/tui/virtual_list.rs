//! Virtualized list: only renders visible items with height caching.
//!
//! O(visible_height) per frame instead of O(total_lines).

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Strip terminal escape sequences and control bytes from display text.
///
/// Rows can carry arbitrary bytes — a `Read` tool result is raw file content,
/// and model text can quote anything. Printing an ESC sequence, carriage
/// return, tab, or backspace verbatim makes the terminal *move its cursor* in
/// the middle of a cell run, so later glyphs land on top of earlier rows and
/// columns (output looks like several writes competing for the same spots).
/// This guarantees the symbols we hand to the terminal can never reposition
/// the cursor: ESC sequences are removed whole, and remaining control bytes
/// are replaced with a space so column math stays stable.
fn sanitize_for_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Consume the whole escape sequence so no leftover parameter
            // bytes leak into the visible text.
            match chars.peek() {
                // CSI: `ESC [ params final` — final byte is 0x40..=0x7e.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        let code = next as u32;
                        if (0x40..=0x7e).contains(&code) {
                            break;
                        }
                    }
                }
                // OSC / DCS / SOS / PM / APC: run until BEL or ST (`ESC \`).
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            // Skip the `\` of the ST terminator, if present.
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other two-byte escape: drop its one parameter char.
                _ => {
                    chars.next();
                }
            }
        } else {
            let code = c as u32;
            if c == '\r' || c == '\u{7}' || c == '\u{8}' || c == '\u{b}' || c == '\u{c}' {
                // Carriage return / BEL / backspace / vertical tab / form
                // feed would all move the terminal cursor.
                continue;
            } else if c == '\t' {
                // Tabs advance to the next tab stop mid-print; render as a
                // plain space so alignment is predictable.
                out.push(' ');
            } else if c == '\n' {
                // A row must stay one line; a stray newline would push the
                // rest of the run onto the next terminal row.
                out.push(' ');
            } else if code < 0x20 || code == 0x7f {
                // Remaining C0 controls / DEL: invisible, drop them.
                continue;
            } else {
                out.push(c);
            }
        }
    }
    out
}

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

    /// Byte offsets of each grapheme-cluster start in `row_text(idx)`, in
    /// order. The end of the last grapheme is `row_text(idx).len()`. Returns
    /// an empty vec when `idx` is out of range.
    ///
    /// This is the bridge between grapheme-cluster indices (what selection
    /// columns are) and byte offsets (what `str` slicing needs). Selection
    /// columns are grapheme indices so they can never land mid-codepoint.
    pub fn row_grapheme_bytes(&self, idx: usize) -> Vec<usize> {
        let text = self.row_text(idx);
        use unicode_segmentation::UnicodeSegmentation;
        let mut offsets = Vec::with_capacity(text.len());
        let mut byte = 0usize;
        for grapheme in text.graphemes(true) {
            offsets.push(byte);
            byte += grapheme.len();
        }
        offsets
    }

    /// Number of grapheme clusters in `row_text(idx)`. Returns 0 when `idx` is
    /// out of range.
    pub fn row_grapheme_count(&self, idx: usize) -> usize {
        self.row_grapheme_bytes(idx).len()
    }

    /// The substring of `row_text(idx)` covering grapheme indices
    /// `start_g..end_g` (clamped to the row's grapheme count). Returns an
    /// empty string when `idx` is out of range or the range is empty.
    pub fn row_slice_by_graphemes(&self, idx: usize, start_g: usize, end_g: usize) -> String {
        let text = self.row_text(idx);
        if text.is_empty() {
            return String::new();
        }
        let offsets = self.row_grapheme_bytes(idx);
        // `offsets.len()` graphemes ⇒ the end boundary is `text.len()`.
        let start_byte = offsets.get(start_g).copied().unwrap_or(text.len());
        let end_byte = offsets.get(end_g).copied().unwrap_or(text.len());
        // Grapheme boundaries are always valid char boundaries, so this slice
        // can never panic mid-codepoint.
        text[start_byte.min(end_byte)..end_byte.max(start_byte)].to_string()
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
    /// normalized output selection `(start_row, start_col, end_row, end_col)`
    /// where the columns are grapheme-cluster indices; the selected graphemes
    /// of overlapping rows get the REVERSED modifier.
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

            // Render line directly to buffer. Content can be arbitrary tool/
            // file bytes, so scrub terminal controls first — printing them
            // verbatim would move the terminal cursor and let later glyphs
            // overwrite earlier rows/columns.
            let safe_line = Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(sanitize_for_display(&span.content), span.style))
                    .collect::<Vec<_>>(),
            );
            let para = Paragraph::new(vec![safe_line]);
            para.render(line_area, buf);

            screen_y += 1;
        }
    }
}

// ─── A copy of `line` with the grapheme range `[start_g..end_g)` (in the
/// joined span text) styled with the REVERSED modifier; everything else
/// keeps its style.
///
/// `start_g`/`end_g` are grapheme-cluster indices into the row's joined
/// span text. They are resolved to byte offsets over the joined text and
/// each span is split at those byte offsets. Because grapheme boundaries are
/// always valid char boundaries, the per-span byte slices can never land
/// mid-codepoint — the selection is grapheme-aligned, so this is panic-free
/// for any text (combining marks, multi-byte CJK, emoji ZWJ sequences).
fn highlight_line(line: &Line<'static>, start_g: usize, end_g: usize) -> Line<'static> {
    // Resolve grapheme indices to byte offsets over the joined span text.
    // `end_g` may be `usize::MAX` (full remainder); clamp it to the grapheme
    // count, whose end byte is the joined text length.
    let joined: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    use unicode_segmentation::UnicodeSegmentation;
    let mut grapheme_bytes: Vec<usize> = Vec::with_capacity(joined.len());
    let mut byte = 0usize;
    for grapheme in joined.graphemes(true) {
        grapheme_bytes.push(byte);
        byte += grapheme.len();
    }
    let total_g = grapheme_bytes.len();
    let start_byte = grapheme_bytes.get(start_g.min(total_g)).copied().unwrap_or(joined.len());
    let end_byte = grapheme_bytes.get(end_g.min(total_g)).copied().unwrap_or(joined.len());
    let (start_byte, end_byte) = (start_byte.min(end_byte), end_byte.max(start_byte));

    let mut spans_out = Vec::new();
    let mut pos = 0usize;
    for span in &line.spans {
        let text = span.content.to_string();
        let span_end = pos + text.len();
        // Map the joined-text byte offsets into this span's local byte range,
        // clamped to the span bounds.
        let a = start_byte.saturating_sub(pos).min(text.len());
        let b = end_byte.saturating_sub(pos).min(text.len());
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
    fn highlight_line_marks_only_selected_graphemes() {
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
    fn row_graphemes_and_count_handle_multibyte_and_clusters() {
        let mut list = VirtualList::new();
        // `é` = `e` + combining acute (U+0301): one grapheme, two codepoints.
        // `▊` = one 3-byte codepoint. `🇯🇵` = regional-indicator ZWJ pair:
        // one grapheme, four bytes, two codepoints.
        list.set_committed(vec![VItem::new(Line::from(vec![
            Span::raw("a"),
            Span::raw("é"),
            Span::raw("▊"),
            Span::raw("🇯🇵"),
        ]))]);
        // joined = "aé▊🇯🇵" → 4 graphemes.
        assert_eq!(list.row_text(0), "aé▊🇯🇵");
        assert_eq!(list.row_grapheme_count(0), 4);
        // Byte offsets of each grapheme start.
        // a=0, é=1..3 (e+◌́), ▊=3..6, 🇯🇵=6..14.
        assert_eq!(list.row_grapheme_bytes(0), vec![0, 1, 3, 6]);
    }

    #[test]
    fn row_slice_by_graphemes_never_splits_clusters() {
        let mut list = VirtualList::new();
        list.set_committed(vec![VItem::new(Line::from(vec![
            Span::raw("a"),
            Span::raw("é"), // e + combining acute
            Span::raw("b"),
        ]))]);
        // Slicing at grapheme index 1..2 returns the whole `é` cluster,
        // never a lone combining mark or partial codepoint.
        assert_eq!(list.row_slice_by_graphemes(0, 1, 2), "é");
        // 0..3 = whole row.
        assert_eq!(list.row_slice_by_graphemes(0, 0, 3), "aéb");
        // out-of-range end clamps to row end.
        assert_eq!(list.row_slice_by_graphemes(0, 1, usize::MAX), "éb");
        // empty range.
        assert_eq!(list.row_slice_by_graphemes(0, 1, 1), "");
    }

    #[test]
    fn highlight_line_marks_selected_graphemes_not_codepoints() {
        // `aéb`: a, é(e+◌́), b — 3 graphemes. With the old byte-offset
        // selection, highlighting grapheme 1 (`é`) would have to land on byte
        // 1 (mid-cluster); grapheme indices make it exact.
        let line = Line::from(vec![Span::raw("a"), Span::raw("éb")]);
        let out = highlight_line(&line, 1, 2);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        // span 0 `a` unchanged; span 1 splits into `é` (reversed) + `b`.
        assert_eq!(texts, vec!["a", "é", "b"]);
        assert!(!out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[2].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn highlight_line_reproduces_original_panic_case_via_graphemes() {
        // The shipped panic was a multi-byte char (▊) split by a byte-offset
        // selection. With grapheme indices the selection can never land inside
        // a codepoint, so the analogous selection (highlight `ab`) is exact.
        let line = Line::from(vec![Span::raw("ab▊cd")]); // graphemes: a b ▊ c d
        let out = highlight_line(&line, 0, 2);
        let texts: Vec<String> = out.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["ab", "▊cd"]);
        assert!(out.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!out.spans[1].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn sanitize_removes_escape_sequences_and_control_bytes() {
        // CSI color codes are dropped whole.
        assert_eq!(sanitize_for_display("\x1b[31mred\x1b[0m"), "red");
        // Cursor-movement escapes (the ones that make later glyphs overwrite
        // earlier rows) are removed entirely.
        assert_eq!(sanitize_for_display("a\x1b[1Ab"), "ab");
        assert_eq!(sanitize_for_display("a\x1b[Hb"), "ab");
        // OSC (terminal title, clipboard) runs to BEL or ST.
        assert_eq!(sanitize_for_display("a\x1b]0;title\x07b"), "ab");
        assert_eq!(sanitize_for_display("a\x1b]52;c,abc\x1b\\b"), "ab");
        // Carriage returns and other cursor movers disappear.
        assert_eq!(sanitize_for_display("a\rb"), "ab");
        assert_eq!(sanitize_for_display("ab\u{8}c"), "abc");
        // Tabs become a plain space so they can't jump the cursor mid-run.
        assert_eq!(sanitize_for_display("a\tb"), "a b");
        // A stray newline (which would push the rest onto the next terminal
        // row) is flattened too.
        assert_eq!(sanitize_for_display("a\nb"), "a b");
        // Remaining C0 / DEL bytes are dropped; normal text is untouched.
        assert_eq!(sanitize_for_display("a\u{0}\u{3}b\u{7f}"), "ab");
        assert_eq!(sanitize_for_display("plain text"), "plain text");
    }

    #[test]
    fn poisoned_rows_never_write_cursor_movers_or_displace_neighbors() {
        // Two rows that carry terminal control bytes: an ESC sequence and a
        // carriage return. Before sanitizing, printing those raw would move
        // the terminal cursor and the rest of the run would land on earlier
        // rows/columns ("multiple writes competing for the same char spots").
        let mut list = VirtualList::new();
        list.set_committed(vec![
            VItem::new(Line::from("AB\x1b[31mCD")),
            VItem::new(Line::from("a\x1b[1Ab")),
            VItem::new(Line::from("END")),
        ]);
        list.set_viewport(3);
        list.sticky_bottom = true;
        let mut buf = Buffer::empty(Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
        });
        list.render(buf.area, &mut buf, None);

        // No cell anywhere may hold a control/escape glyph.
        for y in 0..3 {
            for x in 0..10 {
                let symbol = buf.cell((x, y)).unwrap().symbol();
                for ch in symbol.chars() {
                    let code = ch as u32;
                    assert!(
                        ch != '\u{1b}' && code >= 0x20 || ch == ' ',
                        "control byte {ch:?} leaked at ({x},{y})"
                    );
                }
            }
        }

        let row = |y: u16| {
            (0..10)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        };
        // Row 0 lost the ESC sequence, keeping both visible parts inline.
        assert_eq!(&row(0)[..4], "ABCD");
        // Row 1 kept its own line — nothing was displaced by the cursor-up.
        assert_eq!(&row(1)[..2], "ab");
        assert_eq!(&row(2)[..3], "END");
    }
}
