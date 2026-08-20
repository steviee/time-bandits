// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Weekdays, time windows, and the definition of a "policy day".
//!
//! A policy day is deliberately *not* a calendar day: it starts at a
//! configurable wall-clock time (04:00 by default). Otherwise a child still
//! playing at 23:50 would be handed a fresh daily quota ten minutes later.

use std::fmt;

use jiff::civil::{self, Weekday};
use jiff::tz::TimeZone;
use jiff::{Zoned, ZonedRound};
use serde::{Deserialize, Serialize};

/// A weekday with a stable, language-independent serialization.
///
/// `jiff::civil::Weekday` deliberately does not get serde support bolted on;
/// this type is the boundary towards configuration files and the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Day {
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    #[must_use]
    pub const fn is_weekend(self) -> bool {
        matches!(self, Self::Saturday | Self::Sunday)
    }
}

impl From<Weekday> for Day {
    fn from(w: Weekday) -> Self {
        match w {
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
            Weekday::Sunday => Self::Sunday,
        }
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Monday => "monday",
            Self::Tuesday => "tuesday",
            Self::Wednesday => "wednesday",
            Self::Thursday => "thursday",
            Self::Friday => "friday",
            Self::Saturday => "saturday",
            Self::Sunday => "sunday",
        };
        // `pad`, not `write_str`: the latter silently ignores width and
        // alignment, which quietly breaks every table this appears in.
        f.pad(s)
    }
}

/// One value per weekday, with a default and optional per-day overrides.
///
/// In TOML/JSON:
/// ```toml
/// [daily_quota]
/// default = "2h"
/// saturday = "3h"
/// sunday = "3h"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekSchedule<T> {
    pub default: T,
    #[serde(flatten, default, skip_serializing_if = "DayOverrides::is_empty")]
    pub overrides: DayOverrides<T>,
}

impl<T> WeekSchedule<T> {
    pub const fn uniform(default: T) -> Self {
        Self {
            default,
            overrides: DayOverrides::empty(),
        }
    }

    /// The value for a weekday: its override, otherwise the default.
    pub fn get(&self, day: Day) -> &T {
        self.overrides.get(day).unwrap_or(&self.default)
    }

    pub fn set(&mut self, day: Day, value: T) {
        self.overrides.set(day, value);
    }
}

/// Per-weekday overrides. A dedicated struct so `#[serde(flatten)]` writes the
/// days as siblings of `default` instead of nesting them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DayOverrides<T> {
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub monday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub tuesday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub wednesday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub thursday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub friday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub saturday: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Option::default")]
    pub sunday: Option<T>,
}

impl<T> DayOverrides<T> {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            monday: None,
            tuesday: None,
            wednesday: None,
            thursday: None,
            friday: None,
            saturday: None,
            sunday: None,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        Day::ALL.iter().all(|&d| self.get(d).is_none())
    }

    #[must_use]
    pub fn get(&self, day: Day) -> Option<&T> {
        match day {
            Day::Monday => self.monday.as_ref(),
            Day::Tuesday => self.tuesday.as_ref(),
            Day::Wednesday => self.wednesday.as_ref(),
            Day::Thursday => self.thursday.as_ref(),
            Day::Friday => self.friday.as_ref(),
            Day::Saturday => self.saturday.as_ref(),
            Day::Sunday => self.sunday.as_ref(),
        }
    }

    pub fn set(&mut self, day: Day, value: T) {
        let slot = match day {
            Day::Monday => &mut self.monday,
            Day::Tuesday => &mut self.tuesday,
            Day::Wednesday => &mut self.wednesday,
            Day::Thursday => &mut self.thursday,
            Day::Friday => &mut self.friday,
            Day::Saturday => &mut self.saturday,
            Day::Sunday => &mut self.sunday,
        };
        *slot = Some(value);
    }
}

/// An allowed window inside a policy day, e.g. 15:00–19:00.
///
/// `end` may be earlier than `start`; the window then continues past midnight
/// (e.g. 22:00–01:00) and still belongs to the same policy day as long as it
/// ends before that day does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: civil::Time,
    pub end: civil::Time,
}

impl TimeWindow {
    #[must_use]
    pub const fn new(start: civil::Time, end: civil::Time) -> Self {
        Self { start, end }
    }

    /// Covers the whole day — handy as "no bedtime restriction".
    #[must_use]
    pub fn all_day() -> Self {
        Self::new(civil::Time::midnight(), civil::Time::MAX)
    }

    #[must_use]
    pub fn wraps_midnight(&self) -> bool {
        self.end < self.start
    }

    /// Does the window contain this wall-clock time? Half-open: `[start, end)`.
    #[must_use]
    pub fn contains(&self, t: civil::Time) -> bool {
        if self.wraps_midnight() {
            t >= self.start || t < self.end
        } else {
            t >= self.start && t < self.end
        }
    }
}

impl fmt::Display for TimeWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}-{:02}:{:02}",
            self.start.hour(),
            self.start.minute(),
            self.end.hour(),
            self.end.minute()
        )
    }
}

/// The policy day a moment belongs to, plus the weekday to look rules up under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyDay {
    pub date: civil::Date,
    pub day: Day,
}

/// Determines the policy day: before `day_start`, a moment still counts towards
/// the previous day.
#[must_use]
pub fn policy_day(now: &Zoned, day_start: civil::Time) -> PolicyDay {
    let date = if now.time() < day_start {
        now.date().yesterday().unwrap_or_else(|_| now.date())
    } else {
        now.date()
    };
    PolicyDay {
        date,
        day: Day::from(date.weekday()),
    }
}

/// When does the current policy day end, as a real instant?
///
/// A `day_start` inside a daylight-saving gap moves forward rather than
/// failing — `jiff`'s compatible disambiguation. That matters twice a year with
/// the default 04:00 nowhere near the gap, and more often for anyone who sets
/// `day_start` into the small hours.
#[must_use]
pub fn policy_day_end(day: PolicyDay, day_start: civil::Time, tz: &TimeZone) -> Zoned {
    let next = day.date.tomorrow().unwrap_or(day.date);
    let wall = next.to_datetime(day_start);
    wall.to_zoned(tz.clone()).unwrap_or_else(|_| {
        wall.to_zoned(TimeZone::UTC)
            .unwrap_or_else(|_| Zoned::new(jiff::Timestamp::UNIX_EPOCH, TimeZone::UTC))
    })
}

/// Truncates to whole seconds — ticks are second-granular, and stray
/// nanoseconds make comparisons flicker at window boundaries.
#[must_use]
pub fn truncate_to_second(z: &Zoned) -> Zoned {
    z.round(ZonedRound::new().smallest(jiff::Unit::Second))
        .unwrap_or_else(|_| z.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Berlin").expect("tzdb knows Europe/Berlin")
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .expect("valid instant")
    }

    #[test]
    fn window_without_midnight() {
        let w = TimeWindow::new(civil::time(15, 0, 0, 0), civil::time(19, 0, 0, 0));
        assert!(!w.wraps_midnight());
        assert!(!w.contains(civil::time(14, 59, 0, 0)));
        assert!(w.contains(civil::time(15, 0, 0, 0)));
        assert!(w.contains(civil::time(18, 59, 0, 0)));
        // Half-open: the end is no longer inside.
        assert!(!w.contains(civil::time(19, 0, 0, 0)));
    }

    #[test]
    fn window_across_midnight() {
        let w = TimeWindow::new(civil::time(22, 0, 0, 0), civil::time(1, 0, 0, 0));
        assert!(w.wraps_midnight());
        assert!(w.contains(civil::time(23, 30, 0, 0)));
        assert!(w.contains(civil::time(0, 30, 0, 0)));
        assert!(!w.contains(civil::time(1, 0, 0, 0)));
        assert!(!w.contains(civil::time(12, 0, 0, 0)));
    }

    #[test]
    fn policy_day_starts_at_four() {
        let day_start = civil::time(4, 0, 0, 0);
        // 23:50 on Tuesday belongs to policy day Tuesday.
        let pd = policy_day(&at(2026, 8, 18, 23, 50), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 18));
        assert_eq!(pd.day, Day::Tuesday);
        // 00:30 that night still belongs to Tuesday.
        let pd = policy_day(&at(2026, 8, 19, 0, 30), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 18));
        assert_eq!(pd.day, Day::Tuesday);
        // 04:00 opens the new policy day.
        let pd = policy_day(&at(2026, 8, 19, 4, 0), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 19));
        assert_eq!(pd.day, Day::Wednesday);
    }

    #[test]
    fn policy_day_end_survives_dst_transition() {
        let day_start = civil::time(4, 0, 0, 0);
        // In Europe the hour 02:00–03:00 is skipped on the night of 2026-03-29.
        // 04:00 still exists, but the day is only 23 hours long.
        let pd = policy_day(&at(2026, 3, 28, 10, 0), day_start);
        let end = policy_day_end(pd, day_start, &tz());
        assert_eq!(end.date(), civil::date(2026, 3, 29));
        assert_eq!(end.hour(), 4);
        let start = at(2026, 3, 28, 4, 0);
        let hours = start.duration_until(&end).as_secs() / 3600;
        assert_eq!(hours, 23, "the spring-forward day has 23 hours");
    }

    #[test]
    fn weekday_display_honours_column_width() {
        // A Display impl written with `write_str` ignores width and alignment,
        // which turns every table it appears in into ragged output.
        assert_eq!(format!("{:<10}|", Day::Monday), "monday    |");
        assert_eq!(format!("{:>10}|", Day::Sunday), "    sunday|");
        assert_eq!(format!("{}", Day::Friday), "friday");
    }

    #[test]
    fn a_policy_day_that_starts_in_a_skipped_hour_moves_forward() {
        // Twice a year in the target market, and reachable whenever day_start
        // is set into the small hours.
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        let pd = PolicyDay {
            date: civil::date(2026, 3, 28),
            day: Day::Saturday,
        };
        let end = policy_day_end(pd, civil::time(2, 30, 0, 0), &tz);
        assert_eq!(end.date(), civil::date(2026, 3, 29));
        assert_eq!(end.hour(), 3, "02:30 does not exist, so it becomes 03:30");
        assert_eq!(end.minute(), 30);
    }

    #[test]
    fn week_schedule_falls_back_to_default() {
        let mut s = WeekSchedule::uniform(crate::DurationSpec::from_hours(2));
        assert_eq!(*s.get(Day::Monday), crate::DurationSpec::from_hours(2));
        s.set(Day::Saturday, crate::DurationSpec::from_hours(3));
        assert_eq!(*s.get(Day::Saturday), crate::DurationSpec::from_hours(3));
        assert_eq!(*s.get(Day::Sunday), crate::DurationSpec::from_hours(2));
    }

    #[test]
    fn week_schedule_serializes_flat() {
        let mut s = WeekSchedule::uniform(crate::DurationSpec::from_hours(2));
        s.set(Day::Saturday, crate::DurationSpec::from_hours(3));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"default":"2h","saturday":"3h"}"#);
        let back: WeekSchedule<crate::DurationSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
