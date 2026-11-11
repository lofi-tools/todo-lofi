use crate::tasks::Agent;

/// How a provider's window gets its start.
///
/// Codex rolls its windows forward from the last observed request. Claude
/// reports the reset time itself (a five-hour session and a fixed weekly
/// reset), so the pool never derives one from local usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolResetAnchor {
    LastObservedUse,
    ProviderReported,
}

/// One quota window as the provider defines it.
///
/// A limit says how much a window allows but not when it started. The window's
/// anchor decides whether the pool can derive a reset time locally.
pub struct PoolResetWindow {
    pub key: &'static str,
    pub label: &'static str,
    pub duration_ms: u64,
    pub anchor: PoolResetAnchor,
}

impl PoolResetWindow {
    /// The reset derived from local evidence, or `None` when only the provider
    /// knows the window's start.
    pub fn reset_at(&self, last_used_at: u64) -> Option<u64> {
        match self.anchor {
            PoolResetAnchor::LastObservedUse => Some(last_used_at.saturating_add(self.duration_ms)),
            PoolResetAnchor::ProviderReported => None,
        }
    }
}

/// The reset windows a built-in provider publishes.
pub struct PoolResetPolicy {
    windows: &'static [PoolResetWindow],
}

impl PoolResetPolicy {
    pub fn for_agent(agent: Agent) -> Self {
        let windows = match agent {
            Agent::Codex => CODEX_WINDOWS,
            Agent::Claude => CLAUDE_WINDOWS,
            // OpenCode and Pi borrow another provider's quota, and Freebuff is
            // a terminal wrapper, so no window belongs to their harness.
            Agent::OpenCode | Agent::Pi | Agent::Freebuff => &[],
        };
        Self { windows }
    }

    pub fn windows(&self) -> &'static [PoolResetWindow] {
        self.windows
    }

    pub fn window_by_key(&self, key: &str) -> Option<&'static PoolResetWindow> {
        self.windows.iter().find(|window| window.key == key)
    }

    /// The provider window a catalog period describes, when the period has a
    /// fixed length the provider also uses.
    pub fn window_for_period(&self, period: &str) -> Option<&'static PoolResetWindow> {
        let duration_ms = period_duration_ms(period)?;
        self.windows
            .iter()
            .find(|window| window.duration_ms == duration_ms)
    }
}

/// The length of a catalog reset period. A month has no fixed length, so it
/// stays unmapped instead of being approximated.
pub fn period_duration_ms(period: &str) -> Option<u64> {
    let trimmed = period.trim().to_ascii_lowercase();
    let key = trimmed.strip_suffix('s').unwrap_or(trimmed.as_str());
    let duration_ms = match key {
        "minute" => 60_000,
        "hour" => 3_600_000,
        "day" => 86_400_000,
        "week" => 604_800_000,
        _ => return None,
    };
    Some(duration_ms)
}

const CODEX_WINDOWS: &[PoolResetWindow] = &[
    PoolResetWindow {
        key: "five-hour",
        label: "5-hour",
        duration_ms: 5 * 3_600_000,
        anchor: PoolResetAnchor::LastObservedUse,
    },
    PoolResetWindow {
        key: "weekly",
        label: "Weekly",
        duration_ms: 7 * 86_400_000,
        anchor: PoolResetAnchor::LastObservedUse,
    },
];

const CLAUDE_WINDOWS: &[PoolResetWindow] = &[
    PoolResetWindow {
        key: "five-hour",
        label: "5-hour",
        duration_ms: 5 * 3_600_000,
        anchor: PoolResetAnchor::ProviderReported,
    },
    PoolResetWindow {
        key: "weekly",
        label: "Weekly",
        duration_ms: 7 * 86_400_000,
        anchor: PoolResetAnchor::ProviderReported,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_rolls_a_five_hour_and_a_weekly_window() {
        let policy = PoolResetPolicy::for_agent(Agent::Codex);
        let windows = policy.windows();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].key, "five-hour");
        assert_eq!(windows[0].reset_at(1_000), Some(1_000 + 5 * 3_600_000));
        assert_eq!(windows[1].key, "weekly");
        assert_eq!(windows[1].reset_at(1_000), Some(1_000 + 7 * 86_400_000));
    }

    #[test]
    fn claude_windows_take_their_reset_from_the_provider() {
        let policy = PoolResetPolicy::for_agent(Agent::Claude);
        let windows = policy.windows();
        assert_eq!(windows.len(), 2);
        assert!(windows.iter().all(|window| window.duration_ms > 0));
        assert!(
            windows
                .iter()
                .all(|window| window.anchor == PoolResetAnchor::ProviderReported)
        );
        assert!(
            windows
                .iter()
                .all(|window| window.reset_at(1_000).is_none())
        );
    }

    #[test]
    fn only_a_fixed_period_matches_a_provider_window() {
        let policy = PoolResetPolicy::for_agent(Agent::Codex);
        assert_eq!(
            policy.window_for_period("week").map(|window| window.key),
            Some("weekly")
        );
        assert_eq!(
            policy.window_for_period("7 days").map(|window| window.key),
            None
        );
        assert!(policy.window_for_period("month").is_none());
        assert!(policy.window_for_period("").is_none());
    }

    #[test]
    fn harnesses_without_their_own_quota_report_no_windows() {
        for agent in [Agent::OpenCode, Agent::Pi] {
            assert!(PoolResetPolicy::for_agent(agent).windows().is_empty());
        }
    }

    #[test]
    fn periods_map_to_whole_hours_and_days() {
        assert_eq!(period_duration_ms("Minutes"), Some(60_000));
        assert_eq!(period_duration_ms("hour"), Some(3_600_000));
        assert_eq!(period_duration_ms("days"), Some(86_400_000));
        assert_eq!(period_duration_ms("week"), Some(604_800_000));
        assert_eq!(period_duration_ms("month"), None);
        assert_eq!(period_duration_ms("rolling"), None);
    }
}
