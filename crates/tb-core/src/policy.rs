// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Die Regeln, die für einen verwalteten Benutzer gelten.
//!
//! Eine `Policy` ist reine Konfiguration: sie beschreibt, *was* erlaubt ist, aber
//! kennt weder die bisherige Nutzung noch die aktuelle Uhrzeit. Die Auswertung
//! macht [`crate::engine`].

use jiff::civil;
use serde::{Deserialize, Serialize};

use crate::duration::DurationSpec;
use crate::schedule::{Day, TimeWindow, WeekSchedule};

/// Ein Kontingent — entweder begrenzt oder ausdrücklich unbegrenzt.
///
/// Eigener Typ statt `Option<DurationSpec>`, damit „unbegrenzt“ in der
/// Konfiguration sichtbar dasteht (`"unlimited"`) und nicht durch ein fehlendes
/// Feld entsteht. Ein vergessenes Feld darf nie versehentlich alles freigeben.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quota {
    #[default]
    Unlimited,
    Limited(DurationSpec),
}

impl Quota {
    #[must_use]
    pub const fn limit(self) -> Option<DurationSpec> {
        match self {
            Self::Unlimited => None,
            Self::Limited(d) => Some(d),
        }
    }

    /// Verbleibendes Kontingent nach `used`, oder `None` bei unbegrenzt.
    #[must_use]
    pub fn remaining(self, used: DurationSpec) -> Option<DurationSpec> {
        self.limit().map(|l| l.saturating_sub(used))
    }
}

impl Serialize for Quota {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unlimited => ser.serialize_str("unlimited"),
            Self::Limited(d) => d.serialize(ser),
        }
    }
}

impl<'de> Deserialize<'de> for Quota {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct V;
        impl Visitor<'_> for V {
            type Value = Quota;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("`unlimited` oder eine Dauer wie `2h`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Quota, E> {
                if v.eq_ignore_ascii_case("unlimited") || v.eq_ignore_ascii_case("unbegrenzt") {
                    return Ok(Quota::Unlimited);
                }
                v.parse().map(Quota::Limited).map_err(de::Error::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Quota, E> {
                Ok(Quota::Limited(DurationSpec::from_mins(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Quota, E> {
                u64::try_from(v)
                    .map(|m| Quota::Limited(DurationSpec::from_mins(m)))
                    .map_err(|_| de::Error::custom("negatives Kontingent"))
            }
        }
        de.deserialize_any(V)
    }
}

/// Was passiert, wenn das Kontingent aufgebraucht ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockAction {
    /// Sitzung sperren. Das Entsperren scheitert am PAM-Modul.
    #[default]
    Lock,
    /// Sitzung nach der Schonfrist beenden. Ungespeicherte Arbeit geht verloren.
    Terminate,
    /// Erst sperren, nach der Schonfrist zusätzlich beenden.
    LockThenTerminate,
}

/// Verhalten, wenn die Erfassung nachweislich manipuliert wurde
/// (Agent gekillt, KWin-Skript deaktiviert).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TamperResponse {
    /// Weiterzählen, App als `unknown` buchen, Ereignis melden. Vorgabe.
    #[default]
    CountAndReport,
    /// Sofort sperren. Für ältere Kinder, die gezielt austricksen.
    LockImmediately,
}

/// Die vollständige Regelmenge für einen Benutzer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Monoton steigend. Der Client übernimmt nur neuere Versionen vom Hub.
    pub version: u64,

    /// Betroffener Unix-Benutzername.
    pub subject: String,

    /// Ist die Durchsetzung aktiv? `false` = reiner Beobachtungsmodus.
    #[serde(default = "default_true")]
    pub enforcement: bool,

    /// IANA-Zeitzone, in der Tagesgrenzen und Zeitfenster gelten.
    #[serde(default = "default_timezone")]
    pub timezone: String,

    /// Beginn des Policy-Tages. Vorgabe 04:00, damit die Nacht zum Vortag zählt.
    #[serde(default = "default_day_start")]
    pub day_start: civil::Time,

    /// Tageskontingent je Wochentag.
    #[serde(default = "default_daily_quota")]
    pub daily_quota: WeekSchedule<Quota>,

    /// Zusätzliche Obergrenze über die gesamte Woche (Montag–Sonntag).
    #[serde(default)]
    pub weekly_quota: Quota,

    /// Erlaubte Zeitfenster je Wochentag. **Leere Liste = ganztägig erlaubt.**
    /// Ein Tag, an dem gar nichts erlaubt sein soll, bekommt stattdessen
    /// `daily_quota = "0s"`.
    #[serde(default = "default_windows")]
    pub allowed_windows: WeekSchedule<Vec<TimeWindow>>,

    /// Restzeiten, bei denen gewarnt wird. Absteigend sortiert erwartet.
    #[serde(default = "default_warnings")]
    pub warnings: Vec<DurationSpec>,

    /// Schonfrist zum Speichern zwischen Sperre und Beendigung.
    #[serde(default = "default_grace")]
    pub grace_period: DurationSpec,

    /// Ab welcher Untätigkeit die Zeit nicht mehr zählt.
    #[serde(default = "default_idle")]
    pub idle_threshold: DurationSpec,

    #[serde(default)]
    pub on_exhausted: LockAction,

    #[serde(default)]
    pub on_tamper: TamperResponse,

    /// Fenstertitel mitschreiben. Vorgabe `false` — Datensparsamkeit.
    #[serde(default)]
    pub record_window_titles: bool,
}

fn default_true() -> bool {
    true
}
fn default_timezone() -> String {
    "UTC".to_owned()
}
fn default_day_start() -> civil::Time {
    civil::time(4, 0, 0, 0)
}
fn default_daily_quota() -> WeekSchedule<Quota> {
    WeekSchedule::uniform(Quota::Unlimited)
}
fn default_windows() -> WeekSchedule<Vec<TimeWindow>> {
    WeekSchedule::uniform(Vec::new())
}
fn default_warnings() -> Vec<DurationSpec> {
    vec![
        DurationSpec::from_mins(15),
        DurationSpec::from_mins(5),
        DurationSpec::from_mins(1),
    ]
}
fn default_grace() -> DurationSpec {
    DurationSpec::from_secs(60)
}
fn default_idle() -> DurationSpec {
    DurationSpec::from_mins(2)
}

impl Policy {
    /// Eine Policy, die nichts einschränkt — Ausgangspunkt für neue Benutzer und
    /// das, was der Beobachtungsmodus (M1) verwendet.
    #[must_use]
    pub fn permissive(subject: impl Into<String>) -> Self {
        Self {
            version: 1,
            subject: subject.into(),
            enforcement: false,
            timezone: default_timezone(),
            day_start: default_day_start(),
            daily_quota: default_daily_quota(),
            weekly_quota: Quota::Unlimited,
            allowed_windows: default_windows(),
            warnings: default_warnings(),
            grace_period: default_grace(),
            idle_threshold: default_idle(),
            on_exhausted: LockAction::default(),
            on_tamper: TamperResponse::default(),
            record_window_titles: false,
        }
    }

    /// Warnschwellen absteigend und ohne Duplikate.
    #[must_use]
    pub fn sorted_warnings(&self) -> Vec<DurationSpec> {
        let mut w = self.warnings.clone();
        w.sort_unstable_by(|a, b| b.cmp(a));
        w.dedup();
        w
    }

    /// Prüft die Policy auf Widersprüche, bevor sie übernommen wird.
    ///
    /// Wird sowohl im Hub beim Speichern als auch im Daemon beim Laden
    /// aufgerufen — eine kaputte Policy darf nie zur Sperre führen.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.subject.trim().is_empty() {
            return Err(PolicyError::EmptySubject);
        }
        jiff::tz::TimeZone::get(&self.timezone)
            .map_err(|_| PolicyError::UnknownTimezone(self.timezone.clone()))?;

        for day in Day::ALL {
            let windows = self.allowed_windows.get(day);
            for w in windows {
                if w.start == w.end {
                    return Err(PolicyError::EmptyWindow { day, window: *w });
                }
            }
            // Ein Tag mit Fenstern, aber Kontingent 0, ist widersprüchlich
            // konfiguriert — vermutlich ein Versehen in der Oberfläche.
            if !windows.is_empty() && self.daily_quota.get(day).limit() == Some(DurationSpec::ZERO)
            {
                return Err(PolicyError::WindowsWithZeroQuota(day));
            }
        }

        if let Quota::Limited(weekly) = self.weekly_quota {
            let max_daily: u64 = Day::ALL
                .iter()
                .filter_map(|&d| self.daily_quota.get(d).limit())
                .map(DurationSpec::as_secs)
                .sum();
            let all_days_limited = Day::ALL
                .iter()
                .all(|&d| self.daily_quota.get(d).limit().is_some());
            if all_days_limited && weekly.as_secs() > max_daily {
                return Err(PolicyError::UnreachableWeeklyQuota);
            }
        }
        Ok(())
    }
}

/// Gründe, aus denen eine Policy abgelehnt wird.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("Policy ohne Benutzernamen")]
    EmptySubject,
    #[error("unbekannte Zeitzone `{0}`")]
    UnknownTimezone(String),
    #[error("Zeitfenster {window} am {day} ist leer (Start gleich Ende)")]
    EmptyWindow { day: Day, window: TimeWindow },
    #[error("{0}: Zeitfenster gesetzt, aber Tageskontingent ist 0 — widersprüchlich")]
    WindowsWithZeroQuota(Day),
    #[error("Wochenkontingent ist größer als die Summe aller Tageskontingente")]
    UnreachableWeeklyQuota,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_serialisiert_lesbar() {
        assert_eq!(
            serde_json::to_string(&Quota::Unlimited).unwrap(),
            r#""unlimited""#
        );
        assert_eq!(
            serde_json::to_string(&Quota::Limited(DurationSpec::from_hours(2))).unwrap(),
            r#""2h""#
        );
        assert_eq!(
            serde_json::from_str::<Quota>(r#""unlimited""#).unwrap(),
            Quota::Unlimited
        );
        assert_eq!(
            serde_json::from_str::<Quota>("90").unwrap(),
            Quota::Limited(DurationSpec::from_mins(90))
        );
    }

    #[test]
    fn permissive_policy_ist_gueltig() {
        let p = Policy::permissive("kid");
        assert!(p.validate().is_ok());
        assert!(!p.enforcement, "Vorgabe ist Beobachtungsmodus");
    }

    #[test]
    fn validate_faengt_unbekannte_zeitzone() {
        let mut p = Policy::permissive("kid");
        p.timezone = "Mars/Olympus_Mons".to_owned();
        assert_eq!(
            p.validate(),
            Err(PolicyError::UnknownTimezone("Mars/Olympus_Mons".to_owned()))
        );
    }

    #[test]
    fn validate_faengt_leeres_zeitfenster() {
        let mut p = Policy::permissive("kid");
        let t = civil::time(15, 0, 0, 0);
        p.allowed_windows
            .set(Day::Monday, vec![TimeWindow::new(t, t)]);
        assert!(matches!(p.validate(), Err(PolicyError::EmptyWindow { .. })));
    }

    #[test]
    fn validate_faengt_unerreichbares_wochenkontingent() {
        let mut p = Policy::permissive("kid");
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(1)));
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(20)); // max 7h möglich
        assert_eq!(p.validate(), Err(PolicyError::UnreachableWeeklyQuota));
    }

    #[test]
    fn unbekannte_felder_werden_abgelehnt() {
        // Schützt vor Tippfehlern in handgeschriebenen Policy-Dateien, die sonst
        // stillschweigend als „nicht gesetzt“ durchgehen würden.
        let json = r#"{"version":1,"subject":"kid","daily_qouta":"2h"}"#;
        assert!(serde_json::from_str::<Policy>(json).is_err());
    }

    #[test]
    fn minimale_policy_bekommt_sichere_vorgaben() {
        let p: Policy = serde_json::from_str(r#"{"version":1,"subject":"kid"}"#).unwrap();
        assert_eq!(p.day_start, civil::time(4, 0, 0, 0));
        assert_eq!(p.idle_threshold, DurationSpec::from_mins(2));
        assert!(!p.record_window_titles);
        assert_eq!(p.sorted_warnings().len(), 3);
    }
}
