// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The decision engine: policy + recorded usage + current time → verdict.
//!
//! Deliberately a pure function with no clock, filesystem or network access.
//! The same function answers three questions that would otherwise drift apart:
//!
//! * the daemon, every tick: "do I have to lock now?"
//! * the PAM module, at login: "may this user log in?"
//! * the plasmoid and web UI: "how much time is left?"

use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{Span, Zoned};
use serde::{Deserialize, Serialize};

use crate::duration::DurationSpec;
use crate::policy::{Policy, Quota};
use crate::schedule::{Day, PolicyDay, TimeWindow, policy_day, policy_day_end};

/// The usage figures a policy is evaluated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Time credited during the running policy day.
    pub used_today: DurationSpec,
    /// Time credited during the running policy week (Monday–Sunday).
    pub used_this_week: DurationSpec,
    /// Bonus time granted for today. Counts against the daily quota only.
    pub bonus_today: DurationSpec,
}

/// Which rule is currently capping the remaining time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitedBy {
    /// Nothing caps it — observe-only mode, or every quota is unlimited.
    Nothing,
    DailyQuota,
    WeeklyQuota,
    /// The current time window closes before the quota runs out.
    Window,
}

/// Why a login or session is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    DailyQuotaExhausted,
    WeeklyQuotaExhausted,
    OutsideAllowedWindow,
}

impl DenyReason {
    /// Short key for translations. The visible text is localized by each front
    /// end; the PAM module carries its own terse wording.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::DailyQuotaExhausted => "deny.daily_quota",
            Self::WeeklyQuotaExhausted => "deny.weekly_quota",
            Self::OutsideAllowedWindow => "deny.outside_window",
        }
    }
}

/// The result of an evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed(Allowance),
    Denied(Denial),
}

/// Use is permitted, with this much time left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowance {
    /// Smallest remaining time across all applicable rules. `None` = unlimited.
    pub remaining: Option<DurationSpec>,
    pub limited_by: LimitedBy,
    /// When the remaining time runs out. `None` = unlimited.
    pub expires_at: Option<Zoned>,
    /// The next warning threshold still ahead.
    pub next_warning: Option<DurationSpec>,
}

/// Use is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub reason: DenyReason,
    /// When it becomes allowed again. `None` if that cannot be determined.
    pub retry_at: Option<Zoned>,
}

impl Verdict {
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed(_))
    }

    #[must_use]
    pub const fn denial(&self) -> Option<&Denial> {
        match self {
            Self::Denied(d) => Some(d),
            Self::Allowed(_) => None,
        }
    }

    /// Remaining time, or `ZERO` when blocked.
    #[must_use]
    pub fn remaining(&self) -> Option<DurationSpec> {
        match self {
            Self::Allowed(a) => a.remaining,
            Self::Denied(_) => Some(DurationSpec::ZERO),
        }
    }
}

/// A time window pinned to actual instants on the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Interval {
    start: Zoned,
    end: Zoned,
}

/// Turns the wall-clock windows of one policy day into real instants.
///
/// A window whose start time falls before `day_start` belongs to the *second*
/// half of the policy day and therefore to the next calendar date. That single
/// rule makes windows across midnight (22:00–01:00) work without a special case.
fn concrete_windows(
    pd: PolicyDay,
    windows: &[TimeWindow],
    day_start: civil::Time,
    tz: &TimeZone,
) -> Vec<Interval> {
    let resolve = |t: civil::Time| -> Option<Zoned> {
        let date = if t >= day_start {
            pd.date
        } else {
            pd.date.tomorrow().ok()?
        };
        date.to_datetime(t).to_zoned(tz.clone()).ok()
    };

    let mut out: Vec<Interval> = windows
        .iter()
        .filter_map(|w| {
            let start = resolve(w.start)?;
            let mut end = resolve(w.end)?;
            // Window across midnight: the end lands one day later.
            if end <= start {
                end = end.checked_add(Span::new().days(1)).ok()?;
            }
            Some(Interval { start, end })
        })
        .collect();

    out.sort_by(|a, b| a.start.cmp(&b.start));

    // Merge touching or overlapping windows. Without this, 15:00–17:00 plus
    // 17:00–19:00 would report "window closed" at 17:00 and lock the session.
    let mut merged: Vec<Interval> = Vec::with_capacity(out.len());
    for iv in out {
        match merged.last_mut() {
            Some(last) if iv.start <= last.end => {
                if iv.end > last.end {
                    last.end = iv.end;
                }
            }
            _ => merged.push(iv),
        }
    }
    merged
}

/// Collects windows across several policy days so "when again?" can be answered.
fn upcoming_windows(policy: &Policy, from: PolicyDay, tz: &TimeZone, days: i32) -> Vec<Interval> {
    let mut all = Vec::new();
    for offset in 0..days {
        let Ok(date) = from.date.checked_add(Span::new().days(offset)) else {
            break;
        };
        let pd = PolicyDay {
            date,
            day: Day::from(date.weekday()),
        };
        let windows = policy.allowed_windows.get(pd.day);
        if windows.is_empty() {
            continue;
        }
        all.extend(concrete_windows(pd, windows, policy.day_start, tz));
    }
    all
}

/// Start of the policy week (Monday) for a policy day.
fn week_start(pd: PolicyDay) -> civil::Date {
    let back = i32::from(pd.date.weekday().to_monday_zero_offset());
    pd.date
        .checked_sub(Span::new().days(back))
        .unwrap_or(pd.date)
}

/// Evaluates a policy against usage at a point in time.
///
/// If the time zone cannot be resolved we fall back to UTC rather than failing:
/// an unreadable zone must not lock a child out, and the policy was validated
/// when it was loaded anyway.
#[must_use]
pub fn evaluate(policy: &Policy, usage: &UsageSnapshot, now: &Zoned) -> Verdict {
    if !policy.enforcement {
        return Verdict::Allowed(Allowance {
            remaining: None,
            limited_by: LimitedBy::Nothing,
            expires_at: None,
            next_warning: None,
        });
    }

    let tz = TimeZone::get(&policy.timezone).unwrap_or(TimeZone::UTC);
    let now = now.with_time_zone(tz.clone());
    let pd = policy_day(&now, policy.day_start);

    // --- 1. Time windows (bedtime) -------------------------------------------
    let windows = policy.allowed_windows.get(pd.day);
    let mut window_end: Option<Zoned> = None;
    if !windows.is_empty() {
        let intervals = concrete_windows(pd, windows, policy.day_start, &tz);
        if let Some(current) = intervals.iter().find(|iv| iv.start <= now && now < iv.end) {
            window_end = Some(current.end.clone());
        } else {
            let retry_at = upcoming_windows(policy, pd, &tz, 9)
                .into_iter()
                .map(|iv| iv.start)
                .find(|s| *s > now);
            return Verdict::Denied(Denial {
                reason: DenyReason::OutsideAllowedWindow,
                retry_at,
            });
        }
    }

    // --- 2. Daily quota (bonus included) -------------------------------------
    let daily_limit = policy
        .daily_quota
        .get(pd.day)
        .limit()
        .map(|l| l.saturating_add(usage.bonus_today));
    let daily_remaining = daily_limit.map(|l| l.saturating_sub(usage.used_today));
    if daily_remaining == Some(DurationSpec::ZERO) {
        return Verdict::Denied(Denial {
            reason: DenyReason::DailyQuotaExhausted,
            retry_at: Some(policy_day_end(pd, policy.day_start, &tz)),
        });
    }

    // --- 3. Weekly quota -----------------------------------------------------
    let weekly_remaining = match policy.weekly_quota {
        Quota::Unlimited => None,
        Quota::Limited(l) => Some(l.saturating_sub(usage.used_this_week)),
    };
    if weekly_remaining == Some(DurationSpec::ZERO) {
        // Available again on Monday, when the new policy week starts.
        let retry_at = week_start(pd)
            .checked_add(Span::new().days(7))
            .ok()
            .and_then(|d| d.to_datetime(policy.day_start).to_zoned(tz.clone()).ok());
        return Verdict::Denied(Denial {
            reason: DenyReason::WeeklyQuotaExhausted,
            retry_at,
        });
    }

    // --- 4. Pick the tightest applicable bound -------------------------------
    // `duration_until` rather than span subtraction: a `Span` is calendar-aware
    // and balances into hours and minutes, so `get_seconds()` would return only
    // the seconds component instead of the total.
    let window_remaining = window_end.as_ref().and_then(|end| {
        let secs = now.duration_until(end).as_secs();
        u64::try_from(secs).ok().map(DurationSpec::from_secs)
    });

    let mut remaining: Option<DurationSpec> = None;
    let mut limited_by = LimitedBy::Nothing;
    for (candidate, kind) in [
        (daily_remaining, LimitedBy::DailyQuota),
        (weekly_remaining, LimitedBy::WeeklyQuota),
        (window_remaining, LimitedBy::Window),
    ] {
        if let Some(c) = candidate
            && remaining.is_none_or(|r| c < r)
        {
            remaining = Some(c);
            limited_by = kind;
        }
    }

    let expires_at = remaining.and_then(|r| {
        now.checked_add(Span::new().seconds(i64::try_from(r.as_secs()).ok()?))
            .ok()
    });
    let next_warning = remaining.and_then(|r| {
        policy
            .sorted_warnings()
            .into_iter()
            .find(|&w| w < r)
            .or(Some(DurationSpec::ZERO))
            .filter(|_| !r.is_zero())
    });

    Verdict::Allowed(Allowance {
        remaining,
        limited_by,
        expires_at,
        next_warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::WeekSchedule;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .unwrap()
    }

    /// Enforcing policy, 2 h per day, no other restriction.
    fn policy_2h() -> Policy {
        let mut p = Policy::permissive("kid");
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    fn used(mins: u64) -> UsageSnapshot {
        UsageSnapshot {
            used_today: DurationSpec::from_mins(mins),
            ..UsageSnapshot::default()
        }
    }

    #[test]
    fn observe_only_mode_always_allows() {
        let p = Policy::permissive("kid"); // enforcement = false
        let v = evaluate(&p, &used(10_000), &at(2026, 8, 19, 3, 0));
        assert!(v.is_allowed());
        assert_eq!(v.remaining(), None, "unlimited");
    }

    #[test]
    fn remaining_time_comes_from_the_daily_quota() {
        let v = evaluate(&policy_2h(), &used(90), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else {
            panic!("expected allowed")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        assert_eq!(a.limited_by, LimitedBy::DailyQuota);
    }

    #[test]
    fn exhausted_daily_quota_blocks_until_the_next_policy_day() {
        let v = evaluate(&policy_2h(), &used(120), &at(2026, 8, 19, 16, 0));
        let d = v.denial().expect("expected denied");
        assert_eq!(d.reason, DenyReason::DailyQuotaExhausted);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 20));
        assert_eq!(retry.hour(), 4);
    }

    #[test]
    fn bonus_lifts_the_block_immediately() {
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_mins(120),
            bonus_today: DurationSpec::from_mins(30),
            ..UsageSnapshot::default()
        };
        let v = evaluate(&policy_2h(), &usage, &at(2026, 8, 19, 16, 0));
        assert_eq!(v.remaining(), Some(DurationSpec::from_mins(30)));
    }

    #[test]
    fn outside_the_time_window_is_blocked() {
        let mut p = policy_2h();
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(15, 0, 0, 0),
            civil::time(19, 0, 0, 0),
        )]);
        // 20:00 is past the window → blocked until tomorrow 15:00.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 20, 0));
        let d = v.denial().expect("expected denied");
        assert_eq!(d.reason, DenyReason::OutsideAllowedWindow);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 20));
        assert_eq!(retry.hour(), 15);
    }

    #[test]
    fn the_window_close_caps_remaining_time() {
        let mut p = policy_2h();
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(15, 0, 0, 0),
            civil::time(19, 0, 0, 0),
        )]);
        // 18:30 with a full 2 h quota — but the window closes in 30 minutes.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 18, 30));
        let Verdict::Allowed(a) = v else {
            panic!("expected allowed")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        assert_eq!(a.limited_by, LimitedBy::Window);
    }

    #[test]
    fn touching_windows_are_merged() {
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        p.allowed_windows = WeekSchedule::uniform(vec![
            TimeWindow::new(civil::time(15, 0, 0, 0), civil::time(17, 0, 0, 0)),
            TimeWindow::new(civil::time(17, 0, 0, 0), civil::time(19, 0, 0, 0)),
        ]);
        // The seam between the two windows must not lock the session.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 17, 0));
        let Verdict::Allowed(a) = v else {
            panic!("the seam between windows must not block")
        };
        assert_eq!(
            a.remaining,
            Some(DurationSpec::from_hours(2)),
            "until 19:00"
        );
    }

    #[test]
    fn a_window_across_midnight_stays_in_the_same_policy_day() {
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(22, 0, 0, 0),
            civil::time(1, 0, 0, 0),
        )]);
        // 00:30 on the 20th belongs to policy day the 19th, inside the window.
        let v = evaluate(&p, &used(0), &at(2026, 8, 20, 0, 30));
        let Verdict::Allowed(a) = v else {
            panic!("expected allowed")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        // 01:30 is past it.
        assert!(!evaluate(&p, &used(0), &at(2026, 8, 20, 1, 30)).is_allowed());
    }

    #[test]
    fn the_weekly_quota_can_bind_before_the_daily_one() {
        let mut p = policy_2h();
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_mins(30),
            used_this_week: DurationSpec::from_mins(9 * 60 + 45),
            ..UsageSnapshot::default()
        };
        let v = evaluate(&p, &usage, &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else {
            panic!("expected allowed")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(15)));
        assert_eq!(a.limited_by, LimitedBy::WeeklyQuota);
    }

    #[test]
    fn exhausted_weekly_quota_blocks_until_monday() {
        let mut p = policy_2h();
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        let usage = UsageSnapshot {
            used_this_week: DurationSpec::from_hours(10),
            ..UsageSnapshot::default()
        };
        // 2026-08-19 is a Wednesday, so the next Monday is the 24th.
        let v = evaluate(&p, &usage, &at(2026, 8, 19, 16, 0));
        let d = v.denial().expect("expected denied");
        assert_eq!(d.reason, DenyReason::WeeklyQuotaExhausted);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 24));
        assert_eq!(retry.weekday(), civil::Weekday::Monday);
        assert_eq!(retry.hour(), 4);
    }

    #[test]
    fn the_next_warning_is_below_the_remaining_time() {
        // 2 h quota, 1 h 50 min used → 10 min left, next warning at 5 min.
        let v = evaluate(&policy_2h(), &used(110), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else { panic!() };
        assert_eq!(a.next_warning, Some(DurationSpec::from_mins(5)));
        // 25 min left → next warning at 15 min.
        let v = evaluate(&policy_2h(), &used(95), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else { panic!() };
        assert_eq!(a.next_warning, Some(DurationSpec::from_mins(15)));
    }

    #[test]
    fn usage_beyond_the_quota_blocks_instead_of_wrapping() {
        // If the daemon was offline and booked more time than allowed, the
        // saturating subtraction must not hand out time again.
        let v = evaluate(&policy_2h(), &used(500), &at(2026, 8, 19, 16, 0));
        assert_eq!(
            v.denial().map(|d| d.reason),
            Some(DenyReason::DailyQuotaExhausted)
        );
    }

    #[test]
    fn a_weekly_budget_can_replace_daily_limits_entirely() {
        // Unlimited days plus a weekly ceiling means the child allocates the
        // week themselves — three hours on Saturday and nothing on Sunday is
        // their call. No new mechanism is needed for this; it falls out of the
        // two quotas being independent.
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        assert!(p.validate().is_ok());

        // Six hours in one sitting is allowed while the week has room.
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_hours(6),
            used_this_week: DurationSpec::from_hours(6),
            ..UsageSnapshot::default()
        };
        let v = evaluate(&p, &usage, &at(2026, 8, 22, 16, 0));
        assert_eq!(v.remaining(), Some(DurationSpec::from_hours(4)));
        assert_eq!(
            match &v {
                Verdict::Allowed(a) => a.limited_by,
                Verdict::Denied(_) => panic!("expected allowed"),
            },
            LimitedBy::WeeklyQuota
        );

        // And spending it all stops them for the rest of the week, not the day.
        let spent = UsageSnapshot {
            used_this_week: DurationSpec::from_hours(10),
            ..UsageSnapshot::default()
        };
        let d = evaluate(&p, &spent, &at(2026, 8, 22, 16, 0));
        assert_eq!(
            d.denial().map(|d| d.reason),
            Some(DenyReason::WeeklyQuotaExhausted)
        );
    }

    #[test]
    fn a_weekly_budget_can_still_carry_a_daily_ceiling() {
        // The middle setting: spend the week freely, but not more than three
        // hours in any one day.
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(3)));
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        assert!(p.validate().is_ok());

        let usage = UsageSnapshot {
            used_today: DurationSpec::from_hours(3),
            used_this_week: DurationSpec::from_hours(4),
            ..UsageSnapshot::default()
        };
        let d = evaluate(&p, &usage, &at(2026, 8, 22, 16, 0));
        assert_eq!(
            d.denial().map(|d| d.reason),
            Some(DenyReason::DailyQuotaExhausted),
            "the daily ceiling binds first, and says so"
        );
    }

    #[test]
    fn a_day_with_zero_quota_blocks_all_day() {
        let mut p = policy_2h();
        p.daily_quota
            .set(Day::Monday, Quota::Limited(DurationSpec::ZERO));
        // 2026-08-24 is a Monday.
        assert!(!evaluate(&p, &used(0), &at(2026, 8, 24, 10, 0)).is_allowed());
        // Tuesday is normal again.
        assert!(evaluate(&p, &used(0), &at(2026, 8, 25, 10, 0)).is_allowed());
    }
}
