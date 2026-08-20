// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Receives reports from `timebandits-agent`.
//!
//! This socket is reachable by unprivileged users, which the PAM one is not.
//! The rule that makes that safe is simple and absolute: **the sender's
//! identity comes from the kernel, never from the message.** `SO_PEERCRED`
//! gives the uid of the process on the other end, and that is what the daemon
//! resolves to a user name. A report has no field in which to claim otherwise.
//!
//! What arrives here is a hint, not a fact. logind already tells the daemon
//! whether a session exists and whether it is locked; the agent adds which
//! window has focus and how long the user has been idle. An agent that is
//! killed, or lies, costs attribution — never enforcement.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use jiff::{Timestamp, Zoned};
use tb_core::duration::DurationSpec;
use tb_core::engine::Verdict;
use tb_proto::agent::{MAX_MESSAGE_BYTES, Report, State, VERSION};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::store::Store;

/// A report together with when it arrived.
#[derive(Debug, Clone)]
pub struct Observed {
    pub report: Report,
    pub at: Timestamp,
}

/// The latest report from each user's agent.
///
/// Shared between the socket server and the tick loop. Deliberately small: it
/// holds one report per user and nothing historical, because anything older
/// than a few seconds is not evidence of what is on screen now.
#[derive(Debug, Clone, Default)]
pub struct AgentReports {
    inner: Arc<Mutex<HashMap<String, Observed>>>,
}

impl AgentReports {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, subject: &str, report: Report, at: Timestamp) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(subject.to_owned(), Observed { report, at });
        }
    }

    /// The latest report, if it is recent enough to describe the present.
    ///
    /// A stale entry is treated as no report at all. That is what makes a
    /// killed agent indistinguishable from one that never existed — both mean
    /// the daemon stops attributing time and says so.
    #[must_use]
    pub fn fresh(&self, subject: &str, now: Timestamp, max_age: DurationSpec) -> Option<Report> {
        let map = self.inner.lock().ok()?;
        let observed = map.get(subject)?;
        let age = observed.at.duration_until(now).as_secs();
        if u64::try_from(age).unwrap_or(u64::MAX) > max_age.as_secs() {
            return None;
        }
        Some(observed.report.clone())
    }

    /// Drops entries for users who have not reported in a long while, so a
    /// machine with many past users does not accumulate them forever.
    pub fn expire(&self, now: Timestamp, max_age: DurationSpec) {
        if let Ok(mut map) = self.inner.lock() {
            map.retain(|_, o| {
                let age = o.at.duration_until(now).as_secs();
                u64::try_from(age).unwrap_or(u64::MAX) <= max_age.as_secs()
            });
        }
    }
}

/// Resolves a uid to a user name through NSS.
///
/// Returns `None` for a uid with no passwd entry, which is the right answer for
/// a process the daemon should ignore rather than guess about.
// The one place in the daemon that needs unsafe: there is no safe wrapper for
// `getpwuid_r` in the dependency set, and pulling one in for a dozen lines is a
// worse trade than reviewing these.
#[allow(unsafe_code)]
#[must_use]
pub fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0i8; 1024];
    // SAFETY: `passwd` is a C struct of plain data and raw pointers, for which
    // an all-zero bit pattern is a valid value. `getpwuid_r` overwrites it
    // before anything reads it.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();

    // SAFETY: buffer and length agree; `getpwuid_r` writes only within them and
    // sets `result` to null when there is no such user.
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut pwd,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: on success `pw_name` points into `buf` and is NUL-terminated.
    unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Handles reports and answers with the user's current state.
#[derive(Debug, Clone)]
pub struct Receiver {
    store: Arc<Mutex<Store>>,
    reports: AgentReports,
}

impl Receiver {
    #[must_use]
    pub fn new(store: Arc<Mutex<Store>>, reports: AgentReports) -> Self {
        Self { store, reports }
    }

    #[must_use]
    pub fn reports(&self) -> AgentReports {
        self.reports.clone()
    }

    /// Records one report and produces the answer.
    ///
    /// `subject` comes from the socket's peer credentials. Nothing in `report`
    /// influences whose data is touched.
    #[must_use]
    pub fn accept(&self, subject: &str, report: Report, now: &Zoned) -> State {
        let report = Report {
            focus: report.focus.map(tb_proto::agent::Focus::sanitized),
            ..report
        };
        self.reports.record(subject, report, now.timestamp());
        self.state_for(subject, now)
    }

    /// The state to hand back: how much time is left and what to tell the child.
    #[must_use]
    pub fn state_for(&self, subject: &str, now: &Zoned) -> State {
        let Ok(store) = self.store.lock() else {
            return State::unmanaged(subject);
        };
        let Ok(Some(policy)) = store.load_policy(subject) else {
            return State::unmanaged(subject);
        };
        let Ok((snapshot, _day)) = store.snapshot(&policy, now) else {
            return State::unmanaged(subject);
        };

        let mut state = State {
            enforcement: policy.enforcement,
            record_titles: policy.record_window_titles,
            used_today_secs: snapshot.used_today.as_secs(),
            warn_at_secs: policy
                .sorted_warnings()
                .iter()
                .map(|w| w.as_secs())
                .collect(),
            ..State::unmanaged(subject)
        };

        match tb_core::evaluate(&policy, &snapshot, now) {
            Verdict::Allowed(a) => {
                state.remaining_secs = a.remaining.map(DurationSpec::as_secs);
            }
            Verdict::Denied(d) => {
                state.blocked = true;
                state.remaining_secs = Some(0);
                state.message = Some(crate::pamserver::deny_message(&d, now));
            }
        }
        state
    }
}

/// Binds the agent socket.
///
/// Mode `0666`, because every child's agent has to reach it. That is safe only
/// because of the peer-credential rule: being able to connect grants the right
/// to report about yourself and nobody else.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => tracing::warn!(?path, "removed a stale agent socket"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    tracing::info!(?path, "listening for agent reports");
    Ok(listener)
}

/// Serves agent connections until cancelled.
pub async fn serve(listener: UnixListener, receiver: Receiver) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let receiver = receiver.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, &receiver).await {
                        tracing::debug!(error = %e, "agent connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(stream: UnixStream, receiver: &Receiver) -> anyhow::Result<()> {
    // Before anything is read: who is on the other end?
    let uid = stream.peer_cred()?.uid();
    let Some(subject) = user_name(uid) else {
        anyhow::bail!("no user for uid {uid}");
    };

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).take(MAX_MESSAGE_BYTES as u64);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let now = Zoned::now();
    let state = match serde_json::from_str::<Report>(line.trim()) {
        Ok(report) if report.v == VERSION => receiver.accept(&subject, report, &now),
        Ok(report) => {
            tracing::warn!(their = report.v, ours = VERSION, %subject, "unsupported agent version");
            receiver.state_for(&subject, &now)
        }
        Err(e) => {
            tracing::debug!(error = %e, %subject, "unparseable agent report");
            receiver.state_for(&subject, &now)
        }
    };

    let mut out = serde_json::to_string(&state)?;
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;
    use tb_core::policy::{Policy, Quota};
    use tb_core::schedule::WeekSchedule;
    use tb_core::usage::UsageSegment;
    use tb_proto::agent::Focus;
    use uuid::Uuid;

    fn tz() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("Europe/Berlin").unwrap()
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .unwrap()
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(1_756_000_000 + secs).unwrap()
    }

    fn policy_2h() -> Policy {
        let mut p = Policy::permissive("kid");
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    fn receiver_with(policy: &Policy, used_mins: i64, from: &Zoned) -> Receiver {
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
        Receiver::new(Arc::new(Mutex::new(store)), AgentReports::new())
    }

    #[test]
    fn a_fresh_report_is_returned_and_a_stale_one_is_not() {
        let reports = AgentReports::new();
        reports.record("kid", Report::new(), ts(0));

        assert!(
            reports
                .fresh("kid", ts(10), DurationSpec::from_secs(30))
                .is_some()
        );
        assert!(
            reports
                .fresh("kid", ts(120), DurationSpec::from_secs(30))
                .is_none(),
            "a killed agent must look the same as one that never ran"
        );
    }

    #[test]
    fn reports_are_kept_per_user() {
        let reports = AgentReports::new();
        let mut kid = Report::new();
        kid.idle_secs = 5;
        let mut sibling = Report::new();
        sibling.idle_secs = 300;
        reports.record("kid", kid, ts(0));
        reports.record("sibling", sibling, ts(0));

        let max = DurationSpec::from_secs(30);
        assert_eq!(reports.fresh("kid", ts(1), max).unwrap().idle_secs, 5);
        assert_eq!(reports.fresh("sibling", ts(1), max).unwrap().idle_secs, 300);
        assert!(reports.fresh("nobody", ts(1), max).is_none());
    }

    #[test]
    fn expiry_clears_users_who_stopped_reporting() {
        let reports = AgentReports::new();
        reports.record("kid", Report::new(), ts(0));
        reports.record("sibling", Report::new(), ts(1000));
        reports.expire(ts(1010), DurationSpec::from_secs(60));
        assert!(
            reports
                .fresh("kid", ts(1010), DurationSpec::from_secs(9999))
                .is_none()
        );
        assert!(
            reports
                .fresh("sibling", ts(1010), DurationSpec::from_secs(60))
                .is_some()
        );
    }

    #[test]
    fn a_report_is_filed_under_the_peer_not_anything_it_claims() {
        // The security property. `accept` takes the subject as an argument, so
        // there is no path from message content to whose data is touched.
        let r = receiver_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let now = at(2026, 8, 19, 16, 0);
        let mut report = Report::new();
        report.idle_secs = 42;
        let state = r.accept("kid", report, &now);

        assert_eq!(state.subject, "kid");
        // Queried against the same clock the report was filed with: mixing a
        // fixed fixture clock with the real one makes the test fail whenever
        // the fixture date drifts into the past.
        let stored = r
            .reports()
            .fresh("kid", now.timestamp(), DurationSpec::from_secs(60));
        assert_eq!(stored.map(|s| s.idle_secs), Some(42));
    }

    #[test]
    fn titles_are_sanitized_on_arrival() {
        // The agent runs as the child, so what it sends is attacker-controlled.
        let r = receiver_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let mut report = Report::new();
        report.focus = Some(Focus {
            desktop_file: Some("org.mozilla.firefox".into()),
            title: Some(format!("evil\n{}", "x".repeat(9999))),
            ..Focus::default()
        });
        let now = at(2026, 8, 19, 16, 0);
        let _ = r.accept("kid", report, &now);

        let stored = r
            .reports()
            .fresh("kid", now.timestamp(), DurationSpec::from_secs(60))
            .unwrap();
        let title = stored.focus.unwrap().title.unwrap();
        assert!(!title.contains('\n'));
        assert!(title.chars().count() <= tb_proto::agent::MAX_TITLE_LEN);
    }

    #[test]
    fn an_unmanaged_user_gets_a_harmless_answer() {
        let r = receiver_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let state = r.accept("guest", Report::new(), &at(2026, 8, 19, 16, 0));
        assert_eq!(state.subject, "guest");
        assert!(!state.enforcement);
        assert!(!state.blocked);
        assert_eq!(state.remaining_secs, None);
    }

    #[test]
    fn the_state_carries_remaining_time_for_the_plasmoid() {
        let r = receiver_with(&policy_2h(), 90, &at(2026, 8, 19, 15, 0));
        let state = r.state_for("kid", &at(2026, 8, 19, 17, 0));
        assert!(state.enforcement);
        assert_eq!(state.remaining_secs, Some(30 * 60));
        assert_eq!(state.used_today_secs, 90 * 60);
        assert!(!state.blocked);
        assert_eq!(
            state.warn_at_secs,
            vec![900, 300, 60],
            "the policy decides when to warn, not the agent"
        );
    }

    #[test]
    fn a_blocked_child_is_told_why_and_until_when() {
        let r = receiver_with(&policy_2h(), 120, &at(2026, 8, 19, 15, 0));
        let state = r.state_for("kid", &at(2026, 8, 19, 18, 0));
        assert!(state.blocked);
        assert_eq!(state.remaining_secs, Some(0));
        let message = state.message.expect("a message");
        assert!(message.contains("used up"), "{message}");
        assert!(message.contains("04:00"), "{message}");
    }

    #[test]
    fn titles_are_only_requested_when_the_policy_allows_them() {
        // Collection is opt-in, and the agent takes its cue from here.
        let r = receiver_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        assert!(!r.state_for("kid", &at(2026, 8, 19, 16, 0)).record_titles);

        let mut p = policy_2h();
        p.record_window_titles = true;
        p.version = 2;
        let r = receiver_with(&p, 0, &at(2026, 8, 19, 15, 0));
        assert!(r.state_for("kid", &at(2026, 8, 19, 16, 0)).record_titles);
    }

    #[allow(unsafe_code)]
    #[test]
    fn the_current_uid_resolves_to_a_name() {
        // Sanity check on the NSS wrapper: whoever runs the tests exists.
        // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
        let me = user_name(unsafe { libc::getuid() });
        assert!(me.is_some(), "the running user must resolve");
        assert!(user_name(4_294_967_294).is_none(), "nobody has this uid");
    }

    #[allow(unsafe_code)]
    #[tokio::test]
    async fn the_socket_identifies_the_peer_and_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = bind(&path).unwrap();

        let receiver = receiver_with(&policy_2h(), 0, &at(2026, 8, 19, 15, 0));
        let reports = receiver.reports();
        tokio::spawn(serve(listener, receiver));

        let stream = UnixStream::connect(&path).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut report = Report::new();
        report.idle_secs = 7;
        report.focus_tracking = true;
        let line = serde_json::to_string(&report).unwrap();
        w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        w.flush().await.unwrap();

        let mut response = String::new();
        BufReader::new(r).read_line(&mut response).await.unwrap();
        let state: State = serde_json::from_str(response.trim()).unwrap();

        // The daemon filed it under the running user, which the test never told it.
        // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
        let me = user_name(unsafe { libc::getuid() }).unwrap();
        assert_eq!(state.subject, me);
        // This one legitimately uses the real clock: the server stamped the
        // report with Zoned::now() itself.
        assert!(
            reports
                .fresh(&me, Timestamp::now(), DurationSpec::from_secs(60))
                .is_some(),
            "the report was filed under the peer's identity"
        );
    }
}
