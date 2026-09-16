//! Shared design tokens for the todo-2 dark UI.
//!
//! Colors are defined once here so surfaces stay consistent instead of
//! repeating hex literals across views.

/// Background of the app window and the task list view.
pub const APP_BG: u32 = 0x1a1a1a;

/// Background of floating cards (e.g. the blocked-until picker).
pub const CARD_BG: u32 = 0x242424;

/// 1px hairline borders (panels, inputs).
pub const HAIRLINE: u32 = 0x333333;

/// Chrome surfaces: the navbar and the window-wide footer strip.
pub const PANEL_BG: u32 = 0x1e1e1e;

/// Hover and selection background for rows and pane switchers.
pub const PANEL_HOVER: u32 = 0x2a2a2a;

/// Secondary text: readable but de-emphasized.
pub const TEXT_MUTED: u32 = 0xa3a3a3;

/// Incidental metadata only; keep it off anything load-bearing.
pub const TEXT_FAINT: u32 = 0x737373;

/// Emphasized text, e.g. the selected pane switcher.
pub const TEXT_STRONG: u32 = 0xe5e5e5;

/// Success state (completed tool calls, connected integrations).
pub const SUCCESS: u32 = 0x4ade80;

/// Warning state (degraded but not failing: a sync that fell back, a stale
/// token).
pub const WARNING: u32 = 0xf59e0b;

/// Error state (failed tool calls, launch failures).
pub const DANGER: u32 = 0xef4444;

/// Error toast surface: a neutral gray that keeps the card distinct from app
/// panels while letting the error icon carry the severity.
pub const ERROR_TOAST_BG: u32 = 0x2e2e2e;

/// Added-line tint in a rendered diff.
pub const DIFF_ADD_BG: u32 = 0x16301f;

/// Removed-line tint in a rendered diff.
pub const DIFF_DEL_BG: u32 = 0x3a1d1d;
