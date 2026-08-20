// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The protocol between `timebandits-agent` and `timebanditsd`.
//!
//! Unlike the PAM socket, this one is reachable by unprivileged users — it has
//! to be, because the agent runs as the child. That shapes the whole design:
//!
//! **A report does not say who it is about.** There is no user field, and no
//! session field the daemon trusts. The daemon reads the peer's uid from the
//! socket itself and resolves the name from that. A child cannot report time on
//! their sibling's behalf, because there is nowhere in the message to claim to
//! be them.
//!
//! **Nothing a report says is authoritative.** It is a hint that improves
//! accuracy. The daemon already knows, from logind, whether a session exists
//! and whether it is locked; the agent adds which application has focus and how
//! long the user has been idle. An agent that lies, or is killed, costs
//! attribution — never enforcement.

use serde::{Deserialize, Serialize};

/// Where the daemon listens for agent reports.
pub const SOCKET_PATH: &str = "/run/timebandits/agent.sock";

/// Protocol version, bumped only on incompatible changes.
pub const VERSION: u32 = 1;

/// Cap on a single message. Window titles are attacker-controlled text from the
/// agent's point of view, so the size is bounded before it reaches a parser.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Longest window title that will be accepted. Titles are only recorded when a
/// parent switches it on, and even then a novel's worth of them is not wanted
/// in the database.
pub const MAX_TITLE_LEN: usize = 200;

/// The focused window, as the compositor describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Focus {
    /// `desktopFileName` — the most precise identifier available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_file: Option<String>,
    /// `resourceClass` — coarser, but almost always present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_class: Option<String>,
    /// Window title. Only sent when the daemon has said titles are recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Focus {
    /// Trims anything oversized. Called on the receiving side, because a
    /// well-behaved agent is not something to rely on.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        fn clip(s: Option<String>, max: usize) -> Option<String> {
            s.map(|mut v| {
                if v.chars().count() > max {
                    v = v.chars().take(max).collect();
                }
                // Control characters in a title would corrupt log output and
                // anything that later renders it.
                v.retain(|c| !c.is_control());
                v
            })
            .filter(|v| !v.trim().is_empty())
        }
        self.desktop_file = clip(self.desktop_file, 128);
        self.resource_class = clip(self.resource_class, 128);
        self.title = clip(self.title, MAX_TITLE_LEN);
        self
    }
}

/// What the agent sends, once per interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub v: u32,
    /// The focused window, if any. Absent means nothing is focused, or the
    /// compositor script is not running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Focus>,
    /// Seconds since the last input event.
    #[serde(default)]
    pub idle_secs: u64,
    /// Whether the agent believes the screen is locked. Cross-checked against
    /// logind, which the daemon trusts more.
    #[serde(default)]
    pub locked: bool,
    /// Whether the compositor script is reporting focus. `false` means the
    /// agent is running but blind — the script was never loaded or was
    /// disabled — which is worth distinguishing from no agent at all.
    #[serde(default)]
    pub focus_tracking: bool,
}

impl Report {
    #[must_use]
    pub fn new() -> Self {
        Self {
            v: VERSION,
            focus: None,
            idle_secs: 0,
            locked: false,
            focus_tracking: false,
        }
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

/// One application's share of the day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppUsage {
    /// Normalised identifier, e.g. `org.mozilla.firefox`, or `unknown`.
    pub id: String,
    /// What to call it on screen. Falls back to the identifier when no better
    /// name is known.
    pub name: String,
    pub secs: u64,
}

/// One day of the policy week, as the widget shows it.
///
/// With a weekly budget `allowance_secs` is `None` for every day — there is no
/// per-day allocation, and inventing one would be showing a rule that does not
/// exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayOutlook {
    /// Short weekday name, already in the daemon's own words. The front end
    /// translates it; two letters is all the strip has room for.
    pub weekday: String,
    /// Time allowed on this day, or `None` for unlimited or weekly budgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance_secs: Option<u64>,
    pub used_secs: u64,
    /// The policy day in progress.
    #[serde(default)]
    pub today: bool,
    /// Still to come this week.
    #[serde(default)]
    pub future: bool,
}

/// Which budget the child is working against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    /// Each day carries its own allowance.
    #[default]
    Daily,
    /// One pot for the week, the child divides it.
    Weekly,
}

/// What the daemon tells the agent, so it can warn the child and feed the
/// plasmoid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub v: u32,
    /// The user the daemon resolved from the socket's peer credentials — not
    /// from anything the report claimed.
    pub subject: String,
    /// Is anything being enforced at all?
    pub enforcement: bool,
    /// Seconds left, or absent when unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_secs: Option<u64>,
    /// Currently blocked.
    #[serde(default)]
    pub blocked: bool,
    /// English text, for a front end that cannot localise. A current one
    /// composes its own from `reason` and `retry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Why access was refused, for the front end to put into words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<crate::text::Reason>,
    /// When it returns, as facts.
    #[serde(default, skip_serializing_if = "crate::text::RetryAt::is_empty")]
    pub retry: crate::text::RetryAt,
    /// Whether the daemon wants window titles. The agent must not send them
    /// otherwise — collection is opt-in, and the decision belongs to the parent.
    #[serde(default)]
    pub record_titles: bool,
    /// Seconds of usage recorded today, for the plasmoid.
    #[serde(default)]
    pub used_today_secs: u64,
    /// Remaining-time thresholds at which the child should be warned, in
    /// seconds, descending. Sent by the daemon so the policy stays the single
    /// source of truth rather than the agent carrying its own idea of when to
    /// speak up.
    #[serde(default)]
    pub warn_at_secs: Vec<u64>,
    /// Where today's time went, longest first.
    #[serde(default)]
    pub apps: Vec<AppUsage>,
    /// The policy week, Monday first.
    #[serde(default)]
    pub week: Vec<DayOutlook>,
    #[serde(default)]
    pub budget_kind: BudgetKind,
    /// Seconds left in the week, when there is a weekly budget at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_remaining_secs: Option<u64>,
}

impl State {
    /// The answer for a user nobody manages.
    #[must_use]
    pub fn unmanaged(subject: impl Into<String>) -> Self {
        Self {
            v: VERSION,
            subject: subject.into(),
            enforcement: false,
            remaining_secs: None,
            blocked: false,
            message: None,
            reason: None,
            retry: crate::text::RetryAt::default(),
            record_titles: false,
            used_today_secs: 0,
            warn_at_secs: Vec::new(),
            apps: Vec::new(),
            week: Vec::new(),
            budget_kind: BudgetKind::Daily,
            weekly_remaining_secs: None,
        }
    }

    /// Whether a warning is due when this much time is left, given what has
    /// already been announced.
    ///
    /// Every threshold at or above `remaining` counts as crossed, but only one
    /// notification is produced. A machine that was asleep through the 15 and 5
    /// minute marks and wakes at 3 should say one thing, not three.
    #[must_use]
    pub fn crossed_thresholds(&self, remaining_secs: u64) -> Vec<u64> {
        let mut crossed: Vec<u64> = self
            .warn_at_secs
            .iter()
            .copied()
            .filter(|&t| remaining_secs <= t)
            .collect();
        crossed.sort_unstable_by(|a, b| b.cmp(a));
        crossed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_carries_no_claim_about_who_sent_it() {
        // The property the whole design rests on: there is nowhere in the
        // message to claim to be another user.
        let json = serde_json::to_string(&Report::new()).unwrap();
        for forbidden in ["user", "subject", "uid", "session"] {
            assert!(
                !json.contains(forbidden),
                "a report must not carry `{forbidden}`: {json}"
            );
        }
    }

    #[test]
    fn a_report_stays_on_one_line() {
        let mut r = Report::new();
        r.focus = Some(Focus {
            desktop_file: Some("org.mozilla.firefox".into()),
            resource_class: Some("firefox".into()),
            title: None,
        });
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains('\n'));
    }

    #[test]
    fn oversized_titles_are_clipped() {
        let focus = Focus {
            title: Some("x".repeat(10_000)),
            ..Focus::default()
        }
        .sanitized();
        assert_eq!(focus.title.unwrap().chars().count(), MAX_TITLE_LEN);
    }

    #[test]
    fn control_characters_are_stripped_from_titles() {
        // A window title is text the child chooses. Newlines in it would break
        // every log line and every report that renders it afterwards.
        let focus = Focus {
            title: Some("evil\ntitle\u{0}with\rcontrols".into()),
            ..Focus::default()
        }
        .sanitized();
        assert_eq!(focus.title.unwrap(), "eviltitlewithcontrols");
    }

    #[test]
    fn blank_fields_become_absent() {
        let focus = Focus {
            desktop_file: Some("   ".into()),
            resource_class: Some(String::new()),
            title: Some("\u{0}".into()),
        }
        .sanitized();
        assert_eq!(focus, Focus::default());
    }

    #[test]
    fn clipping_never_splits_a_character() {
        // Truncating by bytes in the middle of a multi-byte character would
        // produce invalid UTF-8 or a panic; this clips by character.
        let focus = Focus {
            title: Some("ä".repeat(500)),
            ..Focus::default()
        }
        .sanitized();
        let title = focus.title.unwrap();
        assert_eq!(title.chars().count(), MAX_TITLE_LEN);
        assert!(title.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn crossed_thresholds_reports_every_mark_passed() {
        let state = State {
            warn_at_secs: vec![900, 300, 60],
            ..State::unmanaged("kid")
        };
        assert_eq!(state.crossed_thresholds(1200), Vec::<u64>::new());
        assert_eq!(state.crossed_thresholds(600), vec![900]);
        // Woken from sleep with three minutes left: two marks were passed, and
        // the caller announces once.
        assert_eq!(state.crossed_thresholds(180), vec![900, 300]);
        assert_eq!(state.crossed_thresholds(0), vec![900, 300, 60]);
    }

    #[test]
    fn a_weekly_budget_reports_no_per_day_allowance() {
        // The distinction the widget depends on: with a weekly pot there is no
        // daily allocation, so every day says None rather than a made-up share.
        let state = State {
            budget_kind: BudgetKind::Weekly,
            weekly_remaining_secs: Some(12_000),
            week: vec![DayOutlook {
                weekday: "monday".into(),
                allowance_secs: None,
                used_secs: 3600,
                today: true,
                future: false,
            }],
            ..State::unmanaged("kid")
        };
        let back: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(back.budget_kind, BudgetKind::Weekly);
        assert_eq!(back.week[0].allowance_secs, None);
        assert_eq!(back.weekly_remaining_secs, Some(12_000));
    }

    #[test]
    fn state_round_trips() {
        let s = State {
            remaining_secs: Some(1800),
            message: Some("30 minutes left".into()),
            ..State::unmanaged("kid")
        };
        let back: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_report_from_a_newer_agent_still_parses() {
        let line = r#"{"v":1,"idle_secs":5,"locked":false,"battery_saver":true}"#;
        let r: Report = serde_json::from_str(line).expect("unknown fields ignored");
        assert_eq!(r.idle_secs, 5);
    }
}
