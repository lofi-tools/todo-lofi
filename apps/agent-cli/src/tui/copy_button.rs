//! Pure state machine for the per-turn "copy row" affordance.
//!
//! A committed turn owns one [`CopyRowState`] describing its copy row: it
//! renders idle (a collapsed glyph), hovered (expanded "Copy" label), or
//! copied (checkmark + "Copied") — and a successful copy resets back to idle
//! exactly [`COPIED_RESET_DELAY_MS`] after it happened, unless another copy
//! re-arms the deadline first (mirroring the web client's `copyButtonHandlers`
//! and `useCopyToClipboard` reset timer).
//!
//! The logic is deliberately free of any rendering or input plumbing so it can
//! be unit-tested in isolation; mouse events are mapped to the pure transition
//! helpers below and the TUI's frame tick drives expiry.

/// How long a successful copy keeps the row in the "Copied" state before it
/// reverts to idle.
pub const COPIED_RESET_DELAY_MS: u64 = 2000;

/// Length of one TUI redraw tick in milliseconds (see
/// `event_loop::TICK_RATE`); used to convert the reset delay into frames so
/// expiry can be driven by the draw loop.
pub const TUI_TICK_MS: u64 = 16;

/// [`COPIED_RESET_DELAY_MS`] expressed in TUI frames.
pub const COPIED_RESET_DELAY_FRAMES: u64 = COPIED_RESET_DELAY_MS / TUI_TICK_MS;

/// Which of the three copy-row states to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRowVisual {
    /// Not hovered, not copied: collapsed clipboard glyph, muted styling.
    Idle,
    /// Hovered and not copied: expanded "Copy" label next to the glyph.
    Hovered,
    /// A copy happened recently: checkmark + "Copied" label, success styling.
    /// Takes priority over hovered.
    Copied,
}

/// Whether mouse-over should activate the hovered state. Hover is suppressed
/// while the "Copied" confirmation is showing so the two states don't fight.
pub fn handle_mouse_over(is_copied: bool) -> bool {
    !is_copied
}

/// Whether mouse-out should keep the hovered state: always clears it.
pub fn handle_mouse_out() -> bool {
    false
}

/// What a click on the copy row does: mark it copied and drop the hover state.
/// Returns `(is_copied, is_hovered)`.
pub fn handle_copy() -> (bool, bool) {
    (true, false)
}

/// The visual state for the given flags. "Copied" wins over "Hovered".
pub fn copy_row_visual(is_copied: bool, is_hovered: bool) -> CopyRowVisual {
    if is_copied {
        CopyRowVisual::Copied
    } else if is_hovered {
        CopyRowVisual::Hovered
    } else {
        CopyRowVisual::Idle
    }
}

/// The text the copy row shows for the given flags, mirroring the web client's
/// `getCopyIconText`. `leading_space` prepends a space before the glyph (used
/// when the row sits directly after other inline content).
pub fn copy_row_text(is_copied: bool, is_hovered: bool, leading_space: bool) -> String {
    let lead = if leading_space { " " } else { "" };
    match copy_row_visual(is_copied, is_hovered) {
        CopyRowVisual::Idle => format!("{lead}⎘"),
        CopyRowVisual::Hovered => format!("{lead}⎘ Copy"),
        CopyRowVisual::Copied => format!("{lead}✓ Copied"),
    }
}

/// Per-turn copy-row state. Frame-based so the TUI's draw loop can expire the
/// "Copied" confirmation without a timer; re-copying cancels any earlier
/// pending reset by simply re-arming the deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopyRowState {
    is_copied: bool,
    is_hovered: bool,
    /// The frame a copy was last performed on; `None` while idle. Once
    /// `now - copied_frame >= COPIED_RESET_DELAY_FRAMES` the row reverts to
    /// idle.
    copied_frame: Option<u64>,
}

impl CopyRowState {
    /// An idle copy row (nothing copied, not hovered).
    pub fn idle() -> Self {
        Self::default()
    }

    pub fn is_copied(&self) -> bool {
        self.is_copied
    }

    pub fn is_hovered(&self) -> bool {
        self.is_hovered
    }

    /// The visual state to render right now.
    pub fn visual(&self) -> CopyRowVisual {
        copy_row_visual(self.is_copied, self.is_hovered)
    }

    /// The text to render right now.
    pub fn text(&self, leading_space: bool) -> String {
        copy_row_text(self.is_copied, self.is_hovered, leading_space)
    }

    /// Mouse entered the row. Hover only activates while not copied, so the
    /// "Copied" confirmation is never replaced by a hover state.
    pub fn mouse_over(&mut self) {
        self.is_hovered = handle_mouse_over(self.is_copied);
    }

    /// Mouse left the row: hover always clears.
    pub fn mouse_out(&mut self) {
        self.is_hovered = handle_mouse_out();
    }

    /// The user clicked the row at `now` (in frames): mark it copied and arm
    /// the reset deadline. A second click before the previous deadline expires
    /// re-arms it, which is what cancels the prior pending reset — there is
    /// only ever one deadline, so rapid re-copies can't stack timers.
    pub fn copy(&mut self, now: u64) {
        let (copied, hovered) = handle_copy();
        self.is_copied = copied;
        self.is_hovered = hovered;
        self.copied_frame = Some(now);
    }

    /// Whether the "Copied" confirmation is still within its display window at
    /// `now`.
    pub fn copied_active(&self, now: u64) -> bool {
        match self.copied_frame {
            None => false,
            Some(frame) => now.wrapping_sub(frame) < COPIED_RESET_DELAY_FRAMES,
        }
    }

    /// Advance the row to `now`. Returns `true` when the "Copied" confirmation
    /// just expired (so the caller can redraw); the row reverts to idle.
    pub fn tick(&mut self, now: u64) -> bool {
        let expired = self.is_copied && !self.copied_active(now);
        if expired {
            self.is_copied = false;
            self.is_hovered = false;
            self.copied_frame = None;
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_text_covers_all_flag_combinations() {
        // copied=false, hovered=false: collapsed glyph only.
        assert_eq!(copy_row_text(false, false, false), "⎘");
        assert_eq!(copy_row_text(false, false, true), " ⎘");
        // copied=false, hovered=true: expanded "Copy" label.
        assert_eq!(copy_row_text(false, true, false), "⎘ Copy");
        assert_eq!(copy_row_text(false, true, true), " ⎘ Copy");
        // copied=true (hover ignored): checkmark + "Copied".
        assert_eq!(copy_row_text(true, false, false), "✓ Copied");
        assert_eq!(copy_row_text(true, false, true), " ✓ Copied");
        assert_eq!(copy_row_text(true, true, false), "✓ Copied");
        assert_eq!(copy_row_text(true, true, true), " ✓ Copied");
    }

    #[test]
    fn mouse_over_is_suppressed_while_copied() {
        assert!(handle_mouse_over(false));
        assert!(!handle_mouse_over(true));
    }

    #[test]
    fn mouse_out_always_clears_hover() {
        assert!(!handle_mouse_out());
        assert!(!handle_mouse_out());
    }

    #[test]
    fn copy_marks_copied_and_clears_hover() {
        assert_eq!(handle_copy(), (true, false));
    }

    #[test]
    fn copied_state_takes_priority_over_hovered() {
        assert_eq!(copy_row_visual(false, false), CopyRowVisual::Idle);
        assert_eq!(copy_row_visual(false, true), CopyRowVisual::Hovered);
        assert_eq!(copy_row_visual(true, false), CopyRowVisual::Copied);
        assert_eq!(copy_row_visual(true, true), CopyRowVisual::Copied);
    }

    #[test]
    fn copied_confirmation_resets_after_exactly_the_delay() {
        let mut row = CopyRowState::idle();
        row.copy(100);
        assert!(row.is_copied());
        assert_eq!(row.visual(), CopyRowVisual::Copied);

        // Still showing through the last frame inside the window.
        assert!(row.copied_active(100 + COPIED_RESET_DELAY_FRAMES - 1));
        assert!(!row.tick(100 + COPIED_RESET_DELAY_FRAMES - 1));
        assert!(row.is_copied());

        // Exactly one delay later the confirmation expires back to idle.
        let expired_at = 100 + COPIED_RESET_DELAY_FRAMES;
        assert!(!row.copied_active(expired_at));
        assert!(row.tick(expired_at));
        assert_eq!(row, CopyRowState::idle());
        assert!(!row.copied_active(u64::MAX));
    }

    #[test]
    fn rapid_recopies_rearm_one_deadline_without_overlapping_timers() {
        let mut row = CopyRowState::idle();
        row.copy(0);
        // A second copy lands before the first deadline (frame 125) would fire.
        row.copy(60);

        // The first deadline must not reset the row — only the re-armed one
        // (60 + COPIED_RESET_DELAY_FRAMES) may.
        let first_deadline = COPIED_RESET_DELAY_FRAMES;
        assert!(!row.tick(first_deadline));
        assert!(row.is_copied());

        let rearmed_deadline = 60 + COPIED_RESET_DELAY_FRAMES;
        assert!(row.copied_active(rearmed_deadline - 1));
        assert!(!row.tick(rearmed_deadline - 1));
        assert!(row.is_copied());

        assert!(row.tick(rearmed_deadline));
        assert_eq!(row, CopyRowState::idle());
    }

    #[test]
    fn hover_never_reactivates_while_copied() {
        let mut row = CopyRowState::idle();
        row.copy(0);
        // Mouse re-enters while "Copied" is showing: hover stays off and the
        // copied confirmation keeps priority.
        row.mouse_over();
        assert!(!row.is_hovered());
        assert_eq!(row.visual(), CopyRowVisual::Copied);

        // After expiry, hover can activate again.
        row.tick(COPIED_RESET_DELAY_FRAMES);
        row.mouse_over();
        assert!(row.is_hovered());
        assert_eq!(row.visual(), CopyRowVisual::Hovered);
        row.mouse_out();
        assert!(!row.is_hovered());
        assert_eq!(row.visual(), CopyRowVisual::Idle);
    }
}
