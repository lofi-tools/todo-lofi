//! Scroll state management with bounds checking.

/// Scroll state for a scrollable area (messages, side panel, etc.).
#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    pub offset: u16,
    pub content_height: u16,
    pub viewport_height: u16,
    pub sticky_bottom: bool,
}

impl ScrollState {
    pub fn new() -> Self {
        Self {
            offset: 0,
            content_height: 0,
            viewport_height: 0,
            sticky_bottom: true,
        }
    }

    /// Maximum valid offset.
    fn max_offset(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// Clamp offset to valid range.
    pub fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    /// Effective offset: if sticky_bottom, scroll to end.
    pub fn effective_offset(&self) -> u16 {
        if self.sticky_bottom {
            self.max_offset()
        } else {
            self.offset.min(self.max_offset())
        }
    }

    /// Scroll up by n lines.
    ///
    /// While sticky-bottom is active, `offset` is stale (not maintained), so
    /// materialize it from the bottom before applying the scroll. Without this,
    /// the first scroll-up would jump to the top instead of one line up.
    pub fn scroll_up(&mut self, n: u16) {
        if self.sticky_bottom {
            self.offset = self.max_offset();
            self.sticky_bottom = false;
        }
        self.offset = self.offset.saturating_sub(n);
    }

    /// Scroll down by n lines.
    ///
    /// A no-op while sticky-bottom is active: the view is already at the end,
    /// and unsticking here would teleport it to a stale offset.
    pub fn scroll_down(&mut self, n: u16) {
        if self.sticky_bottom {
            return;
        }
        self.offset = (self.offset + n).min(self.max_offset());
        // Re-enable sticky bottom if we've scrolled to the end
        if self.offset >= self.max_offset() {
            self.sticky_bottom = true;
        }
    }

    /// Jump to bottom and enable sticky.
    pub fn scroll_to_bottom(&mut self) {
        self.sticky_bottom = true;
        self.offset = self.max_offset();
    }

    /// Page up (one viewport).
    pub fn page_up(&mut self) {
        self.scroll_up(self.viewport_height.saturating_sub(2));
    }

    /// Page down (one viewport).
    pub fn page_down(&mut self) {
        self.scroll_down(self.viewport_height.saturating_sub(2));
    }

    /// Update content and viewport dimensions.
    pub fn update_dimensions(&mut self, content_height: u16, viewport_height: u16) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default state: sticky bottom, offset 0.
    #[test]
    fn starts_sticky_at_bottom() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        assert!(s.sticky_bottom);
        assert_eq!(s.effective_offset(), 90);
    }

    /// The core bug: first scroll-up from sticky-bottom must move up one line,
    /// not snap to the top.
    #[test]
    fn scroll_up_from_sticky_bottom_moves_one_line() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(1);
        assert!(!s.sticky_bottom);
        assert_eq!(s.offset, 89);
        assert_eq!(s.effective_offset(), 89);
    }

    #[test]
    fn page_up_from_sticky_bottom_moves_one_page() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.page_up();
        assert_eq!(s.effective_offset(), 82); // 90 - (10 - 2)
    }

    #[test]
    fn home_from_sticky_bottom_jumps_to_top() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(s.content_height);
        assert_eq!(s.effective_offset(), 0);
    }

    #[test]
    fn scrolling_back_to_bottom_reenables_sticky() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(5);
        assert!(!s.sticky_bottom);
        s.scroll_down(5);
        assert!(s.sticky_bottom, "reaching the bottom should re-enable sticky");
        assert_eq!(s.effective_offset(), 90);
    }

    #[test]
    fn scroll_to_bottom_reenables_sticky() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(20);
        s.scroll_to_bottom();
        assert!(s.sticky_bottom);
        assert_eq!(s.effective_offset(), 90);
    }

    /// New content arriving while sticky keeps the view pinned to the bottom.
    #[test]
    fn sticky_follows_new_content() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.update_dimensions(120, 10);
        assert_eq!(s.effective_offset(), 110);
    }

    /// New content while scrolled up must NOT yank the viewport — the user
    /// stays anchored where they scrolled to.
    #[test]
    fn non_sticky_offset_survives_new_content() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(30); // offset = 60
        s.update_dimensions(120, 10);
        assert_eq!(s.effective_offset(), 60);
    }

    #[test]
    fn offset_clamps_when_content_shrinks() {
        let mut s = ScrollState::new();
        s.update_dimensions(100, 10);
        s.scroll_up(50); // offset = 40
        s.update_dimensions(45, 10); // max offset now 35
        assert_eq!(s.effective_offset(), 35);
    }

    #[test]
    fn content_shorter_than_viewport_stays_at_zero() {
        let mut s = ScrollState::new();
        s.update_dimensions(5, 10);
        assert_eq!(s.effective_offset(), 0);
        s.scroll_up(3); // no-op, already at top
        assert_eq!(s.effective_offset(), 0);
    }
}
