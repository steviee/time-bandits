// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the agent knows, and when it should say something.
//!
//! Kept free of D-Bus and sockets so the rules about *when a child is spoken
//! to* can be tested directly. Those rules matter more than they look: an agent
//! that warns every few seconds is one a child learns to ignore, and an agent
//! that never warns turns a limit into an ambush.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use tb_proto::agent::{Focus, State};

/// How long a focus report from the compositor stays current.
///
/// The KWin script only speaks when the focused window changes, so silence is
/// normal — but silence that outlasts this means the script is gone, not that
/// nobody switched windows.
const FOCUS_FRESHNESS: Duration = Duration::from_secs(300);

/// Something worth telling the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announcement {
    /// Time is running out.
    Warning { remaining_secs: u64 },
    /// Access has just been refused.
    Blocked { message: String },
    /// Access has just come back — a parent granted time, or a new day started.
    Restored,
    /// Nothing to say.
    Nothing,
}

/// The agent's view of the world.
#[derive(Debug)]
pub struct AgentState {
    focus: Option<Focus>,
    focus_at: Option<Instant>,
    daemon: State,
    /// Warning thresholds already announced for the current allowance.
    announced: BTreeSet<u64>,
    blocked_announced: bool,
    last_remaining: Option<u64>,
}

impl AgentState {
    #[must_use]
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            focus: None,
            focus_at: None,
            daemon: State::unmanaged(subject),
            announced: BTreeSet::new(),
            blocked_announced: false,
            last_remaining: None,
        }
    }

    /// Records what the compositor says has focus.
    pub fn set_focus(&mut self, focus: Focus, at: Instant) {
        self.focus = Some(focus.sanitized());
        self.focus_at = Some(at);
    }

    /// Is the compositor script still reporting?
    #[must_use]
    pub fn focus_tracking(&self, now: Instant) -> bool {
        self.focus_at
            .is_some_and(|at| now.duration_since(at) < FOCUS_FRESHNESS)
    }

    /// The focus to send, or `None` when it is stale or titles are not wanted.
    #[must_use]
    pub fn focus_for_report(&self, now: Instant) -> Option<Focus> {
        if !self.focus_tracking(now) {
            return None;
        }
        let mut focus = self.focus.clone()?;
        // Titles are only ever sent when the daemon has said the policy allows
        // them. Deciding this here means a misconfigured agent cannot leak them.
        if !self.daemon.record_titles {
            focus.title = None;
        }
        Some(focus)
    }

    #[must_use]
    pub fn daemon_state(&self) -> &State {
        &self.daemon
    }

    /// Takes in the daemon's answer and decides what, if anything, to say.
    pub fn ingest(&mut self, state: State) -> Announcement {
        let was_blocked = self.daemon.blocked;
        let previous_remaining = self.last_remaining;
        self.last_remaining = state.remaining_secs;

        // More time than before means a new allowance: a parent granted bonus
        // time, a new policy day began, or a window opened. Everything already
        // said about the old allowance no longer applies.
        if let (Some(now), Some(before)) = (state.remaining_secs, previous_remaining)
            && now > before
        {
            self.announced.clear();
            self.blocked_announced = false;
        }

        self.daemon = state;

        if self.daemon.blocked {
            if self.blocked_announced {
                return Announcement::Nothing;
            }
            self.blocked_announced = true;
            return Announcement::Blocked {
                message: self
                    .daemon
                    .message
                    .clone()
                    .unwrap_or_else(|| "Screen time is up.".to_owned()),
            };
        }

        if was_blocked {
            self.blocked_announced = false;
            self.announced.clear();
            return Announcement::Restored;
        }

        let Some(remaining) = self.daemon.remaining_secs else {
            return Announcement::Nothing;
        };

        // Mark every threshold now passed, but speak once.
        let mut fresh = false;
        for threshold in self.daemon.crossed_thresholds(remaining) {
            if self.announced.insert(threshold) {
                fresh = true;
            }
        }
        if fresh {
            Announcement::Warning {
                remaining_secs: remaining,
            }
        } else {
            Announcement::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(remaining: Option<u64>) -> State {
        State {
            enforcement: true,
            remaining_secs: remaining,
            warn_at_secs: vec![900, 300, 60],
            ..State::unmanaged("kid")
        }
    }

    fn blocked(message: &str) -> State {
        State {
            enforcement: true,
            blocked: true,
            remaining_secs: Some(0),
            message: Some(message.to_owned()),
            warn_at_secs: vec![900, 300, 60],
            ..State::unmanaged("kid")
        }
    }

    #[test]
    fn plenty_of_time_left_says_nothing() {
        let mut s = AgentState::new("kid");
        assert_eq!(s.ingest(managed(Some(3600))), Announcement::Nothing);
    }

    #[test]
    fn each_threshold_is_announced_exactly_once() {
        let mut s = AgentState::new("kid");
        s.ingest(managed(Some(3600)));

        assert_eq!(
            s.ingest(managed(Some(800))),
            Announcement::Warning {
                remaining_secs: 800
            }
        );
        // Still under fifteen minutes: already said.
        assert_eq!(s.ingest(managed(Some(700))), Announcement::Nothing);
        assert_eq!(s.ingest(managed(Some(400))), Announcement::Nothing);
        // Under five minutes is a new threshold.
        assert_eq!(
            s.ingest(managed(Some(280))),
            Announcement::Warning {
                remaining_secs: 280
            }
        );
    }

    #[test]
    fn waking_up_past_several_thresholds_speaks_once() {
        // The machine slept through the fifteen and five minute marks.
        let mut s = AgentState::new("kid");
        s.ingest(managed(Some(3600)));
        assert_eq!(
            s.ingest(managed(Some(180))),
            Announcement::Warning {
                remaining_secs: 180
            }
        );
        assert_eq!(s.ingest(managed(Some(175))), Announcement::Nothing);
    }

    #[test]
    fn being_blocked_is_announced_once_not_every_poll() {
        let mut s = AgentState::new("kid");
        s.ingest(managed(Some(120)));
        assert_eq!(
            s.ingest(blocked("Time is up until 07:00.")),
            Announcement::Blocked {
                message: "Time is up until 07:00.".to_owned()
            }
        );
        for _ in 0..5 {
            assert_eq!(
                s.ingest(blocked("Time is up until 07:00.")),
                Announcement::Nothing
            );
        }
    }

    #[test]
    fn bonus_time_resets_what_has_been_said() {
        // Otherwise a child granted another hour is never warned again.
        let mut s = AgentState::new("kid");
        s.ingest(managed(Some(3600)));
        s.ingest(managed(Some(200)));
        assert_eq!(s.ingest(managed(Some(150))), Announcement::Nothing);

        // A parent grants half an hour.
        assert_eq!(s.ingest(managed(Some(1800))), Announcement::Nothing);
        // The five-minute mark can be announced again.
        assert_eq!(
            s.ingest(managed(Some(280))),
            Announcement::Warning {
                remaining_secs: 280
            }
        );
    }

    #[test]
    fn access_coming_back_is_worth_saying() {
        let mut s = AgentState::new("kid");
        s.ingest(managed(Some(60)));
        s.ingest(blocked("Time is up."));
        assert_eq!(s.ingest(managed(Some(1800))), Announcement::Restored);
    }

    #[test]
    fn an_unmanaged_user_is_never_spoken_to() {
        let mut s = AgentState::new("guest");
        for _ in 0..5 {
            assert_eq!(s.ingest(State::unmanaged("guest")), Announcement::Nothing);
        }
    }

    #[test]
    fn focus_goes_stale_when_the_compositor_stops_reporting() {
        let mut s = AgentState::new("kid");
        let t0 = Instant::now();
        assert!(!s.focus_tracking(t0), "nothing reported yet");

        s.set_focus(
            Focus {
                desktop_file: Some("org.mozilla.firefox".into()),
                ..Focus::default()
            },
            t0,
        );
        assert!(s.focus_tracking(t0 + Duration::from_secs(60)));
        assert!(
            !s.focus_tracking(t0 + FOCUS_FRESHNESS + Duration::from_secs(1)),
            "a script that stopped must not look like a quiet one forever"
        );
    }

    #[test]
    fn titles_are_withheld_unless_the_daemon_asks_for_them() {
        // The decision belongs to the parent, and is enforced on both sides.
        let mut s = AgentState::new("kid");
        let t0 = Instant::now();
        s.set_focus(
            Focus {
                desktop_file: Some("org.mozilla.firefox".into()),
                title: Some("Something private".into()),
                ..Focus::default()
            },
            t0,
        );

        s.ingest(managed(Some(3600)));
        assert_eq!(s.focus_for_report(t0).unwrap().title, None);

        s.ingest(State {
            record_titles: true,
            ..managed(Some(3600))
        });
        assert_eq!(
            s.focus_for_report(t0).unwrap().title.as_deref(),
            Some("Something private")
        );
    }

    #[test]
    fn stale_focus_is_not_reported_at_all() {
        let mut s = AgentState::new("kid");
        let t0 = Instant::now();
        s.set_focus(
            Focus {
                desktop_file: Some("firefox".into()),
                ..Focus::default()
            },
            t0,
        );
        s.ingest(managed(Some(3600)));
        assert!(s.focus_for_report(t0 + FOCUS_FRESHNESS).is_none());
    }
}
