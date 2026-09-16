//! App-wide notifications: the footer's error indicator and the pane that
//! lists what the app reported.
//!
//! Two things feed the log. Views report a failure they are about to show
//! inline, and a `tracing` layer turns the app's own `warn!`/`error!` events
//! into entries, so a failure that only reached the log file is visible too.
//! Every error is also shown as a toast when it is recorded; warnings and
//! informational messages stay in the pane and the footer's indicator.

use gpui::{App, Global};
use gpui_component::IconName;
use gpui_component::notification::Notification;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::theme;

/// Severity of one logged notification, ordered from least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

impl NoticeLevel {
    /// Noun for the pane's counts, plural against the count it labels.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Info => "messages",
            Self::Warning => "warnings",
            Self::Error => "errors",
        }
    }

    /// Icon for the list row and the footer's indicator.
    pub fn icon(self) -> IconName {
        match self {
            Self::Info => IconName::Info,
            Self::Warning => IconName::TriangleAlert,
            Self::Error => IconName::CircleX,
        }
    }

    /// Colour for the list row and the footer's indicator.
    pub fn color(self) -> u32 {
        match self {
            Self::Info => theme::TEXT_MUTED,
            Self::Warning => theme::WARNING,
            Self::Error => theme::DANGER,
        }
    }
}

/// One logged notification.
#[derive(Clone, Debug)]
pub struct Notice {
    pub level: NoticeLevel,
    pub message: String,
    pub at: jiff::Timestamp,
    /// How many identical messages arrived back to back after this one.
    pub repeats: u32,
}

impl Notice {
    /// The toast this notification raises, if any.
    ///
    /// Only errors interrupt. A warning is a quieter signal — a retry that
    /// fell back, a token about to go stale — and the pane is where it is
    /// already listed, so popping up for one would just add noise.
    pub fn toast(&self) -> Option<Notification> {
        match self.level {
            NoticeLevel::Error => Some(error_toast(self.message.clone())),
            NoticeLevel::Warning | NoticeLevel::Info => None,
        }
    }
}

/// The card one error is shown in.
///
/// Keyed by its message, so the same failure arriving twice — a view reports
/// what it just logged — replaces the card instead of stacking a second copy
/// of it, and a retry loop cannot pile up identical cards.
pub fn error_toast(message: impl Into<String>) -> Notification {
    let message = message.into();
    Notification::error(message.clone()).id1::<Notice>(message)
}

/// How many entries the log keeps; older ones fall off the front.
const NOTICE_CAPACITY: usize = 200;

/// The notification history the pane renders, oldest first.
#[derive(Default)]
pub struct NoticeLog {
    entries: Vec<Notice>,
}

impl NoticeLog {
    /// Record `message`, collapsing a repeat of the newest entry into a count so
    /// a retry loop cannot flood the pane. Returns whether this added an entry.
    pub fn push(&mut self, level: NoticeLevel, message: String, at: jiff::Timestamp) -> bool {
        if let Some(newest) = self.entries.last_mut()
            && newest.level == level
            && newest.message == message
        {
            newest.repeats += 1;
            newest.at = at;
            return false;
        }
        self.entries.push(Notice {
            level,
            message,
            at,
            repeats: 0,
        });
        if self.entries.len() > NOTICE_CAPACITY {
            self.entries.remove(0);
        }
        true
    }

    pub fn entries(&self) -> &[Notice] {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn count(&self, level: NoticeLevel) -> usize {
        self.entries
            .iter()
            .filter(|notice| notice.level == level)
            .count()
    }
}

/// What the pane lists: failures by default, everything on request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoticeFilter {
    /// Errors and warnings, the things worth acting on.
    #[default]
    Problems,
    /// Every logged message, informational ones included.
    All,
}

impl NoticeFilter {
    pub fn shows(self, level: NoticeLevel) -> bool {
        match self {
            Self::Problems => level != NoticeLevel::Info,
            Self::All => true,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Problems => "Errors & warnings",
            Self::All => "All",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Problems => Self::All,
            Self::All => Self::Problems,
        }
    }
}

/// Sending half of the notification feed.
///
/// Installed once as an app global, so a view can report a failure by adding a
/// line at the place it sets its own status instead of threading a handle
/// through every constructor.
#[derive(Clone)]
pub struct NoticeSink(UnboundedSender<Notice>);

impl Global for NoticeSink {}

impl NoticeSink {
    fn send(&self, level: NoticeLevel, message: impl Into<String>) {
        let notice = Notice {
            level,
            message: message.into(),
            at: jiff::Timestamp::now(),
            repeats: 0,
        };
        if let Err(error) = self.0.send(notice) {
            // Only closed once the UI is gone, at shutdown. `debug` keeps this
            // out of the log layer, which would otherwise feed itself.
            tracing::debug!("notification dropped: {error}");
        }
    }
}

/// Report a failure a view is about to show inline, so the footer's indicator
/// and the notifications pane see it too. Called from the place a view sets its
/// own status line.
pub fn report(cx: &App, level: NoticeLevel, message: impl Into<String>) {
    match cx.try_global::<NoticeSink>() {
        Some(sink) => sink.send(level, message),
        // No sink means a test or a headless run; the inline status the caller
        // is setting is the only place the message needs to appear.
        None => tracing::debug!("notification dropped: no sink installed"),
    }
}

/// Both halves of the feed: `main` installs a layer for the sink before the app
/// starts, and the layout drains the receiver.
pub struct NoticeFeed {
    sink: NoticeSink,
    receiver: UnboundedReceiver<Notice>,
}

impl NoticeFeed {
    pub fn new() -> Self {
        let (sink, receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            sink: NoticeSink(sink),
            receiver,
        }
    }

    /// The sending half: cloned into the tracing layer and set as the global.
    pub fn sink(&self) -> NoticeSink {
        self.sink.clone()
    }

    /// The receiving half, drained by the layout's foreground task.
    pub fn into_receiver(self) -> UnboundedReceiver<Notice> {
        self.receiver
    }
}

/// The crates whose warnings and errors the user can act on. A dependency's
/// (`gpui`, `toasty`, `reqwest`) noise would bury them.
const APP_TARGETS: [&str; 4] = ["todo_2", "storage", "acp_client", "gpui_tokio"];

fn is_app_target(target: &str) -> bool {
    APP_TARGETS.iter().any(|name| {
        target == *name
            || target
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

/// Turns the app's own `warn!`/`error!` events into notifications, so a failure
/// that only reached the log file is visible in the pane too.
pub struct NoticeLayer {
    sink: NoticeSink,
}

impl NoticeLayer {
    pub fn new(sink: NoticeSink) -> Self {
        Self { sink }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for NoticeLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::ERROR => NoticeLevel::Error,
            tracing::Level::WARN => NoticeLevel::Warning,
            // Info and below are the log file's business, not the pane's.
            _ => return,
        };
        if !is_app_target(metadata.target()) {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let Some(message) = visitor.message else {
            return;
        };
        self.sink.send(level, message);
    }
}

/// Pulls an event's `message` field out, the way the fmt layer formats it.
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(seconds).expect("timestamp")
    }

    #[test]
    fn a_repeated_message_collapses_into_one_entry() {
        let mut log = NoticeLog::default();
        assert!(log.push(NoticeLevel::Error, "sync failed".to_string(), at(1)));
        // The retry loop's second identical failure is not a second entry.
        assert!(!log.push(NoticeLevel::Error, "sync failed".to_string(), at(2)));
        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.entries()[0].repeats, 1);
        assert_eq!(log.entries()[0].at, at(2));
        // A different message starts a new entry.
        assert!(log.push(NoticeLevel::Warning, "retrying".to_string(), at(3)));
        assert_eq!(log.entries().len(), 2);
    }

    #[test]
    fn the_log_stops_growing_at_its_capacity() {
        let mut log = NoticeLog::default();
        for second in 0..(NOTICE_CAPACITY as i64 + 10) {
            log.push(NoticeLevel::Info, format!("message {second}"), at(second));
        }
        assert_eq!(log.entries().len(), NOTICE_CAPACITY);
        // The oldest entries fell off the front.
        assert_eq!(log.entries()[0].message, "message 10");
    }

    /// The toast policy: only errors interrupt, everything else waits in the
    /// pane (still counted by the footer's indicator).
    #[test]
    fn only_errors_raise_a_toast() {
        let notice = |level| Notice {
            level,
            message: "sync failed".to_string(),
            at: at(1),
            repeats: 0,
        };
        assert!(notice(NoticeLevel::Error).toast().is_some());
        assert!(notice(NoticeLevel::Warning).toast().is_none());
        assert!(notice(NoticeLevel::Info).toast().is_none());
    }

    #[test]
    fn problems_hide_informational_messages_until_asked_for() {
        assert!(!NoticeFilter::Problems.shows(NoticeLevel::Info));
        assert!(NoticeFilter::Problems.shows(NoticeLevel::Warning));
        assert!(NoticeFilter::Problems.shows(NoticeLevel::Error));
        assert!(NoticeFilter::All.shows(NoticeLevel::Info));
        assert_eq!(NoticeFilter::Problems.toggle(), NoticeFilter::All);
        assert_eq!(NoticeFilter::All.toggle(), NoticeFilter::Problems);
    }

    /// The path from a logged failure to the pane's feed, which is how a
    /// failure that only reached the log file becomes visible.
    #[test]
    fn a_logged_failure_reaches_the_feed() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let feed = NoticeFeed::new();
        let subscriber = tracing_subscriber::registry().with(NoticeLayer::new(feed.sink()));
        let _guard = tracing::subscriber::set_default(subscriber);

        tracing::error!("GitHub sync failed: Bad credentials");
        tracing::debug!("not a notification");

        let mut receiver = feed.into_receiver();
        let notice = receiver.try_recv().expect("the error was published");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert_eq!(notice.message, "GitHub sync failed: Bad credentials");
        assert!(receiver.try_recv().is_err(), "debug is not published");
    }

    #[test]
    fn only_the_apps_own_crates_reach_the_pane() {
        assert!(is_app_target("todo_2"));
        assert!(is_app_target("todo_2::ui_parts::integrations"));
        assert!(is_app_target("storage::github"));
        assert!(is_app_target("acp_client"));
        // A dependency's warning is not something the user can act on.
        assert!(!is_app_target("gpui_component::notification"));
        assert!(!is_app_target("storage_reports"));
    }
}
