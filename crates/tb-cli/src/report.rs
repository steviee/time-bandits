// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Formatting what the daemon knows into something a parent can read.
//!
//! Pure functions over data, so the wording is testable. That matters more than
//! it sounds: "2h left" and "2h used" differ by one word and lead to opposite
//! decisions.

use std::fmt::Write as _;

use jiff::Zoned;
use tb_core::appid::AppId;
use tb_core::duration::DurationSpec;
use tb_core::engine::{DenyReason, LimitedBy, UsageSnapshot, Verdict};
use tb_core::policy::{Policy, Quota};
use tb_core::schedule::{Day, PolicyDay};

/// Renders a duration the way people say it: "1 h 30 min", "45 min", "30 s".
#[must_use]
pub fn human(d: DurationSpec) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return "none".to_owned();
    }
    if secs < 60 {
        return format!("{secs} s");
    }
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A clock time, plus the weekday when it is not today.
#[must_use]
pub fn when(target: &Zoned, now: &Zoned) -> String {
    let time = format!("{:02}:{:02}", target.hour(), target.minute());
    if target.date() == now.date() {
        time
    } else if target.date() == now.date().tomorrow().unwrap_or(target.date()) {
        format!("tomorrow {time}")
    } else {
        format!("{} {time}", target.strftime("%A"))
    }
}

/// The one-screen answer to "how is my child doing right now".
#[must_use]
pub fn status(
    policy: &Policy,
    usage: &UsageSnapshot,
    verdict: &Verdict,
    day: PolicyDay,
    now: &Zoned,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", policy.subject);
    let _ = writeln!(
        out,
        "  policy day   {} ({}), timezone {}",
        day.date, day.day, policy.timezone
    );

    if !policy.enforcement {
        let _ = writeln!(
            out,
            "  enforcement  OFF — observing only, nothing is limited"
        );
    }

    let _ = writeln!(out, "  used today   {}", human(usage.used_today));
    if !usage.bonus_today.is_zero() {
        let _ = writeln!(out, "  bonus today  {}", human(usage.bonus_today));
    }
    if policy.weekly_quota != Quota::Unlimited {
        let _ = writeln!(out, "  used, week   {}", human(usage.used_this_week));
    }

    match verdict {
        Verdict::Allowed(a) => match a.remaining {
            Some(remaining) => {
                let reason = match a.limited_by {
                    LimitedBy::DailyQuota => "daily quota",
                    LimitedBy::WeeklyQuota => "weekly quota",
                    LimitedBy::Window => "time window closing",
                    LimitedBy::Nothing => "nothing",
                };
                let _ = writeln!(out, "  remaining    {} ({reason})", human(remaining));
                if let Some(at) = &a.expires_at {
                    let _ = writeln!(out, "  runs out     {}", when(at, now));
                }
            }
            None => {
                let _ = writeln!(out, "  remaining    unlimited");
            }
        },
        Verdict::Denied(d) => {
            let reason = match d.reason {
                DenyReason::DailyQuotaExhausted => "daily quota used up",
                DenyReason::WeeklyQuotaExhausted => "weekly quota used up",
                DenyReason::OutsideAllowedWindow => "outside the allowed hours",
            };
            let _ = writeln!(out, "  BLOCKED      {reason}");
            match &d.retry_at {
                Some(at) => {
                    let _ = writeln!(out, "  allowed at   {}", when(at, now));
                }
                None => {
                    let _ = writeln!(out, "  allowed at   unknown");
                }
            }
        }
    }
    out
}

/// The rules themselves, as configured.
#[must_use]
pub fn policy(p: &Policy) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}  (version {})", p.subject, p.version);
    let _ = writeln!(
        out,
        "  enforcement  {}",
        if p.enforcement {
            "on"
        } else {
            "off (observing only)"
        }
    );
    let _ = writeln!(out, "  timezone     {}", p.timezone);
    let _ = writeln!(
        out,
        "  policy day   starts {:02}:{:02}",
        p.day_start.hour(),
        p.day_start.minute()
    );

    let _ = writeln!(out, "  daily quota");
    for day in Day::ALL {
        let quota = match p.daily_quota.get(day) {
            Quota::Unlimited => "unlimited".to_owned(),
            Quota::Limited(d) => human(*d),
        };
        let windows = p.allowed_windows.get(day);
        let hours = if windows.is_empty() {
            "any time".to_owned()
        } else {
            windows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(out, "    {day:<10} {quota:<12} {hours}");
    }

    if let Quota::Limited(w) = p.weekly_quota {
        let _ = writeln!(out, "  weekly quota {}", human(w));
    }
    let _ = writeln!(
        out,
        "  warnings     {}",
        p.sorted_warnings()
            .iter()
            .map(|w| human(*w))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(out, "  when used up {:?}", p.on_exhausted);
    let _ = writeln!(out, "  grace period {}", human(p.grace_period));
    let _ = writeln!(out, "  idle after   {}", human(p.idle_threshold));
    let _ = writeln!(
        out,
        "  window titles {}",
        if p.record_window_titles {
            "recorded"
        } else {
            "not recorded"
        }
    );
    out
}

/// Time per application, longest first.
#[must_use]
pub fn usage_table(totals: &[(AppId, DurationSpec)], total: DurationSpec) -> String {
    if totals.is_empty() {
        return "no recorded usage in this period\n".to_owned();
    }
    let width = totals
        .iter()
        .map(|(a, _)| a.as_str().len())
        .max()
        .unwrap_or(10)
        .clamp(10, 40);

    let mut out = String::new();
    for (app, duration) in totals {
        let share = if total.as_secs() == 0 {
            0
        } else {
            duration.as_secs() * 100 / total.as_secs()
        };
        // A bar makes the shape obvious at a glance; the numbers stay for detail.
        let bar = "#".repeat(usize::try_from(share / 4).unwrap_or(0));
        let _ = writeln!(
            out,
            "  {:<width$}  {:>10}  {:>3}%  {}",
            app.as_str(),
            human(*duration),
            share,
            bar,
            width = width
        );
    }
    let _ = writeln!(
        out,
        "  {:<width$}  {:>10}",
        "TOTAL",
        human(total),
        width = width
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;
    use tb_core::schedule::WeekSchedule;

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

    #[test]
    fn durations_read_like_speech() {
        assert_eq!(human(DurationSpec::ZERO), "none");
        assert_eq!(human(DurationSpec::from_secs(30)), "30 s");
        assert_eq!(human(DurationSpec::from_mins(45)), "45 min");
        assert_eq!(human(DurationSpec::from_hours(2)), "2 h");
        assert_eq!(human(DurationSpec::from_mins(90)), "1 h 30 min");
    }

    #[test]
    fn times_say_which_day_when_it_is_not_today() {
        let now = at(2026, 8, 19, 16, 0);
        assert_eq!(when(&at(2026, 8, 19, 19, 0), &now), "19:00");
        assert_eq!(when(&at(2026, 8, 20, 4, 0), &now), "tomorrow 04:00");
        assert_eq!(when(&at(2026, 8, 24, 4, 0), &now), "Monday 04:00");
    }

    #[test]
    fn status_says_remaining_not_used() {
        let p = policy_2h();
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_mins(90),
            ..UsageSnapshot::default()
        };
        let now = at(2026, 8, 19, 16, 0);
        let day = tb_core::schedule::policy_day(&now, p.day_start);
        let verdict = tb_core::evaluate(&p, &usage, &now);

        let text = status(&p, &usage, &verdict, day, &now);
        assert!(text.contains("used today   1 h 30 min"), "{text}");
        assert!(text.contains("remaining    30 min (daily quota)"), "{text}");
        assert!(!text.contains("BLOCKED"), "{text}");
    }

    #[test]
    fn status_of_a_blocked_child_says_when_they_are_back() {
        let p = policy_2h();
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_hours(2),
            ..UsageSnapshot::default()
        };
        let now = at(2026, 8, 19, 16, 0);
        let day = tb_core::schedule::policy_day(&now, p.day_start);
        let verdict = tb_core::evaluate(&p, &usage, &now);

        let text = status(&p, &usage, &verdict, day, &now);
        assert!(text.contains("BLOCKED      daily quota used up"), "{text}");
        assert!(text.contains("allowed at   tomorrow 04:00"), "{text}");
    }

    #[test]
    fn observe_only_mode_says_so_prominently() {
        // Otherwise a parent reads a tidy report and believes limits apply.
        let p = Policy::permissive("kid");
        let usage = UsageSnapshot::default();
        let now = at(2026, 8, 19, 16, 0);
        let day = tb_core::schedule::policy_day(&now, p.day_start);
        let verdict = tb_core::evaluate(&p, &usage, &now);

        let text = status(&p, &usage, &verdict, day, &now);
        assert!(text.contains("enforcement  OFF"), "{text}");
        assert!(text.contains("nothing is limited"), "{text}");
    }

    #[test]
    fn bonus_time_is_shown_when_granted() {
        let p = policy_2h();
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_hours(2),
            bonus_today: DurationSpec::from_mins(30),
            ..UsageSnapshot::default()
        };
        let now = at(2026, 8, 19, 16, 0);
        let day = tb_core::schedule::policy_day(&now, p.day_start);
        let verdict = tb_core::evaluate(&p, &usage, &now);

        let text = status(&p, &usage, &verdict, day, &now);
        assert!(text.contains("bonus today  30 min"), "{text}");
        assert!(text.contains("remaining    30 min"), "{text}");
    }

    #[test]
    fn the_policy_listing_shows_every_day() {
        let mut p = policy_2h();
        p.daily_quota
            .set(Day::Saturday, Quota::Limited(DurationSpec::from_hours(3)));
        p.allowed_windows.set(
            Day::Monday,
            vec![tb_core::TimeWindow::new(
                civil::time(15, 0, 0, 0),
                civil::time(19, 0, 0, 0),
            )],
        );
        let text = policy(&p);
        for day in Day::ALL {
            assert!(text.contains(&day.to_string()), "missing {day}: {text}");
        }
        assert!(text.contains("15:00-19:00"), "{text}");
        assert!(text.contains("any time"), "days without windows: {text}");
    }

    #[test]
    fn the_usage_table_totals_and_ranks() {
        let totals = vec![
            (AppId::new("firefox"), DurationSpec::from_mins(90)),
            (AppId::new("org.kde.konsole"), DurationSpec::from_mins(30)),
        ];
        let text = usage_table(&totals, DurationSpec::from_mins(120));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("firefox"), "longest first: {text}");
        assert!(lines[0].contains("75%"), "{text}");
        assert!(lines[1].contains("25%"), "{text}");
        assert!(lines[2].contains("TOTAL"), "{text}");
        assert!(lines[2].contains("2 h"), "{text}");
    }

    #[test]
    fn an_empty_period_says_so_instead_of_printing_a_bare_zero() {
        let text = usage_table(&[], DurationSpec::ZERO);
        assert!(text.contains("no recorded usage"), "{text}");
    }
}
