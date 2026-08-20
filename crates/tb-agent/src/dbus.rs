// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The session-bus interface the widget reads and the compositor writes to.
//!
//! Two callers, opposite directions:
//!
//! * The KWin script calls [`AgentInterface::report_focus`] whenever the
//!   focused window changes. It is the only part of the system that is tied to
//!   a compositor, and it is a dozen lines of JavaScript.
//! * The plasmoid's C++ plugin reads the properties and follows
//!   `PropertiesChanged`, so the panel updates when something happens rather
//!   than when a timer fires.
//!
//! Everything here is advisory. The daemon enforces; this only reports.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tb_proto::agent::Focus;
use tb_proto::text::{Locale, deny_text};
use zbus::interface;
use zbus::object_server::SignalEmitter;

use crate::state::AgentState;

/// Where the interface lives on the session bus.
pub const BUS_NAME: &str = "org.timebandits.Agent1";
pub const OBJECT_PATH: &str = "/org/timebandits/Agent1";

/// One application's share of today, as the widget wants it.
pub type AppShare = (String, String, u32);
/// One day of the week: short name, allowance (-1 unlimited), used, is-today.
pub type DayShare = (String, i64, u32, bool);

/// The D-Bus face of the agent.
#[derive(Debug)]
pub struct AgentInterface {
    state: Arc<Mutex<AgentState>>,
}

impl AgentInterface {
    #[must_use]
    pub fn new(state: Arc<Mutex<AgentState>>) -> Self {
        Self { state }
    }

    fn with<T>(&self, f: impl FnOnce(&AgentState) -> T, fallback: T) -> T {
        self.state.lock().map_or(fallback, |s| f(&s))
    }
}

#[interface(name = "org.timebandits.Agent1")]
impl AgentInterface {
    /// Reports which window has focus.
    ///
    /// Called by the KWin script. Empty strings mean "not known", which is
    /// normal — some windows carry no desktop file name.
    fn report_focus(&self, desktop_file: String, resource_class: String, title: String) {
        let focus = Focus {
            desktop_file: (!desktop_file.is_empty()).then_some(desktop_file),
            resource_class: (!resource_class.is_empty()).then_some(resource_class),
            title: (!title.is_empty()).then_some(title),
        };
        if let Ok(mut state) = self.state.lock() {
            state.set_focus(focus, Instant::now());
        }
    }

    /// Whether the daemon has answered at all.
    ///
    /// False means the widget should say the service is not running rather
    /// than showing a reassuring zero, which would be the worse lie.
    #[zbus(property)]
    fn available(&self) -> bool {
        self.with(AgentState::has_answer, false)
    }

    #[zbus(property)]
    fn subject(&self) -> String {
        self.with(|s| s.daemon_state().subject.clone(), String::new())
    }

    /// Is anything actually being limited?
    #[zbus(property)]
    fn enforcement(&self) -> bool {
        self.with(|s| s.daemon_state().enforcement, false)
    }

    #[zbus(property)]
    fn blocked(&self) -> bool {
        self.with(|s| s.daemon_state().blocked, false)
    }

    /// Seconds left, or `-1` for unlimited. Signed because "no limit" and
    /// "no time" must not be the same number.
    #[zbus(property)]
    fn remaining_seconds(&self) -> i64 {
        self.with(
            |s| {
                s.daemon_state()
                    .remaining_secs
                    .map_or(-1, |v| i64::try_from(v).unwrap_or(i64::MAX))
            },
            -1,
        )
    }

    #[zbus(property)]
    fn used_today_seconds(&self) -> u32 {
        self.with(
            |s| u32::try_from(s.daemon_state().used_today_secs).unwrap_or(u32::MAX),
            0,
        )
    }

    /// The refusal, in this session's language.
    ///
    /// Composed here rather than taken from the daemon: the daemon runs as a
    /// systemd service and cannot know the child's locale.
    #[zbus(property)]
    fn message(&self) -> String {
        self.with(
            |s| {
                let d = s.daemon_state();
                match d.reason {
                    Some(reason) => deny_text(reason, &d.retry, Locale::from_env()),
                    None => d.message.clone().unwrap_or_default(),
                }
            },
            String::new(),
        )
    }

    /// Wall-clock time access returns, `HH:MM`, empty when not blocked.
    #[zbus(property)]
    fn retry_clock(&self) -> String {
        self.with(
            |s| s.daemon_state().retry.clock.clone().unwrap_or_default(),
            String::new(),
        )
    }

    /// Whether window titles are being recorded, so the widget can say so.
    #[zbus(property)]
    fn record_titles(&self) -> bool {
        self.with(|s| s.daemon_state().record_titles, false)
    }

    /// Whether the compositor script is reporting. `false` means the breakdown
    /// below is incomplete, and the widget should not pretend otherwise.
    #[zbus(property)]
    fn focus_tracking(&self) -> bool {
        self.with(|s| s.focus_tracking(Instant::now()), false)
    }

    /// Time per application today, longest first.
    #[zbus(property)]
    fn apps(&self) -> Vec<AppShare> {
        self.with(AgentState::apps, Vec::new())
    }

    /// `daily` or `weekly` — which budget the child is working against.
    #[zbus(property)]
    fn budget_kind(&self) -> String {
        self.with(|s| s.budget_kind().to_owned(), "daily".to_owned())
    }

    /// Seconds left in the week, or `-1` when there is no weekly budget.
    #[zbus(property)]
    fn weekly_remaining_seconds(&self) -> i64 {
        self.with(AgentState::weekly_remaining_secs, -1)
    }

    /// The week ahead, one entry per day.
    #[zbus(property)]
    fn week(&self) -> Vec<DayShare> {
        self.with(AgentState::week, Vec::new())
    }
}

impl AgentInterface {
    /// Tells listeners that everything may have changed.
    ///
    /// Coarse on purpose: the widget re-reads the handful of properties it
    /// shows, and one announcement beats tracking which of fifteen values
    /// moved. Each call is spelled out rather than iterated because zbus gives
    /// every generated notifier its own opaque future type.
    pub async fn announce(&self, emitter: &SignalEmitter<'_>) {
        let _ = self.available_changed(emitter).await;
        let _ = self.blocked_changed(emitter).await;
        let _ = self.enforcement_changed(emitter).await;
        let _ = self.remaining_seconds_changed(emitter).await;
        let _ = self.used_today_seconds_changed(emitter).await;
        let _ = self.message_changed(emitter).await;
        let _ = self.retry_clock_changed(emitter).await;
        let _ = self.record_titles_changed(emitter).await;
        let _ = self.focus_tracking_changed(emitter).await;
        let _ = self.apps_changed(emitter).await;
        let _ = self.week_changed(emitter).await;
        let _ = self.budget_kind_changed(emitter).await;
        let _ = self.weekly_remaining_seconds_changed(emitter).await;
    }
}
