// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning command-line arguments into policy changes.
//!
//! The parsing here is deliberately strict. `tbctl policy set` is how a parent
//! decides when their child may use the computer, and an argument that is
//! quietly misread produces a rule nobody intended and nobody notices — until
//! the wrong person is locked out at the wrong moment.

use anyhow::{Result, bail};
use jiff::civil;
use tb_core::duration::DurationSpec;
use tb_core::policy::{LockAction, Policy, Quota};
use tb_core::schedule::{Day, TimeWindow};

/// Parses a weekday, in full or abbreviated.
pub fn parse_day(s: &str) -> Result<Day> {
    let s = s.trim().to_ascii_lowercase();
    Ok(match s.as_str() {
        "mon" | "monday" => Day::Monday,
        "tue" | "tuesday" => Day::Tuesday,
        "wed" | "wednesday" => Day::Wednesday,
        "thu" | "thursday" => Day::Thursday,
        "fri" | "friday" => Day::Friday,
        "sat" | "saturday" => Day::Saturday,
        "sun" | "sunday" => Day::Sunday,
        other => bail!("unknown weekday `{other}` (use mon..sun or the full name)"),
    })
}

/// `HH:MM`, or `HH:MM:SS`.
pub fn parse_time(s: &str) -> Result<civil::Time> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        bail!("`{s}` is not a time of day (expected HH:MM)");
    }
    let hour: i8 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("bad hour in `{s}`"))?;
    let minute: i8 = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("bad minute in `{s}`"))?;
    let second: i8 = match parts.get(2) {
        Some(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("bad second in `{s}`"))?,
        None => 0,
    };
    civil::Time::new(hour, minute, second, 0)
        .map_err(|_| anyhow::anyhow!("`{s}` is not a valid time of day"))
}

/// A quota argument: either `2h` for every day, or `sat=3h` for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaArg {
    pub day: Option<Day>,
    pub quota: Quota,
}

impl std::str::FromStr for QuotaArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (day, value) = match s.split_once('=') {
            Some((d, v)) => (Some(parse_day(d)?), v),
            None => (None, s),
        };
        let value = value.trim();
        let quota = if value.eq_ignore_ascii_case("unlimited") {
            Quota::Unlimited
        } else {
            Quota::Limited(value.parse::<DurationSpec>()?)
        };
        Ok(Self { day, quota })
    }
}

/// A window argument: `mon=15:00-19:00`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowArg {
    pub day: Day,
    pub window: TimeWindow,
}

impl std::str::FromStr for WindowArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let Some((day, range)) = s.split_once('=') else {
            bail!("`{s}` needs the form DAY=HH:MM-HH:MM, for example mon=15:00-19:00");
        };
        let day = parse_day(day)?;
        let Some((from, to)) = range.split_once('-') else {
            bail!("`{range}` needs the form HH:MM-HH:MM");
        };
        let (start, end) = (parse_time(from)?, parse_time(to)?);
        if start == end {
            bail!("a window from {from} to {to} is empty");
        }
        Ok(Self {
            day,
            window: TimeWindow::new(start, end),
        })
    }
}

/// Everything `tbctl policy set` can change. All optional: an argument that is
/// not given leaves that part of the policy alone.
#[derive(Debug, Clone, Default)]
pub struct PolicyEdit {
    pub enforcement: Option<bool>,
    pub timezone: Option<String>,
    pub day_start: Option<civil::Time>,
    pub daily: Vec<QuotaArg>,
    pub weekly: Option<Quota>,
    pub windows: Vec<WindowArg>,
    pub clear_windows: bool,
    pub grace_period: Option<DurationSpec>,
    pub idle_threshold: Option<DurationSpec>,
    pub on_exhausted: Option<LockAction>,
    pub record_window_titles: Option<bool>,
    pub warnings: Option<Vec<DurationSpec>>,
}

impl PolicyEdit {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enforcement.is_none()
            && self.timezone.is_none()
            && self.day_start.is_none()
            && self.daily.is_empty()
            && self.weekly.is_none()
            && self.windows.is_empty()
            && !self.clear_windows
            && self.grace_period.is_none()
            && self.idle_threshold.is_none()
            && self.on_exhausted.is_none()
            && self.record_window_titles.is_none()
            && self.warnings.is_none()
    }

    /// Applies the changes and validates the result.
    ///
    /// The version is bumped only if validation passes, so a rejected edit
    /// leaves no trace and cannot be mistaken for a newer policy by the hub.
    pub fn apply(&self, base: &Policy) -> Result<Policy> {
        let mut p = base.clone();

        if let Some(v) = self.enforcement {
            p.enforcement = v;
        }
        if let Some(tz) = &self.timezone {
            p.timezone.clone_from(tz);
        }
        if let Some(t) = self.day_start {
            p.day_start = t;
        }
        for arg in &self.daily {
            match arg.day {
                Some(d) => p.daily_quota.set(d, arg.quota),
                // Setting the default alone would leave earlier per-day
                // overrides in place, so `--daily 2h` would silently not apply
                // to a day someone configured last month.
                None => p.daily_quota = tb_core::schedule::WeekSchedule::uniform(arg.quota),
            }
        }
        if let Some(q) = self.weekly {
            p.weekly_quota = q;
        }
        if self.clear_windows {
            p.allowed_windows = tb_core::schedule::WeekSchedule::uniform(Vec::new());
        }
        for arg in &self.windows {
            let mut existing = p.allowed_windows.get(arg.day).clone();
            if !existing.contains(&arg.window) {
                existing.push(arg.window);
            }
            existing.sort_by_key(|w| w.start);
            p.allowed_windows.set(arg.day, existing);
        }
        if let Some(d) = self.grace_period {
            p.grace_period = d;
        }
        if let Some(d) = self.idle_threshold {
            p.idle_threshold = d;
        }
        if let Some(a) = self.on_exhausted {
            p.on_exhausted = a;
        }
        if let Some(v) = self.record_window_titles {
            p.record_window_titles = v;
        }
        if let Some(w) = &self.warnings {
            p.warnings.clone_from(w);
        }

        p.validate()?;
        p.version = base.version.saturating_add(1);
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;

    fn base() -> Policy {
        Policy::permissive("kid")
    }

    #[test]
    fn weekdays_parse_long_and_short() {
        assert_eq!(parse_day("mon").unwrap(), Day::Monday);
        assert_eq!(parse_day("Monday").unwrap(), Day::Monday);
        assert_eq!(parse_day(" SUN ").unwrap(), Day::Sunday);
        assert!(parse_day("someday").is_err());
    }

    #[test]
    fn times_parse_and_reject_nonsense() {
        assert_eq!(parse_time("15:00").unwrap(), civil::time(15, 0, 0, 0));
        assert_eq!(parse_time("07:30:15").unwrap(), civil::time(7, 30, 15, 0));
        assert!(parse_time("25:00").is_err());
        assert!(parse_time("15").is_err());
        assert!(parse_time("15:").is_err());
        assert!(parse_time("quarter past three").is_err());
    }

    #[test]
    fn quota_arguments_cover_both_forms() {
        let all: QuotaArg = "2h".parse().unwrap();
        assert_eq!(all.day, None);
        assert_eq!(all.quota, Quota::Limited(DurationSpec::from_hours(2)));

        let one: QuotaArg = "sat=3h".parse().unwrap();
        assert_eq!(one.day, Some(Day::Saturday));

        let none: QuotaArg = "mon=unlimited".parse().unwrap();
        assert_eq!(none.quota, Quota::Unlimited);

        assert!(QuotaArg::from_str("sat=").is_err());
        assert!(QuotaArg::from_str("someday=2h").is_err());
    }

    #[test]
    fn window_arguments_parse() {
        let w: WindowArg = "mon=15:00-19:00".parse().unwrap();
        assert_eq!(w.day, Day::Monday);
        assert_eq!(w.window.start, civil::time(15, 0, 0, 0));
        assert_eq!(w.window.end, civil::time(19, 0, 0, 0));

        // Across midnight is legitimate and must survive parsing.
        let w: WindowArg = "fri=22:00-01:00".parse().unwrap();
        assert!(w.window.wraps_midnight());

        assert!(WindowArg::from_str("mon=15:00").is_err());
        assert!(WindowArg::from_str("15:00-19:00").is_err());
        assert!(
            WindowArg::from_str("mon=15:00-15:00").is_err(),
            "empty window"
        );
    }

    #[test]
    fn an_empty_edit_changes_only_the_version() {
        let p = PolicyEdit::default().apply(&base()).unwrap();
        assert_eq!(p.version, base().version + 1);
        assert_eq!(
            Policy {
                version: base().version,
                ..p
            },
            base()
        );
    }

    #[test]
    fn setting_the_default_quota_clears_stale_per_day_overrides() {
        // Otherwise `--daily 2h` would silently not apply to a Saturday somebody
        // configured months ago, and nobody would find out until it mattered.
        let mut start = base();
        start
            .daily_quota
            .set(Day::Saturday, Quota::Limited(DurationSpec::from_hours(5)));

        let edit = PolicyEdit {
            daily: vec!["2h".parse().unwrap()],
            ..PolicyEdit::default()
        };
        let p = edit.apply(&start).unwrap();
        assert_eq!(
            *p.daily_quota.get(Day::Saturday),
            Quota::Limited(DurationSpec::from_hours(2))
        );
    }

    #[test]
    fn a_default_and_an_override_can_be_given_together() {
        let edit = PolicyEdit {
            daily: vec!["2h".parse().unwrap(), "sat=3h".parse().unwrap()],
            ..PolicyEdit::default()
        };
        let p = edit.apply(&base()).unwrap();
        assert_eq!(
            *p.daily_quota.get(Day::Monday),
            Quota::Limited(DurationSpec::from_hours(2))
        );
        assert_eq!(
            *p.daily_quota.get(Day::Saturday),
            Quota::Limited(DurationSpec::from_hours(3))
        );
    }

    #[test]
    fn windows_accumulate_and_stay_sorted() {
        let edit = PolicyEdit {
            windows: vec![
                "mon=17:00-19:00".parse().unwrap(),
                "mon=07:00-08:00".parse().unwrap(),
            ],
            ..PolicyEdit::default()
        };
        let p = edit.apply(&base()).unwrap();
        let windows = p.allowed_windows.get(Day::Monday);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].start, civil::time(7, 0, 0, 0));
    }

    #[test]
    fn adding_the_same_window_twice_adds_it_once() {
        let edit = PolicyEdit {
            windows: vec!["mon=15:00-19:00".parse().unwrap()],
            ..PolicyEdit::default()
        };
        let once = edit.apply(&base()).unwrap();
        let twice = edit.apply(&once).unwrap();
        assert_eq!(twice.allowed_windows.get(Day::Monday).len(), 1);
    }

    #[test]
    fn windows_can_be_cleared_and_replaced_in_one_go() {
        let mut start = base();
        start.allowed_windows.set(
            Day::Monday,
            vec![TimeWindow::new(
                civil::time(7, 0, 0, 0),
                civil::time(8, 0, 0, 0),
            )],
        );
        let edit = PolicyEdit {
            clear_windows: true,
            windows: vec!["mon=15:00-19:00".parse().unwrap()],
            ..PolicyEdit::default()
        };
        let p = edit.apply(&start).unwrap();
        let windows = p.allowed_windows.get(Day::Monday);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start, civil::time(15, 0, 0, 0));
    }

    #[test]
    fn a_rejected_edit_does_not_bump_the_version() {
        // A version bump on an invalid policy would let the hub mistake a
        // discarded change for a newer one.
        let edit = PolicyEdit {
            timezone: Some("Mars/Olympus_Mons".to_owned()),
            ..PolicyEdit::default()
        };
        assert!(edit.apply(&base()).is_err());
    }

    #[test]
    fn contradictory_edits_are_refused_rather_than_stored() {
        let edit = PolicyEdit {
            daily: vec!["mon=0s".parse().unwrap()],
            windows: vec!["mon=15:00-19:00".parse().unwrap()],
            ..PolicyEdit::default()
        };
        let err = edit.apply(&base()).unwrap_err().to_string();
        assert!(err.contains("contradictory"), "got: {err}");
    }

    #[test]
    fn enforcement_can_be_switched_on() {
        let edit = PolicyEdit {
            enforcement: Some(true),
            daily: vec!["2h".parse().unwrap()],
            timezone: Some("Europe/Berlin".to_owned()),
            ..PolicyEdit::default()
        };
        let p = edit.apply(&base()).unwrap();
        assert!(p.enforcement);
        assert_eq!(p.timezone, "Europe/Berlin");
    }
}
