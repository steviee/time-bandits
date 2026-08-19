// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Wochentage, Zeitfenster und die Definition des „Policy-Tages“.
//!
//! Ein Policy-Tag ist bewusst *nicht* der Kalendertag: er beginnt zu einer
//! konfigurierbaren Uhrzeit (Vorgabe 04:00). Sonst würde ein Kind, das um 23:50
//! noch spielt, um 00:00 ein frisches Tageskontingent bekommen.

use std::fmt;

use jiff::civil::{self, Weekday};
use jiff::tz::TimeZone;
use jiff::{Zoned, ZonedRound};
use serde::{Deserialize, Serialize};

/// Wochentag mit stabiler, sprachunabhängiger Serialisierung.
///
/// `jiff::civil::Weekday` bekommt bewusst keinen eigenen serde-Support
/// aufgesetzt — dieser Typ ist die Schnittstelle zu Konfiguration und API.
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
        f.write_str(s)
    }
}

/// Ein Wert pro Wochentag, mit Vorgabewert und optionalen Ausnahmen.
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

    /// Wert für einen Wochentag: Ausnahme, sonst Vorgabe.
    pub fn get(&self, day: Day) -> &T {
        self.overrides.get(day).unwrap_or(&self.default)
    }

    pub fn set(&mut self, day: Day, value: T) {
        self.overrides.set(day, value);
    }
}

/// Ausnahmen je Wochentag. Eigener Typ, damit `#[serde(flatten)]` die Tage als
/// Felder auf gleicher Ebene wie `default` schreibt statt verschachtelt.
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

/// Ein erlaubtes Zeitfenster innerhalb eines Policy-Tages, z. B. 15:00–19:00.
///
/// `end` darf kleiner als `start` sein; das Fenster läuft dann über Mitternacht
/// hinweg weiter (z. B. 22:00–01:00) und gehört trotzdem zum selben Policy-Tag,
/// solange es vor dessen Ende liegt.
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

    /// Ganztägig — nützlich als Vorgabe „kein Bettzeit-Limit“.
    #[must_use]
    pub fn all_day() -> Self {
        Self::new(civil::Time::midnight(), civil::Time::MAX)
    }

    #[must_use]
    pub fn wraps_midnight(&self) -> bool {
        self.end < self.start
    }

    /// Enthält das Fenster diese Wanduhrzeit? Halboffen: `[start, end)`.
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

/// Der Policy-Tag, zu dem ein Zeitpunkt gehört, plus der Wochentag dafür.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyDay {
    pub date: civil::Date,
    pub day: Day,
}

/// Bestimmt den Policy-Tag: liegt die Uhrzeit vor `day_start`, zählt der Zeitpunkt
/// noch zum Vortag.
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

/// Wann endet der aktuelle Policy-Tag als echter Zeitpunkt?
///
/// Geht über `jiff` und respektiert damit Sommerzeitwechsel: fällt `day_start`
/// in eine übersprungene Stunde, rückt der Zeitpunkt nach vorn statt zu scheitern.
#[must_use]
pub fn policy_day_end(day: PolicyDay, day_start: civil::Time, tz: &TimeZone) -> Zoned {
    let next = day.date.tomorrow().unwrap_or(day.date);
    next.to_datetime(day_start)
        .to_zoned(tz.clone())
        .unwrap_or_else(|_| {
            // Nur erreichbar, wenn die Zeitzone diesen Zeitpunkt gar nicht kennt;
            // dann lieber auf Mitternacht zurückfallen als die Auswertung abbrechen.
            next.to_datetime(civil::Time::midnight())
                .to_zoned(tz.clone())
                .expect("Mitternacht existiert in jeder Zeitzone")
        })
}

/// Rundet auf volle Sekunden ab — Ticks arbeiten sekundengenau, und
/// Nanosekunden in Vergleichen führen zu Flackern an Fenstergrenzen.
#[must_use]
pub fn truncate_to_second(z: &Zoned) -> Zoned {
    z.round(ZonedRound::new().smallest(jiff::Unit::Second))
        .unwrap_or_else(|_| z.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Berlin").expect("tzdb kennt Europe/Berlin")
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .expect("gültiger Zeitpunkt")
    }

    #[test]
    fn zeitfenster_ohne_mitternacht() {
        let w = TimeWindow::new(civil::time(15, 0, 0, 0), civil::time(19, 0, 0, 0));
        assert!(!w.wraps_midnight());
        assert!(!w.contains(civil::time(14, 59, 0, 0)));
        assert!(w.contains(civil::time(15, 0, 0, 0)));
        assert!(w.contains(civil::time(18, 59, 0, 0)));
        // Halboffen: das Ende gehört nicht mehr dazu.
        assert!(!w.contains(civil::time(19, 0, 0, 0)));
    }

    #[test]
    fn zeitfenster_ueber_mitternacht() {
        let w = TimeWindow::new(civil::time(22, 0, 0, 0), civil::time(1, 0, 0, 0));
        assert!(w.wraps_midnight());
        assert!(w.contains(civil::time(23, 30, 0, 0)));
        assert!(w.contains(civil::time(0, 30, 0, 0)));
        assert!(!w.contains(civil::time(1, 0, 0, 0)));
        assert!(!w.contains(civil::time(12, 0, 0, 0)));
    }

    #[test]
    fn policy_tag_beginnt_um_vier_uhr() {
        let day_start = civil::time(4, 0, 0, 0);
        // 23:50 am Dienstag gehört zum Policy-Tag Dienstag.
        let pd = policy_day(&at(2026, 8, 18, 23, 50), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 18));
        assert_eq!(pd.day, Day::Tuesday);
        // 00:30 in der Nacht darauf gehört immer noch zu Dienstag.
        let pd = policy_day(&at(2026, 8, 19, 0, 30), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 18));
        assert_eq!(pd.day, Day::Tuesday);
        // 04:00 startet den neuen Policy-Tag.
        let pd = policy_day(&at(2026, 8, 19, 4, 0), day_start);
        assert_eq!(pd.date, civil::date(2026, 8, 19));
        assert_eq!(pd.day, Day::Wednesday);
    }

    #[test]
    fn policy_tag_ende_ueberlebt_sommerzeitwechsel() {
        let day_start = civil::time(4, 0, 0, 0);
        // In Europa wird in der Nacht auf den 29.03.2026 die Stunde 02:00–03:00
        // übersprungen. 04:00 existiert weiterhin, der Tag ist aber nur 23 h lang.
        let pd = policy_day(&at(2026, 3, 28, 10, 0), day_start);
        let end = policy_day_end(pd, day_start, &tz());
        assert_eq!(end.date(), civil::date(2026, 3, 29));
        assert_eq!(end.hour(), 4);
        let start = at(2026, 3, 28, 4, 0);
        let hours = start.duration_until(&end).as_secs() / 3600;
        assert_eq!(hours, 23, "Tag des Sommerzeitwechsels hat 23 Stunden");
    }

    #[test]
    fn wochenplan_faellt_auf_default_zurueck() {
        let mut s = WeekSchedule::uniform(crate::DurationSpec::from_hours(2));
        assert_eq!(*s.get(Day::Monday), crate::DurationSpec::from_hours(2));
        s.set(Day::Saturday, crate::DurationSpec::from_hours(3));
        assert_eq!(*s.get(Day::Saturday), crate::DurationSpec::from_hours(3));
        assert_eq!(*s.get(Day::Sunday), crate::DurationSpec::from_hours(2));
    }

    #[test]
    fn wochenplan_serialisiert_flach() {
        let mut s = WeekSchedule::uniform(crate::DurationSpec::from_hours(2));
        s.set(Day::Saturday, crate::DurationSpec::from_hours(3));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"default":"2h","saturday":"3h"}"#);
        let back: WeekSchedule<crate::DurationSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
