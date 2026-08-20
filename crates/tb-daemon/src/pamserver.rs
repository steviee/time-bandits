// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Answers `pam_timebandits.so` over a Unix socket.
//!
//! Every request here sits in somebody's login path, so the handler does the
//! minimum: one database read, one pure evaluation, one line back. No network,
//! no locks held across an await, no work that can grow with the size of the
//! history.
//!
//! The socket is `0600` and owned by root. Every PAM stack we integrate with —
//! `sddm`, `login`, `sshd`, and `kcheckpass` for the lock screen — runs as root,
//! so nothing legitimate needs wider access.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use jiff::Zoned;
use tb_core::engine::{Denial, DenyReason, Verdict};
use tb_proto::pam::{Answer, Query, VERSION};
use tb_proto::text::{Locale, Reason, RetryAt, deny_text};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::config::Config;
use crate::store::{EventKind, Store};
use crate::users::Membership;

/// Shared state the handler needs.
#[derive(Debug, Clone)]
pub struct Responder {
    store: Arc<Mutex<Store>>,
    membership: Arc<dyn Membership + Send + Sync>,
    managed_group: String,
}

impl Responder {
    #[must_use]
    pub fn new(
        store: Arc<Mutex<Store>>,
        membership: Arc<dyn Membership + Send + Sync>,
        managed_group: impl Into<String>,
    ) -> Self {
        Self {
            store,
            membership,
            managed_group: managed_group.into(),
        }
    }

    /// Decides one query. Pure apart from the database read, so it is tested
    /// directly rather than through a socket.
    #[must_use]
    pub fn answer(&self, query: &Query, now: &Zoned) -> Answer {
        // The emergency brake outranks everything, including a stored policy.
        if Config::enforcement_disabled() {
            return Answer::ignore();
        }

        let Ok(store) = self.store.lock() else {
            // A poisoned lock means another thread panicked mid-write. Refusing
            // to answer lets the module apply its own fallback, which is the
            // deliberate policy for "the daemon is not healthy".
            tracing::error!("store lock poisoned; declining to answer");
            return Answer::ignore();
        };

        let policy = match store.load_policy(&query.user) {
            Ok(Some(p)) => p,
            // No policy means this user is not managed on this machine.
            Ok(None) => return Answer::ignore(),
            Err(e) => {
                tracing::error!(user = %query.user, error = %e, "cannot read policy");
                return Answer::ignore();
            }
        };

        // A policy is not permission — the same rule the tick loop applies.
        // Without this the two halves disagree in the worst possible
        // direction: a stray policy would leave an adult's session running
        // while refusing their next login.
        if self.membership.is_member(&query.user, &self.managed_group) != Some(true) {
            tracing::warn!(
                user = %query.user,
                group = %self.managed_group,
                "a policy exists but the user is not in the managed group; permitting"
            );
            return Answer::ignore();
        }

        let (snapshot, _day) = match store.snapshot(&policy, now) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(user = %query.user, error = %e, "cannot read usage");
                return Answer::ignore();
            }
        };

        match tb_core::evaluate(&policy, &snapshot, now) {
            Verdict::Allowed(_) => Answer::allow(),
            Verdict::Denied(denial) => {
                let _ = store.record_event(
                    &query.user,
                    EventKind::AccessDenied,
                    Some(&format!("{:?} via {}", denial.reason, query.service)),
                );
                let (reason, retry) = deny_facts(&denial, now);
                // The English text goes along as a fallback for a module older
                // than this daemon; a current one writes its own.
                let mut answer = Answer::deny(deny_text(reason, &retry, Locale::English))
                    .with_reason(reason, retry);
                if let Some(secs) = seconds_until(&denial, now) {
                    answer = answer.with_retry_in(secs);
                }
                answer
            }
        }
    }
}

/// Seconds from `now` until access returns, if that is known.
fn seconds_until(denial: &Denial, now: &Zoned) -> Option<u64> {
    let retry = denial.retry_at.as_ref()?;
    u64::try_from(now.duration_until(retry).as_secs()).ok()
}

/// Why access was refused and when it returns, as facts rather than prose.
///
/// The daemon deliberately does not write the sentence. It runs as a systemd
/// service, usually with no `LANG` and never the child's, so anything composed
/// here arrives in the wrong language on a German desktop. What it does know,
/// and nobody else does, is the policy's time zone — so it sends the clock
/// reading and lets each front end write the sentence around it.
#[must_use]
pub fn deny_facts(denial: &Denial, now: &Zoned) -> (Reason, RetryAt) {
    let reason = match denial.reason {
        DenyReason::DailyQuotaExhausted => Reason::DailyQuota,
        DenyReason::WeeklyQuotaExhausted => Reason::WeeklyQuota,
        DenyReason::OutsideAllowedWindow => Reason::OutsideWindow,
    };

    let retry = denial
        .retry_at
        .as_ref()
        .map_or_else(RetryAt::default, |at| {
            let days_ahead = at.date().since(now.date()).map_or(0, |s| s.get_days());
            RetryAt {
                clock: Some(format!("{:02}:{:02}", at.hour(), at.minute())),
                not_today: days_ahead != 0,
                // Beyond tomorrow a bare time is not enough to plan around, so the
                // weekday goes along too. English here; the reader translates it.
                weekday: (days_ahead > 1).then(|| at.strftime("%A").to_string()),
            }
        });

    (reason, retry)
}

/// Binds the socket, replacing a stale one left by an unclean shutdown.
///
/// Must be called from inside a Tokio runtime: the listener registers with
/// the reactor on creation.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket file left behind by a killed daemon would make bind() fail.
    // Removing it is safe: if another daemon were live, its lock on the state
    // database would have stopped us long before this point.
    match std::fs::remove_file(path) {
        Ok(()) => tracing::warn!(?path, "removed a stale socket"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(?path, "listening for PAM queries");
    Ok(listener)
}

/// Serves queries until cancelled.
pub async fn serve(listener: UnixListener, responder: Responder) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let responder = responder.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, &responder).await {
                        tracing::debug!(error = %e, "PAM connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "accept failed");
                // Back off rather than spinning on a persistent error such as
                // running out of file descriptors.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(stream: UnixStream, responder: &Responder) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).take(tb_proto::pam::MAX_MESSAGE_BYTES as u64);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let answer = match serde_json::from_str::<Query>(line.trim()) {
        Ok(query) if query.v == VERSION => {
            tracing::debug!(user = %query.user, service = %query.service, phase = ?query.phase, "PAM query");
            responder.answer(&query, &Zoned::now())
        }
        Ok(query) => {
            // A module from a different package version. Staying out of the way
            // beats guessing at a protocol we do not know.
            tracing::warn!(
                their = query.v,
                ours = VERSION,
                "unsupported protocol version"
            );
            Answer::ignore()
        }
        Err(e) => {
            tracing::warn!(error = %e, "unparseable query");
            Answer::ignore()
        }
    };

    let mut out = serde_json::to_string(&answer)?;
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// A fixed answer to "is this user managed?", so the tests never consult
    /// the machine's own passwd database.
    #[derive(Debug)]
    struct FakeGroups(Option<bool>);
    impl Membership for FakeGroups {
        fn is_member(&self, _user: &str, _group: &str) -> Option<bool> {
            self.0
        }
    }

    use super::*;
    use jiff::civil;
    use tb_core::duration::DurationSpec;
    use tb_core::policy::{Policy, Quota};
    use tb_core::schedule::{TimeWindow, WeekSchedule};
    use tb_core::usage::UsageSegment;
    use tb_proto::pam::{Decision, Phase};
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

    fn policy_2h() -> Policy {
        let mut p = Policy::permissive("kid");
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    fn responder_with(policy: &Policy, used_mins: i64, from: &Zoned) -> Responder {
        let store = Store::in_memory().unwrap();
        store.save_policy(policy).unwrap();
        if used_mins > 0 {
            let start = from.timestamp();
            store
                .insert_segment(&UsageSegment {
                    id: Uuid::now_v7(),
                    subject: policy.subject.clone(),
                    app: tb_core::AppId::new("firefox"),
                    source: tb_core::AppIdSource::DesktopFile,
                    start,
                    end: start
                        .checked_add(jiff::SignedDuration::from_secs(used_mins * 60))
                        .unwrap(),
                    title: None,
                })
                .unwrap();
        }
        Responder::new(
            Arc::new(Mutex::new(store)),
            Arc::new(FakeGroups(Some(true))),
            "kids",
        )
    }

    /// The same exhausted state, with a chosen answer to "is this user in the
    /// managed group?".
    fn responder_with_group(answer: Option<bool>, from: &Zoned) -> Responder {
        let store = Store::in_memory().unwrap();
        store.save_policy(&policy_2h()).unwrap();
        let start = from.timestamp();
        store
            .insert_segment(&UsageSegment {
                id: Uuid::now_v7(),
                subject: "kid".to_owned(),
                app: tb_core::AppId::new("firefox"),
                source: tb_core::AppIdSource::DesktopFile,
                start,
                end: start
                    .checked_add(jiff::SignedDuration::from_secs(120 * 60))
                    .unwrap(),
                title: None,
            })
            .unwrap();
        Responder::new(
            Arc::new(Mutex::new(store)),
            Arc::new(FakeGroups(answer)),
            "kids",
        )
    }

    #[test]
    fn a_user_outside_the_managed_group_is_never_refused() {
        // The two halves of enforcement have to agree. Refusing here while the
        // tick loop declines to lock is the worst of both: the session keeps
        // running and the next login fails. Measured against the real module
        // in a container before it was fixed.
        let start = at(2026, 8, 19, 15, 0);
        let r = responder_with_group(Some(false), &start);
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 18, 0),
        );
        assert_eq!(a.decision, Decision::Ignore);
    }

    #[test]
    fn a_failed_group_lookup_permits() {
        let start = at(2026, 8, 19, 15, 0);
        let r = responder_with_group(None, &start);
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 18, 0),
        );
        assert_eq!(a.decision, Decision::Ignore);
    }

    #[test]
    fn a_managed_user_is_still_refused() {
        let start = at(2026, 8, 19, 15, 0);
        let r = responder_with_group(Some(true), &start);
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 18, 0),
        );
        assert_eq!(a.decision, Decision::Deny, "enforcement must still work");
    }

    #[test]
    fn a_user_without_a_policy_is_not_our_business() {
        let r = Responder::new(
            Arc::new(Mutex::new(Store::in_memory().unwrap())),
            Arc::new(FakeGroups(Some(true))),
            "kids",
        );
        let a = r.answer(
            &Query::new("guest", "sddm", Phase::Account),
            &at(2026, 8, 19, 12, 0),
        );
        assert_eq!(a.decision, Decision::Ignore);
    }

    #[test]
    fn a_child_with_time_left_is_allowed() {
        let start = at(2026, 8, 19, 15, 0);
        let r = responder_with(&policy_2h(), 30, &start);
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 16, 0),
        );
        assert_eq!(a.decision, Decision::Allow);
    }

    #[test]
    fn an_exhausted_quota_is_refused_with_a_time_to_come_back() {
        let start = at(2026, 8, 19, 15, 0);
        let r = responder_with(&policy_2h(), 120, &start);
        let now = at(2026, 8, 19, 18, 0);
        let a = r.answer(&Query::new("kid", "kde", Phase::Auth), &now);

        assert_eq!(a.decision, Decision::Deny);
        let msg = a.message.expect("a message");
        assert!(msg.contains("used up"), "got {msg:?}");
        assert!(msg.contains("04:00"), "must say when: {msg:?}");
        // 18:00 to 04:00 the next day is ten hours.
        assert_eq!(a.retry_in_secs, Some(10 * 3600));
    }

    #[test]
    fn a_refusal_is_recorded_as_an_event() {
        let start = at(2026, 8, 19, 15, 0);
        let store = Store::in_memory().unwrap();
        store.save_policy(&policy_2h()).unwrap();
        let s = start.timestamp();
        store
            .insert_segment(&UsageSegment {
                id: Uuid::now_v7(),
                subject: "kid".to_owned(),
                app: tb_core::AppId::new("firefox"),
                source: tb_core::AppIdSource::DesktopFile,
                start: s,
                end: s
                    .checked_add(jiff::SignedDuration::from_secs(2 * 3600))
                    .unwrap(),
                title: None,
            })
            .unwrap();
        let shared = Arc::new(Mutex::new(store));
        let r = Responder::new(shared.clone(), Arc::new(FakeGroups(Some(true))), "kids");

        let _ = r.answer(
            &Query::new("kid", "kde", Phase::Auth),
            &at(2026, 8, 19, 18, 0),
        );
        let count = shared
            .lock()
            .unwrap()
            .event_count("kid", EventKind::AccessDenied)
            .unwrap();
        assert_eq!(count, 1, "parents need to see refusals in the log");
    }

    #[test]
    fn a_bedtime_refusal_names_the_weekday_when_it_is_not_today() {
        let mut p = policy_2h();
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(15, 0, 0, 0),
            civil::time(19, 0, 0, 0),
        )]);
        let r = responder_with(&p, 0, &at(2026, 8, 19, 15, 0));
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 20, 0),
        );
        assert_eq!(a.decision, Decision::Deny);
        let msg = a.message.expect("a message");
        // Tomorrow is said as "tomorrow", not by name — a weekday is only
        // informative once it is further out than that.
        assert!(msg.contains("tomorrow at 15:00"), "got {msg:?}");
        assert_eq!(a.retry.clock.as_deref(), Some("15:00"));
        assert!(a.retry.not_today);
        assert_eq!(a.retry.weekday, None, "tomorrow needs no name");
    }

    #[test]
    fn a_refusal_further_out_than_tomorrow_names_the_day() {
        // A bare clock reading is not enough to plan around once it is days
        // away, so the weekday goes along — in English, for the reader to
        // translate.
        let mut p = policy_2h();
        // Days left open on purpose: with a daily limit as well, that one is
        // checked first and the refusal would point at tomorrow instead.
        p.daily_quota = tb_core::schedule::WeekSchedule::uniform(Quota::Unlimited);
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(1));
        let start = at(2026, 8, 19, 15, 0);
        let store = Store::in_memory().unwrap();
        store.save_policy(&p).unwrap();
        let s = start.timestamp();
        store
            .insert_segment(&UsageSegment {
                id: Uuid::now_v7(),
                subject: "kid".to_owned(),
                app: tb_core::AppId::unknown(),
                source: tb_core::AppIdSource::Unknown,
                start: s,
                end: s.checked_add(jiff::SignedDuration::from_hours(2)).unwrap(),
                title: None,
            })
            .unwrap();
        let r = Responder::new(
            Arc::new(Mutex::new(store)),
            Arc::new(FakeGroups(Some(true))),
            "kids",
        );

        // Wednesday the 19th; the week reopens on Monday the 24th.
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 18, 0),
        );
        assert_eq!(a.decision, Decision::Deny);
        assert_eq!(a.retry.weekday.as_deref(), Some("Monday"));
        assert!(a.retry.not_today);
        let msg = a.message.expect("a message");
        assert!(msg.contains("Monday 04:00"), "got {msg:?}");
    }

    #[test]
    fn observe_only_policies_never_refuse() {
        let p = Policy::permissive("kid"); // enforcement = false
        let r = responder_with(&p, 10_000, &at(2026, 8, 19, 5, 0));
        let a = r.answer(
            &Query::new("kid", "sddm", Phase::Account),
            &at(2026, 8, 19, 20, 0),
        );
        assert_eq!(a.decision, Decision::Allow);
    }

    #[tokio::test]
    async fn the_socket_speaks_the_protocol_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pam.sock");
        let listener = bind(&path).unwrap();

        let start = at(2026, 8, 19, 15, 0);
        let responder = responder_with(&policy_2h(), 120, &start);
        tokio::spawn(serve(listener, responder));

        // Speak the protocol as the real module does: one line in, one line out.
        let stream = UnixStream::connect(&path).await.unwrap();
        let (r, mut w) = stream.into_split();
        let query = serde_json::to_string(&Query::new("kid", "kde", Phase::Auth)).unwrap();
        w.write_all(format!("{query}\n").as_bytes()).await.unwrap();
        w.flush().await.unwrap();

        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.unwrap();
        let answer: Answer = serde_json::from_str(line.trim()).unwrap();
        // The stored usage is from 2026; against the real clock the quota is
        // untouched, so the decision is Allow. What this proves is the framing.
        assert_eq!(answer.v, VERSION);
        assert!(matches!(answer.decision, Decision::Allow | Decision::Deny));
    }

    #[tokio::test]
    async fn garbage_on_the_socket_gets_a_polite_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pam.sock");
        let listener = bind(&path).unwrap();
        tokio::spawn(serve(
            listener,
            Responder::new(
                Arc::new(Mutex::new(Store::in_memory().unwrap())),
                Arc::new(FakeGroups(Some(true))),
                "kids",
            ),
        ));

        let stream = UnixStream::connect(&path).await.unwrap();
        let (r, mut w) = stream.into_split();
        w.write_all(b"not json at all\n").await.unwrap();
        w.flush().await.unwrap();

        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.unwrap();
        let answer: Answer = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(answer.decision, Decision::Ignore);
    }

    #[tokio::test]
    async fn a_stale_socket_file_does_not_block_startup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pam.sock");
        std::fs::write(&path, b"leftover").unwrap();
        assert!(bind(&path).is_ok(), "must replace the stale file");
    }
}
