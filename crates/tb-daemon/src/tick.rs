// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The enforcement loop: observe, record, decide, act.
//!
//! Runs a few times a minute and is the only place that changes a session's
//! state. Two things keep it honest:
//!
//! * It is a *state machine*, not a set of conditions re-evaluated from scratch.
//!   Locking is an edge, not a level — without that, a child would be sent a
//!   fresh lock request every five seconds for the rest of the evening.
//! * It takes the clock as an argument. Every escalation path, including the
//!   grace period and the day rollover, is tested by handing it timestamps.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use jiff::{Timestamp, Zoned, civil};
use tb_core::appid::{self, AppObservation};
use tb_core::duration::DurationSpec;
use tb_core::engine::{DenyReason, Verdict};
use tb_core::policy::{LockAction, Policy};
use tb_core::usage::{SegmentBuilder, SegmentConfig, Tick as UsageTick};

use crate::agentserver::AgentReports;
use crate::config::Config;
use crate::logind::{SessionControl, SessionInfo, by_user};
use crate::store::{EventKind, Store};

/// How the daemon tells a child what is happening.
///
/// Warnings can only be delivered by something inside the session, which is the
/// untrusted side of the system. That is acceptable: a missed warning is a
/// usability problem, never an enforcement one — the lock happens either way.
pub trait Notifier {
    /// Time is running low.
    fn warn(&self, subject: &str, remaining: DurationSpec);
    /// The session is about to be locked or ended.
    fn closing(&self, subject: &str, reason: DenyReason, grace: DurationSpec);
}

/// Writes to the log. Used until the session agent exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn warn(&self, subject: &str, remaining: DurationSpec) {
        tracing::info!(user = subject, %remaining, "screen time running low");
    }
    fn closing(&self, subject: &str, reason: DenyReason, grace: DurationSpec) {
        tracing::info!(user = subject, ?reason, %grace, "closing session");
    }
}

/// Where one user stands in the escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing to do; time may or may not be running.
    Running,
    /// Time is up and the session was closed; waiting out the grace period
    /// before ending it for real.
    Grace { since: Timestamp },
    /// Fully enforced. Getting back in has to go through PAM.
    Enforced,
}

#[derive(Debug)]
struct UserState {
    phase: Phase,
    /// Warning thresholds, in seconds, already delivered for `warned_on`.
    warned: BTreeSet<u64>,
    warned_on: Option<civil::Date>,
    builder: SegmentBuilder,
    /// When this user was last ticked, used to clamp a backdated idle start.
    last_tick: Option<Timestamp>,
    /// Whether the current lack of tracking has already been recorded. Without
    /// this the event log would gain a row every few seconds for as long as a
    /// child leaves the agent stopped.
    tamper_reported: bool,
}

impl UserState {
    fn new(subject: &str, cfg: SegmentConfig) -> Self {
        Self {
            phase: Phase::Running,
            warned: BTreeSet::new(),
            warned_on: None,
            builder: SegmentBuilder::new(subject, cfg),
            last_tick: None,
            tamper_reported: false,
        }
    }
}

/// The enforcement loop.
#[derive(Debug)]
pub struct Ticker<S, N> {
    store: Arc<Mutex<Store>>,
    sessions: S,
    notifier: N,
    reports: AgentReports,
    states: HashMap<String, UserState>,
    segment_config: SegmentConfig,
    /// How old an agent report may be and still describe the present.
    report_max_age: DurationSpec,
}

impl<S: SessionControl, N: Notifier> Ticker<S, N> {
    #[must_use]
    pub fn new(
        store: Arc<Mutex<Store>>,
        sessions: S,
        notifier: N,
        reports: AgentReports,
        cfg: &Config,
    ) -> Self {
        Self {
            store,
            sessions,
            notifier,
            reports,
            states: HashMap::new(),
            // Three intervals of silence is a stopped agent, not a slow one.
            report_max_age: DurationSpec::from_secs(cfg.tick_interval.as_secs() * 3),
            segment_config: SegmentConfig {
                tick_interval: cfg.tick_interval,
                // A gap of more than a few ticks means the machine was asleep or
                // the daemon was stopped. Six intervals tolerates a slow tick
                // without crediting a suspend.
                max_gap: DurationSpec::from_secs(cfg.tick_interval.as_secs() * 6),
                ..SegmentConfig::default()
            },
        }
    }

    /// One pass. Errors from a single user are logged and do not stop the others:
    /// one broken policy must not silently switch off enforcement for a sibling.
    pub fn tick(&mut self, now: &Zoned) -> anyhow::Result<()> {
        let sessions = by_user(self.sessions.sessions()?);
        let subjects = {
            let store = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
            store.subjects()?
        };

        for subject in subjects {
            let empty = Vec::new();
            let user_sessions = sessions.get(&subject).unwrap_or(&empty);
            if let Err(e) = self.tick_user(&subject, user_sessions, now) {
                tracing::error!(user = %subject, error = %e, "tick failed for this user");
            }
        }
        Ok(())
    }

    fn tick_user(
        &mut self,
        subject: &str,
        sessions: &[SessionInfo],
        now: &Zoned,
    ) -> anyhow::Result<()> {
        let policy = {
            let store = self.lock_store()?;
            match store.load_policy(subject)? {
                Some(p) => p,
                None => return Ok(()),
            }
        };

        // logind decides whether anybody is at the machine. The agent only
        // refines *what* they are doing, and cannot make time count that logind
        // says is not being spent.
        let at_work = sessions.iter().any(SessionInfo::is_creditable_desktop);
        let now_ts = now.timestamp();
        let report = self.reports.fresh(subject, now_ts, self.report_max_age);
        let tracking_blind = at_work && report.as_ref().is_none_or(|r| !r.focus_tracking);

        let seg_cfg = self.segment_config;
        let (closed, announce_tamper) = {
            let state = self
                .states
                .entry(subject.to_owned())
                .or_insert_with(|| UserState::new(subject, seg_cfg));

            let tick = build_tick(state, &policy, at_work, now_ts, report.as_ref());
            state.last_tick = Some(now_ts);

            // Reported once per transition. Logging every tick would add a row
            // every few seconds for as long as a child leaves the agent stopped.
            let announce = tracking_blind && !state.tamper_reported;
            state.tamper_reported = tracking_blind;

            (state.builder.observe(&tick), announce)
        };

        if let Some(closed) = closed {
            let store = self.lock_store()?;
            store.insert_segment(&closed.segment)?;
            tracing::debug!(
                user = subject,
                app = %closed.segment.app,
                duration = %closed.segment.duration(),
                reason = ?closed.reason,
                "segment recorded"
            );
        }

        if announce_tamper {
            let detail = if report.is_none() {
                "session agent is not reporting"
            } else {
                "agent is running but the compositor script is not reporting focus"
            };
            tracing::warn!(user = subject, detail, "tracking is blind");
            if let Ok(store) = self.store.lock() {
                let _ = store.record_event(subject, EventKind::Tamper, Some(detail));
            }
        }

        // --- decide ------------------------------------------------------
        if Config::enforcement_disabled() {
            return Ok(());
        }

        // A household that has chosen it can treat blind tracking as reason
        // enough to stop the session, rather than accepting unattributed time.
        if tracking_blind && policy.on_tamper == tb_core::TamperResponse::LockImmediately {
            return self.enforce(
                subject,
                sessions,
                &policy,
                DenyReason::DailyQuotaExhausted,
                now,
            );
        }

        let (snapshot, day) = {
            let store = self.lock_store()?;
            store.snapshot(&policy, now)?
        };
        self.apply_verdict(
            subject,
            sessions,
            &Situation {
                policy: &policy,
                snapshot: &snapshot,
                day,
                at_work,
            },
            now,
        )
    }

    /// Acts on the engine's verdict: warn, restore, or enforce.
    fn apply_verdict(
        &mut self,
        subject: &str,
        sessions: &[SessionInfo],
        situation: &Situation<'_>,
        now: &Zoned,
    ) -> anyhow::Result<()> {
        let Situation {
            policy,
            snapshot,
            day,
            at_work,
        } = *situation;
        match tb_core::evaluate(policy, snapshot, now) {
            Verdict::Allowed(allowance) => {
                let Some(state) = self.states.get_mut(subject) else {
                    return Ok(());
                };
                // Warning thresholds are per policy day, so a new day starts quiet.
                if state.warned_on != Some(day.date) {
                    state.warned.clear();
                    state.warned_on = Some(day.date);
                }
                if state.phase != Phase::Running {
                    // Access came back: a parent granted bonus time, a new day
                    // began, or an allowed window opened.
                    tracing::info!(user = subject, "access restored");
                    state.phase = Phase::Running;
                }

                if let Some(remaining) = allowance.remaining
                    && at_work
                {
                    // Mark every threshold now crossed, but speak once. If the
                    // machine was asleep and jumps from 20 to 3 minutes left,
                    // that is one warning, not three.
                    let mut announce = false;
                    for threshold in policy.sorted_warnings() {
                        if remaining <= threshold && state.warned.insert(threshold.as_secs()) {
                            announce = true;
                        }
                    }
                    if announce {
                        self.notifier.warn(subject, remaining);
                    }
                }
                Ok(())
            }
            Verdict::Denied(denial) => self.enforce(subject, sessions, policy, denial.reason, now),
        }
    }

    fn lock_store(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))
    }

    fn enforce(
        &mut self,
        subject: &str,
        sessions: &[SessionInfo],
        policy: &Policy,
        reason: DenyReason,
        now: &Zoned,
    ) -> anyhow::Result<()> {
        let lockable: Vec<&SessionInfo> = sessions.iter().filter(|s| s.is_lockable()).collect();

        let Some(state) = self.states.get_mut(subject) else {
            return Ok(());
        };

        if lockable.is_empty() {
            // Nobody is logged in. Reset, so the next login is handled as a
            // fresh event rather than being mistaken for an ongoing lockout.
            state.phase = Phase::Running;
            return Ok(());
        }

        match state.phase {
            Phase::Running => {
                let terminates = matches!(
                    policy.on_exhausted,
                    LockAction::Terminate | LockAction::LockThenTerminate
                );
                let grace = if terminates {
                    policy.grace_period
                } else {
                    DurationSpec::ZERO
                };
                self.notifier.closing(subject, reason, grace);

                // Plain `Terminate` deliberately does not lock first: the whole
                // point of that setting is to leave the screen usable during the
                // grace period so open work can be saved.
                if policy.on_exhausted != LockAction::Terminate {
                    lock_all(&self.sessions, &lockable, subject);
                }
                {
                    let store = self
                        .store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
                    store.record_event(subject, EventKind::Locked, Some(&format!("{reason:?}")))?;
                }
                let state = self.states.get_mut(subject).expect("just checked");
                state.phase = if terminates {
                    Phase::Grace {
                        since: now.timestamp(),
                    }
                } else {
                    Phase::Enforced
                };
            }

            Phase::Grace { since } => {
                let waited = since.duration_until(now.timestamp()).as_secs();
                if u64::try_from(waited).unwrap_or(0) >= policy.grace_period.as_secs() {
                    for s in &lockable {
                        if let Err(e) = self.sessions.terminate(&s.id) {
                            tracing::error!(user = subject, session = %s.id, error = %e, "terminate failed");
                        } else {
                            tracing::info!(user = subject, session = %s.id, "session terminated");
                        }
                    }
                    let state = self.states.get_mut(subject).expect("just checked");
                    state.phase = Phase::Enforced;
                }
            }

            Phase::Enforced => {
                // Belt and braces. PAM should make unlocking impossible, but if
                // a session is somehow unlocked again, lock it rather than
                // trusting that it cannot happen.
                let stray: Vec<&SessionInfo> = lockable.into_iter().filter(|s| !s.locked).collect();
                if !stray.is_empty() && policy.on_exhausted != LockAction::Terminate {
                    tracing::warn!(
                        user = subject,
                        count = stray.len(),
                        "session unlocked while enforced"
                    );
                    lock_all(&self.sessions, &stray, subject);
                }
            }
        }
        Ok(())
    }

    /// Flushes open segments — on shutdown, so the last stretch is not lost.
    pub fn flush(&mut self, now: &Zoned) {
        for (subject, state) in &mut self.states {
            if let Some(closed) = state.builder.flush(now.timestamp())
                && let Ok(store) = self.store.lock()
                && let Err(e) = store.insert_segment(&closed.segment)
            {
                tracing::error!(user = %subject, error = %e, "could not save final segment");
            }
        }
    }
}

/// Everything the verdict depends on, bundled so the call does not need eight
/// positional arguments.
#[derive(Debug, Clone, Copy)]
struct Situation<'a> {
    policy: &'a Policy,
    snapshot: &'a tb_core::engine::UsageSnapshot,
    day: tb_core::PolicyDay,
    at_work: bool,
}

/// What this tick represents, given what logind and the agent each say.
fn build_tick(
    state: &UserState,
    policy: &Policy,
    at_work: bool,
    now_ts: Timestamp,
    report: Option<&tb_proto::agent::Report>,
) -> UsageTick {
    if !at_work {
        return UsageTick::Idle { at: now_ts };
    }
    match report {
        Some(r) if r.idle_secs >= policy.idle_threshold.as_secs() => {
            // Crediting stops when the inactivity began, not when it was
            // noticed — otherwise the idle threshold itself is charged to the
            // child every time they walk away. Clamped to the previous tick so
            // a backdated instant cannot look like a clock running backwards.
            let began = now_ts
                .checked_sub(jiff::SignedDuration::from_secs(
                    i64::try_from(r.idle_secs).unwrap_or(0),
                ))
                .unwrap_or(now_ts);
            let at = state.last_tick.map_or(began, |last| began.max(last));
            UsageTick::Idle { at }
        }
        Some(r) => UsageTick::Active {
            at: now_ts,
            app: r.focus.as_ref().map_or_else(AppObservation::unknown, |f| {
                appid::observe_window(f.desktop_file.as_deref(), f.resource_class.as_deref())
            }),
            title: if policy.record_window_titles {
                r.focus.as_ref().and_then(|f| f.title.clone())
            } else {
                None
            },
        },
        // No agent: the time is real, so it counts. What is lost is knowing
        // which application it belongs to.
        None => UsageTick::Active {
            at: now_ts,
            app: AppObservation::unknown(),
            title: None,
        },
    }
}

fn lock_all<S: SessionControl>(sessions: &S, targets: &[&SessionInfo], subject: &str) {
    for s in targets {
        if let Err(e) = sessions.lock(&s.id) {
            tracing::error!(user = subject, session = %s.id, error = %e, "lock failed");
        } else {
            tracing::info!(user = subject, session = %s.id, "session locked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tb_core::policy::Quota;
    use tb_core::schedule::WeekSchedule;
    use tb_core::usage::UsageSegment;
    use uuid::Uuid;

    /// Records what the loop asked logind to do.
    #[derive(Debug, Default)]
    struct FakeSessions {
        sessions: RefCell<Vec<SessionInfo>>,
        locked: RefCell<Vec<String>>,
        terminated: RefCell<Vec<String>>,
    }

    impl FakeSessions {
        fn with(sessions: Vec<SessionInfo>) -> Self {
            Self {
                sessions: RefCell::new(sessions),
                ..Self::default()
            }
        }
    }

    impl SessionControl for &FakeSessions {
        fn sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(self.sessions.borrow().clone())
        }
        fn lock(&self, id: &str) -> anyhow::Result<()> {
            self.locked.borrow_mut().push(id.to_owned());
            // logind's LockSession makes the desktop report itself as locked.
            for s in self.sessions.borrow_mut().iter_mut() {
                if s.id == id {
                    s.locked = true;
                }
            }
            Ok(())
        }
        fn terminate(&self, id: &str) -> anyhow::Result<()> {
            self.terminated.borrow_mut().push(id.to_owned());
            self.sessions.borrow_mut().retain(|s| s.id != id);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeNotifier {
        warnings: RefCell<Vec<(String, DurationSpec)>>,
        closings: RefCell<Vec<(String, DenyReason)>>,
    }

    impl Notifier for &FakeNotifier {
        fn warn(&self, subject: &str, remaining: DurationSpec) {
            self.warnings
                .borrow_mut()
                .push((subject.to_owned(), remaining));
        }
        fn closing(&self, subject: &str, reason: DenyReason, _grace: DurationSpec) {
            self.closings
                .borrow_mut()
                .push((subject.to_owned(), reason));
        }
    }

    fn tz() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("Europe/Berlin").unwrap()
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .unwrap()
    }

    fn desktop(user: &str) -> SessionInfo {
        SessionInfo {
            id: format!("{user}-1"),
            uid: 1000,
            user: user.to_owned(),
            active: true,
            locked: false,
            class: "user".to_owned(),
            kind: "wayland".to_owned(),
            remote: false,
        }
    }

    fn policy_2h() -> Policy {
        let mut p = Policy::permissive("kid");
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    /// A store holding `policy` plus `used_mins` of usage starting at `from`.
    fn store_with(policy: &Policy, used_mins: i64, from: &Zoned) -> Arc<Mutex<Store>> {
        let store = Store::in_memory().unwrap();
        store.save_policy(policy).unwrap();
        if used_mins > 0 {
            let start = from.timestamp();
            store
                .insert_segment(&UsageSegment {
                    id: Uuid::now_v7(),
                    subject: policy.subject.clone(),
                    app: tb_core::AppId::unknown(),
                    source: tb_core::AppIdSource::Unknown,
                    start,
                    end: start
                        .checked_add(jiff::SignedDuration::from_secs(used_mins * 60))
                        .unwrap(),
                    title: None,
                })
                .unwrap();
        }
        Arc::new(Mutex::new(store))
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn time_is_recorded_while_a_desktop_session_is_open() {
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(
            store.clone(),
            &sessions,
            &notifier,
            AgentReports::new(),
            &cfg(),
        );

        let base = at(2026, 8, 19, 15, 0);
        for i in 0..4 {
            let now = base.checked_add(jiff::Span::new().seconds(i * 5)).unwrap();
            ticker.tick(&now).unwrap();
        }
        ticker.flush(&base.checked_add(jiff::Span::new().seconds(20)).unwrap());

        let used = store
            .lock()
            .unwrap()
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(used, DurationSpec::from_secs(20));
    }

    #[test]
    fn no_session_means_no_time_counted() {
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::default();
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(
            store.clone(),
            &sessions,
            &notifier,
            AgentReports::new(),
            &cfg(),
        );

        for i in 0..10 {
            ticker
                .tick(
                    &at(2026, 8, 19, 15, 0)
                        .checked_add(jiff::Span::new().seconds(i * 5))
                        .unwrap(),
                )
                .unwrap();
        }
        let used = store
            .lock()
            .unwrap()
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(used, DurationSpec::ZERO);
    }

    #[test]
    fn an_exhausted_quota_locks_the_session_once() {
        let start = at(2026, 8, 19, 13, 0);
        let store = store_with(&policy_2h(), 120, &start);
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        // Twenty ticks over the exhausted quota.
        for i in 0..20 {
            ticker
                .tick(
                    &at(2026, 8, 19, 16, 0)
                        .checked_add(jiff::Span::new().seconds(i * 5))
                        .unwrap(),
                )
                .unwrap();
        }

        assert_eq!(
            sessions.locked.borrow().len(),
            1,
            "locking is an edge, not a level — one lock, not twenty"
        );
        assert_eq!(notifier.closings.borrow().len(), 1);
        assert_eq!(
            notifier.closings.borrow()[0].1,
            DenyReason::DailyQuotaExhausted
        );
        assert!(
            sessions.terminated.borrow().is_empty(),
            "Lock must not end the session"
        );
    }

    #[test]
    fn warnings_fire_once_per_threshold() {
        // 1h50m used of 2h: ten minutes left, so the 15-minute threshold is
        // already crossed and the 5-minute one is not.
        let start = at(2026, 8, 19, 14, 0);
        let store = store_with(&policy_2h(), 110, &start);
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        for i in 0..5 {
            ticker
                .tick(
                    &at(2026, 8, 19, 16, 0)
                        .checked_add(jiff::Span::new().seconds(i * 5))
                        .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            notifier.warnings.borrow().len(),
            1,
            "one warning, not one per tick"
        );
    }

    #[test]
    fn a_big_jump_in_remaining_time_still_warns_only_once() {
        // The machine slept through 15 and 5 minutes left and wakes at 3.
        let start = at(2026, 8, 19, 14, 0);
        let store = store_with(&policy_2h(), 117, &start);
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());
        ticker.tick(&at(2026, 8, 19, 16, 0)).unwrap();
        assert_eq!(notifier.warnings.borrow().len(), 1);
        assert_eq!(notifier.warnings.borrow()[0].1, DurationSpec::from_mins(3));
    }

    #[test]
    fn terminate_waits_out_the_grace_period() {
        let mut p = policy_2h();
        p.on_exhausted = LockAction::LockThenTerminate;
        p.grace_period = DurationSpec::from_secs(60);

        let store = store_with(&p, 120, &at(2026, 8, 19, 13, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        let base = at(2026, 8, 19, 16, 0);
        ticker.tick(&base).unwrap();
        assert_eq!(sessions.locked.borrow().len(), 1, "locked immediately");
        assert!(sessions.terminated.borrow().is_empty(), "not yet ended");

        // Half a minute in: still just locked.
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(30)).unwrap())
            .unwrap();
        assert!(sessions.terminated.borrow().is_empty());

        // Past the grace period.
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(61)).unwrap())
            .unwrap();
        assert_eq!(sessions.terminated.borrow().len(), 1);
    }

    #[test]
    fn plain_terminate_leaves_the_screen_usable_during_grace() {
        // The point of Terminate without Lock is that open work can be saved.
        let mut p = policy_2h();
        p.on_exhausted = LockAction::Terminate;
        p.grace_period = DurationSpec::from_secs(60);

        let store = store_with(&p, 120, &at(2026, 8, 19, 13, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        let base = at(2026, 8, 19, 16, 0);
        ticker.tick(&base).unwrap();
        assert!(sessions.locked.borrow().is_empty(), "must not lock");
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(61)).unwrap())
            .unwrap();
        assert_eq!(sessions.terminated.borrow().len(), 1);
    }

    #[test]
    fn bonus_time_restores_access_without_a_restart() {
        let store = store_with(&policy_2h(), 120, &at(2026, 8, 19, 13, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(
            store.clone(),
            &sessions,
            &notifier,
            AgentReports::new(),
            &cfg(),
        );

        let base = at(2026, 8, 19, 16, 0);
        ticker.tick(&base).unwrap();
        assert_eq!(sessions.locked.borrow().len(), 1);

        // A parent grants half an hour.
        {
            let s = store.lock().unwrap();
            let day = tb_core::schedule::policy_day(&base, civil::time(4, 0, 0, 0));
            s.add_bonus("kid", day, DurationSpec::from_mins(30), "mum")
                .unwrap();
        }
        // Unlocking is the child's job; the daemon just stops enforcing.
        sessions.sessions.borrow_mut()[0].locked = false;

        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(5)).unwrap())
            .unwrap();
        assert_eq!(
            sessions.locked.borrow().len(),
            1,
            "no further lock once access is restored"
        );
    }

    #[test]
    fn a_session_unlocked_while_enforced_is_locked_again() {
        let store = store_with(&policy_2h(), 120, &at(2026, 8, 19, 13, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        let base = at(2026, 8, 19, 16, 0);
        ticker.tick(&base).unwrap();
        assert_eq!(sessions.locked.borrow().len(), 1);

        // Something got past the lock screen. PAM should prevent this; the loop
        // does not rely on that being true.
        sessions.sessions.borrow_mut()[0].locked = false;
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(5)).unwrap())
            .unwrap();
        assert_eq!(sessions.locked.borrow().len(), 2, "locked again");
    }

    #[test]
    fn logging_out_resets_the_escalation() {
        let store = store_with(&policy_2h(), 120, &at(2026, 8, 19, 13, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        let base = at(2026, 8, 19, 16, 0);
        ticker.tick(&base).unwrap();
        assert_eq!(sessions.locked.borrow().len(), 1);

        // Child logs out, then logs back in later.
        sessions.sessions.borrow_mut().clear();
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(5)).unwrap())
            .unwrap();
        sessions.sessions.borrow_mut().push(desktop("kid"));
        ticker
            .tick(&base.checked_add(jiff::Span::new().seconds(10)).unwrap())
            .unwrap();

        assert_eq!(
            sessions.locked.borrow().len(),
            2,
            "the new session is locked as its own event"
        );
    }

    #[test]
    fn the_greeter_is_never_locked() {
        let mut p = policy_2h();
        p.subject = "sddm".to_owned();
        let store = store_with(&p, 120, &at(2026, 8, 19, 13, 0));
        let mut greeter = desktop("sddm");
        greeter.class = "greeter".to_owned();
        let sessions = FakeSessions::with(vec![greeter]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        ticker.tick(&at(2026, 8, 19, 16, 0)).unwrap();
        assert!(
            sessions.locked.borrow().is_empty(),
            "locking the greeter would make the machine unusable for everybody"
        );
    }

    #[test]
    fn one_broken_user_does_not_stop_the_others() {
        let store = store_with(&policy_2h(), 120, &at(2026, 8, 19, 13, 0));
        {
            let s = store.lock().unwrap();
            let mut sibling = policy_2h();
            sibling.subject = "sibling".to_owned();
            s.save_policy(&sibling).unwrap();
        }
        let sessions = FakeSessions::with(vec![desktop("kid"), desktop("sibling")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        ticker.tick(&at(2026, 8, 19, 16, 0)).unwrap();
        // kid is over quota and locked; sibling has used nothing and is not.
        assert_eq!(sessions.locked.borrow().as_slice(), ["kid-1"]);
    }

    // --- what the agent contributes ---------------------------------------

    /// A ticker whose agent reports whatever `report` says.
    fn ticker_with_report<'a>(
        store: Arc<Mutex<Store>>,
        sessions: &'a FakeSessions,
        notifier: &'a FakeNotifier,
        report: Option<tb_proto::agent::Report>,
        at: &Zoned,
    ) -> (Ticker<&'a FakeSessions, &'a FakeNotifier>, AgentReports) {
        let reports = AgentReports::new();
        if let Some(r) = report {
            reports.record("kid", r, at.timestamp());
        }
        (
            Ticker::new(store, sessions, notifier, reports.clone(), &cfg()),
            reports,
        )
    }

    fn focused(app: &str) -> tb_proto::agent::Report {
        tb_proto::agent::Report {
            focus_tracking: true,
            focus: Some(tb_proto::agent::Focus {
                desktop_file: Some(app.to_owned()),
                resource_class: None,
                title: Some("a window title".to_owned()),
            }),
            ..tb_proto::agent::Report::new()
        }
    }

    #[test]
    fn focus_from_the_agent_is_attributed_to_the_application() {
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, reports) = ticker_with_report(
            store.clone(),
            &sessions,
            &notifier,
            Some(focused("org.mozilla.firefox")),
            &base,
        );

        for i in 0..3 {
            let now = base.checked_add(jiff::Span::new().seconds(i * 5)).unwrap();
            reports.record("kid", focused("org.mozilla.firefox"), now.timestamp());
            ticker.tick(&now).unwrap();
        }
        ticker.flush(&base.checked_add(jiff::Span::new().seconds(15)).unwrap());

        let segments = store
            .lock()
            .unwrap()
            .segments_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert!(!segments.is_empty());
        assert!(
            segments
                .iter()
                .all(|s| s.app.as_str() == "org.mozilla.firefox"),
            "got {:?}",
            segments
                .iter()
                .map(|s| s.app.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn window_titles_are_only_stored_when_the_policy_allows_it() {
        for (record, expected) in [(false, None), (true, Some("a window title".to_owned()))] {
            let mut p = policy_2h();
            p.record_window_titles = record;
            let store = store_with(&p, 0, &at(2026, 8, 19, 15, 0));
            let sessions = FakeSessions::with(vec![desktop("kid")]);
            let notifier = FakeNotifier::default();
            let base = at(2026, 8, 19, 15, 0);
            let (mut ticker, _) = ticker_with_report(
                store.clone(),
                &sessions,
                &notifier,
                Some(focused("firefox")),
                &base,
            );
            ticker.tick(&base).unwrap();
            ticker.flush(&base.checked_add(jiff::Span::new().seconds(10)).unwrap());

            let segments = store
                .lock()
                .unwrap()
                .segments_between(
                    "kid",
                    at(2026, 8, 19, 0, 0).timestamp(),
                    at(2026, 8, 20, 0, 0).timestamp(),
                )
                .unwrap();
            assert_eq!(
                segments[0].title, expected,
                "record_window_titles = {record}"
            );
        }
    }

    #[test]
    fn an_idle_report_stops_the_clock_at_the_moment_idleness_began() {
        // Charging the idle threshold itself would cost the child two minutes
        // every time they walk away from the machine.
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, reports) =
            ticker_with_report(store.clone(), &sessions, &notifier, None, &base);

        // Two active ticks, then a report saying idle began 120 s ago.
        for i in 0..2 {
            let now = base.checked_add(jiff::Span::new().seconds(i * 5)).unwrap();
            reports.record("kid", focused("firefox"), now.timestamp());
            ticker.tick(&now).unwrap();
        }
        let idle_at = base.checked_add(jiff::Span::new().seconds(10)).unwrap();
        reports.record(
            "kid",
            tb_proto::agent::Report {
                idle_secs: 120,
                focus_tracking: true,
                ..tb_proto::agent::Report::new()
            },
            idle_at.timestamp(),
        );
        ticker.tick(&idle_at).unwrap();

        let used = store
            .lock()
            .unwrap()
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        // Clamped to the last tick, so the segment ends at 5 s rather than
        // being backdated to before it began.
        assert_eq!(used, DurationSpec::from_secs(5), "got {used}");
    }

    #[test]
    fn without_an_agent_time_still_counts_but_as_unknown() {
        // Killing the agent must cost attribution, never enforcement.
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, _) = ticker_with_report(store.clone(), &sessions, &notifier, None, &base);

        for i in 0..3 {
            ticker
                .tick(&base.checked_add(jiff::Span::new().seconds(i * 5)).unwrap())
                .unwrap();
        }
        ticker.flush(&base.checked_add(jiff::Span::new().seconds(15)).unwrap());

        let store = store.lock().unwrap();
        let used = store
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(used, DurationSpec::from_secs(15));
        let segments = store
            .segments_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert!(segments.iter().all(|s| s.app.is_unknown()));
        assert!(store.event_count("kid", EventKind::Tamper).unwrap() >= 1);
    }

    #[test]
    fn a_blind_agent_is_reported_once_not_every_tick() {
        // Otherwise a child who disables the KWin script fills the event log
        // with a row every few seconds all evening.
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, reports) =
            ticker_with_report(store.clone(), &sessions, &notifier, None, &base);

        // Agent alive, but the compositor script is not reporting focus.
        for i in 0..10 {
            let now = base.checked_add(jiff::Span::new().seconds(i * 5)).unwrap();
            reports.record(
                "kid",
                tb_proto::agent::Report {
                    focus_tracking: false,
                    ..tb_proto::agent::Report::new()
                },
                now.timestamp(),
            );
            ticker.tick(&now).unwrap();
        }

        let count = store
            .lock()
            .unwrap()
            .event_count("kid", EventKind::Tamper)
            .unwrap();
        assert_eq!(count, 1, "one event for one episode, not one per tick");
    }

    #[test]
    fn tracking_recovers_and_a_second_episode_is_reported_again() {
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, reports) =
            ticker_with_report(store.clone(), &sessions, &notifier, None, &base);

        let blind = tb_proto::agent::Report::new();
        for (i, report) in [blind.clone(), focused("firefox"), blind]
            .into_iter()
            .enumerate()
        {
            let now = base
                .checked_add(jiff::Span::new().seconds(i64::try_from(i).unwrap_or(0) * 5))
                .unwrap();
            reports.record("kid", report, now.timestamp());
            ticker.tick(&now).unwrap();
        }

        let count = store
            .lock()
            .unwrap()
            .event_count("kid", EventKind::Tamper)
            .unwrap();
        assert_eq!(count, 2, "two separate episodes");
    }

    #[test]
    fn strict_households_can_lock_when_tracking_goes_blind() {
        let mut p = policy_2h();
        p.on_tamper = tb_core::TamperResponse::LockImmediately;
        let store = store_with(&p, 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, _) = ticker_with_report(store, &sessions, &notifier, None, &base);

        ticker.tick(&base).unwrap();
        assert_eq!(sessions.locked.borrow().len(), 1);
    }

    #[test]
    fn the_default_response_to_blind_tracking_is_to_keep_going() {
        // CountAndReport is the default for a reason: a flaky agent must not
        // lock a child out of a machine they are entitled to use.
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        let (mut ticker, _) = ticker_with_report(store, &sessions, &notifier, None, &base);

        ticker.tick(&base).unwrap();
        assert!(sessions.locked.borrow().is_empty());
    }

    #[test]
    fn a_stale_report_is_treated_as_no_agent_at_all() {
        let store = store_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let base = at(2026, 8, 19, 15, 0);
        // Recorded a full minute before the tick; the window is three intervals.
        let (mut ticker, _) = ticker_with_report(
            store.clone(),
            &sessions,
            &notifier,
            Some(focused("firefox")),
            &base.checked_sub(jiff::Span::new().minutes(1)).unwrap(),
        );

        ticker.tick(&base).unwrap();
        ticker.flush(&base.checked_add(jiff::Span::new().seconds(5)).unwrap());

        let segments = store
            .lock()
            .unwrap()
            .segments_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert!(segments.iter().all(|s| s.app.is_unknown()), "{segments:?}");
    }

    #[test]
    fn observe_only_policies_never_lock() {
        let p = Policy::permissive("kid"); // enforcement = false
        let store = store_with(&p, 10_000, &at(2026, 8, 19, 5, 0));
        let sessions = FakeSessions::with(vec![desktop("kid")]);
        let notifier = FakeNotifier::default();
        let mut ticker = Ticker::new(store, &sessions, &notifier, AgentReports::new(), &cfg());

        ticker.tick(&at(2026, 8, 19, 20, 0)).unwrap();
        assert!(sessions.locked.borrow().is_empty());
        assert!(notifier.closings.borrow().is_empty());
    }
}
