// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The rules that apply to one managed user.
//!
//! A `Policy` is pure configuration: it describes *what* is allowed but knows
//! neither past usage nor the current time. Evaluation lives in [`crate::engine`].

use jiff::civil;
use serde::{Deserialize, Serialize};

use crate::duration::DurationSpec;
use crate::schedule::{Day, TimeWindow, WeekSchedule};

/// A quota — either limited or explicitly unlimited.
///
/// A dedicated type rather than `Option<DurationSpec>` so "unlimited" is spelled
/// out in the configuration instead of arising from a *missing* field. A
/// forgotten field must never silently grant unrestricted access.
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

    /// Quota left after `used`, or `None` when unlimited.
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
                f.write_str("`unlimited` or a duration such as `2h`")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Quota, E> {
                if v.eq_ignore_ascii_case("unlimited") {
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
                    .map_err(|_| de::Error::custom("negative quota"))
            }
        }
        de.deserialize_any(V)
    }
}

/// What happens once the quota runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockAction {
    /// Lock the session. Unlocking then fails in the PAM module.
    #[default]
    Lock,
    /// End the session after the grace period. Unsaved work is lost.
    Terminate,
    /// Lock first, then terminate once the grace period expires.
    LockThenTerminate,
}

/// How to react when tracking was demonstrably tampered with (agent killed,
/// KWin script disabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TamperResponse {
    /// Keep counting, attribute time to `unknown`, report the event. Default.
    #[default]
    CountAndReport,
    /// Lock right away. For older children who game the system deliberately.
    LockImmediately,
}

/// The complete rule set for one user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Monotonically increasing. A client only adopts newer versions from the hub.
    pub version: u64,

    /// The Unix user this applies to.
    pub subject: String,

    /// Is enforcement active? `false` means observe-only.
    #[serde(default = "default_true")]
    pub enforcement: bool,

    /// IANA time zone that day boundaries and windows are expressed in.
    #[serde(default = "default_timezone")]
    pub timezone: String,

    /// Start of the policy day. 04:00 by default so late evenings count towards
    /// the day they started on.
    #[serde(default = "default_day_start")]
    pub day_start: civil::Time,

    /// Daily quota per weekday.
    #[serde(default = "default_daily_quota")]
    pub daily_quota: WeekSchedule<Quota>,

    /// Additional ceiling across the whole week (Monday–Sunday).
    #[serde(default)]
    pub weekly_quota: Quota,

    /// Allowed windows per weekday. **An empty list means all day.** A day on
    /// which nothing should be allowed gets `daily_quota = "0s"` instead.
    #[serde(default = "default_windows")]
    pub allowed_windows: WeekSchedule<Vec<TimeWindow>>,

    /// Remaining-time thresholds that trigger a warning.
    #[serde(default = "default_warnings")]
    pub warnings: Vec<DurationSpec>,

    /// Grace period between locking and terminating, to save open work.
    #[serde(default = "default_grace")]
    pub grace_period: DurationSpec,

    /// How much inactivity stops the clock.
    #[serde(default = "default_idle")]
    pub idle_threshold: DurationSpec,

    #[serde(default)]
    pub on_exhausted: LockAction,

    #[serde(default)]
    pub on_tamper: TamperResponse,

    /// Record window titles. Defaults to `false` — collect as little as possible.
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
    /// A policy that restricts nothing — the starting point for new users and
    /// what observe-only mode (M1) runs with.
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

    /// Warning thresholds, descending and deduplicated.
    #[must_use]
    pub fn sorted_warnings(&self) -> Vec<DurationSpec> {
        let mut w = self.warnings.clone();
        w.sort_unstable_by(|a, b| b.cmp(a));
        w.dedup();
        w
    }

    /// Checks the policy for contradictions before it is adopted.
    ///
    /// Called both by the hub on save and by the daemon on load — a broken
    /// policy must never turn into a lockout.
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
            // Windows plus a zero quota is a contradiction — almost certainly a
            // mistake made in the editing UI.
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

/// Reasons a policy is rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy has no subject")]
    EmptySubject,
    #[error("unknown time zone `{0}`")]
    UnknownTimezone(String),
    #[error("time window {window} on {day} is empty (start equals end)")]
    EmptyWindow { day: Day, window: TimeWindow },
    #[error("{0}: time windows are set but the daily quota is zero — contradictory")]
    WindowsWithZeroQuota(Day),
    #[error("weekly quota exceeds the sum of all daily quotas and can never be reached")]
    UnreachableWeeklyQuota,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_serializes_readably() {
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
    fn permissive_policy_is_valid() {
        let p = Policy::permissive("kid");
        assert!(p.validate().is_ok());
        assert!(!p.enforcement, "observe-only is the default");
    }

    #[test]
    fn validate_catches_unknown_timezone() {
        let mut p = Policy::permissive("kid");
        p.timezone = "Mars/Olympus_Mons".to_owned();
        assert_eq!(
            p.validate(),
            Err(PolicyError::UnknownTimezone("Mars/Olympus_Mons".to_owned()))
        );
    }

    #[test]
    fn validate_catches_empty_window() {
        let mut p = Policy::permissive("kid");
        let t = civil::time(15, 0, 0, 0);
        p.allowed_windows
            .set(Day::Monday, vec![TimeWindow::new(t, t)]);
        assert!(matches!(p.validate(), Err(PolicyError::EmptyWindow { .. })));
    }

    #[test]
    fn validate_catches_unreachable_weekly_quota() {
        let mut p = Policy::permissive("kid");
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(1)));
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(20)); // 7h is the ceiling
        assert_eq!(p.validate(), Err(PolicyError::UnreachableWeeklyQuota));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // Guards against typos in hand-written policy files that would otherwise
        // silently read as "not configured".
        let json = r#"{"version":1,"subject":"kid","daily_qouta":"2h"}"#;
        assert!(serde_json::from_str::<Policy>(json).is_err());
    }

    #[test]
    fn minimal_policy_gets_safe_defaults() {
        let p: Policy = serde_json::from_str(r#"{"version":1,"subject":"kid"}"#).unwrap();
        assert_eq!(p.day_start, civil::time(4, 0, 0, 0));
        assert_eq!(p.idle_threshold, DurationSpec::from_mins(2));
        assert!(!p.record_window_titles);
        assert_eq!(p.sorted_warnings().len(), 3);
    }
}
