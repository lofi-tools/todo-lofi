//! The history pane: what the app pushed out, and what each sync pass brought
//! back.
//!
//! Two feeds share one timeline, newest first, and both live in the persistent
//! sync operation log (`sync_op_log` in the storage layer): the outgoing side
//! is every provider push, delivered or failed, and the incoming side is one
//! row per pass that found something, written here when the integrations view
//! reports it (the log entry's kind is `incoming`). Opening the pane re-reads
//! the log, so both feeds survive a restart.

use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
    Styled, Svg, Task, Window, div, prelude::FluentBuilder, px, rgb, svg,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{IconName, StyledExt};
use storage::prelude::*;

use crate::store::Store;
use crate::theme;
use crate::ui_parts::integrations::since_label;

/// The classic history glyph: a clock face with a counter-clockwise arrow, the
/// icon most apps put on their own history affordance. The bundled icon set
/// has no `History`, so the Lucide geometry is inlined like the task row's
/// arrow.
pub(crate) const HISTORY_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></svg>"##;

/// The glyph at a given size and tint. An alpha-mask SVG paints with a text
/// color of its own, so the caller's foreground has to be applied here rather
/// than inherited from the button that holds it.
pub(crate) fn history_icon(size: Pixels, color: impl Into<Hsla>) -> Svg {
    svg()
        .size(size)
        .data(HISTORY_SVG)
        .text_color(color.into())
}

/// The count of pushes still owed, as a small pill beside the history glyph in
/// the title bar. A danger outline rather than a filled dot keeps the title
/// bar's chrome quiet while still reading as something to look at.
pub(crate) fn failed_badge(count: usize) -> AnyElement {
    div()
        .px_1()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(rgb(theme::DANGER))
        .text_xs()
        .text_color(rgb(theme::DANGER))
        .child(failed_badge_text(count))
        .into_any_element()
}

/// The badge's label. Past [`BADGE_MAX`] the exact number matters less than
/// the badge not stretching the button it sits on.
fn failed_badge_text(count: usize) -> String {
    if count > BADGE_MAX {
        format!("{BADGE_MAX}+")
    } else {
        count.to_string()
    }
}

/// The history button's tooltip, which spells out what the badge counts: the
/// glyph alone does not say whether a number is good news.
pub(crate) fn history_tooltip(failed: usize) -> String {
    match failed {
        0 => "History — synced operations and incoming changes".to_string(),
        1 => "History — 1 failed sync operation waiting to be retried".to_string(),
        count => format!("History — {count} failed sync operations waiting to be retried"),
    }
}

/// How many rows of the persistent log the pane reads, per provider.
const OPERATION_LIMIT: u32 = 100;

/// The providers whose logs the pane merges into one timeline. A provider with
/// no rows simply contributes none.
const SYNC_PROVIDERS: [&str; 2] = ["github", "todoist"];

/// How much of a payload or error a row prints before it is cut off.
const DETAIL_MAX_CHARS: usize = 240;

/// The largest count the badge spells out.
const BADGE_MAX: usize = 99;

/// An incoming change a sync pass brought back: one event per pass, described
/// from that pass's summary rather than from each remote object it touched.
/// Its time is the log's own, stamped when the row is written.
#[derive(Clone, Debug)]
pub struct IncomingChange {
    /// The provider the pass was for (`github`, `todoist`).
    pub provider: &'static str,
    /// What the pass found, in the provider's own words.
    pub description: String,
}

impl IncomingChange {
    pub fn new(provider: &'static str, description: String) -> Self {
        Self {
            provider,
            description,
        }
    }
}

/// The pane's own state. The feed is the persistent log, re-read when the pane
/// opens and when a pass lands; the count beside the header glyph is carried
/// along so the badge does not wait for the pane to be shown.
pub struct SyncHistoryPanel {
    store: Store,
    open: bool,
    operations: Vec<SyncOpLogEntry>,
    /// Tasks whose most recent logged push failed, across providers: the
    /// pushes a replay still owes.
    failed: usize,
    loading: bool,
    _load: Option<Task<()>>,
}

impl SyncHistoryPanel {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            store,
            open: false,
            operations: Vec::new(),
            failed: 0,
            loading: true,
            _load: None,
        };
        // Read the log once at startup so the header badge can show failures
        // recorded before this session without waiting for the pane to open.
        panel.reload(cx);
        panel
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// How many pushes still owe a retry, for the header badge.
    pub fn failed_count(&self) -> usize {
        self.failed
    }

    /// Open the pane and re-read the persistent log, so a push made while it
    /// was closed is on screen when it appears.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.reload(cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
        } else {
            self.open(cx);
        }
    }

    /// Record an incoming change from a sync pass. The row is written to the
    /// log so it outlives the session, and appended to the feed in hand so an
    /// open pane shows it without reading the log again. The write is detached:
    /// successive passes must not cancel each other's rows.
    pub fn note_incoming(&mut self, change: IncomingChange, cx: &mut Context<Self>) {
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            match store
                .record_incoming_change(change.provider.to_string(), change.description, cx)
                .await
            {
                Ok(entry) => {
                    this.update(cx, |this, cx| {
                        this.operations.push(entry);
                        if this.open {
                            cx.notify();
                        }
                    })
                    .ok();
                }
                Err(error) => {
                    tracing::warn!("could not record the incoming sync change: {error}");
                }
            }
        })
        .detach();
    }

    /// Re-read the persistent log for both providers, merging it into one
    /// newest-first list, and take the outstanding-failure count with it.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let loads: Vec<_> = SYNC_PROVIDERS
            .iter()
            .map(|provider| {
                self.store
                    .sync_op_history((*provider).to_string(), OPERATION_LIMIT, cx)
            })
            .collect();
        self._load = Some(cx.spawn(async move |this, cx| {
            let mut entries = Vec::new();
            // A partial read would understate what is owed, so the count only
            // moves when every provider answered.
            let mut owed = Some(0usize);
            for load in loads {
                match load.await {
                    Ok((mut rows, count)) => {
                        entries.append(&mut rows);
                        if let Some(owed) = owed.as_mut() {
                            *owed += count;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("could not read the sync operation log: {error}");
                        owed = None;
                    }
                }
            }
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
            this.update(cx, |this, cx| {
                this.operations = entries;
                if let Some(owed) = owed {
                    this.failed = owed;
                }
                this.loading = false;
                // Notified even while closed: the title bar's badge reads the
                // failure count, and it is on screen whether the pane is.
                cx.notify();
            })
            .ok();
        }));
    }

    /// The log as one timeline, newest first.
    fn feed(&self) -> Vec<&SyncOpLogEntry> {
        let mut rows: Vec<&SyncOpLogEntry> = self.operations.iter().collect();
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
        rows
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(history_icon(px(14.), rgb(theme::TEXT_MUTED))),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(theme::TEXT_STRONG))
                    .child("History"),
            )
            .child(
                Button::new("sync-history-refresh")
                    .ghost()
                    .compact()
                    .icon(IconName::RotateCw)
                    .tooltip("Re-read the sync log")
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
            .child(
                Button::new("sync-history-close")
                    .ghost()
                    .compact()
                    .icon(IconName::Close)
                    .tooltip("Hide the history pane")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .into_any_element()
    }

    fn empty_label(&self) -> &'static str {
        if self.loading {
            "Reading the sync log…"
        } else {
            "Nothing synced yet."
        }
    }

    /// One feed row: a status marker, what happened, its detail, and when.
    fn render_row(&self, index: usize, entry: &SyncOpLogEntry, now: jiff::Timestamp) -> AnyElement {
        let provider = provider_label(&entry.provider);
        let (icon, color, title, detail) = if entry.is_incoming() {
            // A pass that brought something back: no task, no kind worth
            // naming, just what it found.
            (
                IconName::ArrowDown,
                theme::TEXT_MUTED,
                format!("{provider} · incoming"),
                entry.incoming_summary().unwrap_or_default(),
            )
        } else {
            let kind = kind_label(&entry.kind);
            let title = match entry.task_id {
                Some(task_id) => format!("{provider} · {kind} · task #{task_id}"),
                None => format!("{provider} · {kind}"),
            };
            if entry.is_failed() {
                (
                    IconName::CircleX,
                    theme::DANGER,
                    title,
                    entry.error.clone().unwrap_or_default(),
                )
            } else {
                (
                    IconName::CircleCheck,
                    theme::SUCCESS,
                    title,
                    entry.payload.clone(),
                )
            }
        };
        let detail = truncate(&detail, DETAIL_MAX_CHARS);
        div()
            .id(("sync-history-row", index))
            .h_flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .pt_0p5()
                    .text_color(rgb(color))
                    .child(icon),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(title),
                    )
                    .when(!detail.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(since_label(entry.created_at, now)),
            )
            .into_any_element()
    }
}

impl Render for SyncHistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = jiff::Timestamp::now();
        let rows = self.feed();
        let mut list = div().v_flex().gap_2();
        if rows.is_empty() {
            list = list.child(
                div()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(self.empty_label()),
            );
        }
        for (index, entry) in rows.iter().enumerate() {
            list = list.child(self.render_row(index, entry, now));
        }
        div()
            .id("sync-history-pane")
            .size_full()
            .v_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_l_1()
            .border_color(rgb(theme::HAIRLINE))
            .bg(rgb(theme::PANEL_BG))
            .shadow_md()
            .child(self.header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(list),
            )
    }
}

/// The provider as a person writes it.
fn provider_label(provider: &str) -> &str {
    match provider {
        "github" => "GitHub",
        "todoist" => "Todoist",
        other => other,
    }
}

/// What a push was about, in words rather than the log's own key.
fn kind_label(kind: &str) -> &str {
    match kind {
        SYNC_OP_CAPTURE => "capture",
        SYNC_OP_LABELS => "labels",
        SYNC_OP_TASK_DONE => "completed",
        SYNC_OP_TASK_OPEN => "reopened",
        other => other,
    }
}

/// Cut a detail down to `max` characters, counting characters rather than
/// bytes so a multi-byte payload cannot be split.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_provider_labels_read_as_english() {
        assert_eq!(provider_label("github"), "GitHub");
        assert_eq!(provider_label("todoist"), "Todoist");
        // An unknown provider keeps its own name rather than disappearing.
        assert_eq!(provider_label("linear"), "linear");
        assert_eq!(kind_label(SYNC_OP_CAPTURE), "capture");
        assert_eq!(kind_label(SYNC_OP_TASK_DONE), "completed");
        assert_eq!(kind_label("something_new"), "something_new");
    }

    /// The pane prints the front of a detail, not a byte slice that could split
    /// a multi-byte character.
    #[test]
    fn long_details_are_truncated_on_character_boundaries() {
        assert_eq!(truncate("short", 10), "short");
        let long = "é".repeat(20);
        let cut = truncate(&long, 5);
        assert_eq!(cut.chars().count(), 6, "five kept plus the ellipsis");
        assert!(cut.ends_with('…'));
    }

    /// The badge stays narrow: a count past its ceiling is abbreviated rather
    /// than spelled out.
    #[test]
    fn the_badge_abbreviates_a_large_count() {
        assert_eq!(failed_badge_text(0), "0");
        assert_eq!(failed_badge_text(3), "3");
        assert_eq!(failed_badge_text(BADGE_MAX), "99");
        assert_eq!(failed_badge_text(BADGE_MAX + 1), "99+");
    }

    /// The tooltip says what the badge counts, and counts in the singular when
    /// there is exactly one.
    #[test]
    fn the_tooltip_explains_the_badge() {
        assert!(history_tooltip(0).contains("incoming changes"));
        assert_eq!(
            history_tooltip(1),
            "History — 1 failed sync operation waiting to be retried"
        );
        assert_eq!(
            history_tooltip(4),
            "History — 4 failed sync operations waiting to be retried"
        );
    }

    /// A pass that brought something back reads as an incoming row, whatever
    /// its provider.
    #[test]
    fn an_incoming_entry_carries_its_summary() {
        let entry = SyncOpLogEntry::incoming("todoist", "5 task(s), 1 section(s), 0 removed.");
        assert!(entry.is_incoming());
        assert_eq!(
            entry.incoming_summary().as_deref(),
            Some("5 task(s), 1 section(s), 0 removed.")
        );
    }
}
