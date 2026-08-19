// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Menschenlesbare Dauern (`"1h30m"`) für Konfiguration und Wire-Format.
//!
//! Eigene Implementierung statt einer Fremd-Crate, weil dieser Typ in *jeder*
//! Komponente vorkommt — inklusive des PAM-Moduls, das so wenige Abhängigkeiten
//! wie möglich haben soll.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Eine Zeitspanne, die als `"2h"`, `"90m"`, `"1h30m"` oder `"45s"` geschrieben wird.
///
/// Serialisiert immer als String, damit Konfigurationsdateien und JSON lesbar
/// bleiben und keine Mehrdeutigkeit über die Einheit entsteht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct DurationSpec(Duration);

impl DurationSpec {
    pub const ZERO: Self = Self(Duration::ZERO);

    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }

    #[must_use]
    pub const fn from_mins(mins: u64) -> Self {
        Self(Duration::from_secs(mins * 60))
    }

    #[must_use]
    pub const fn from_hours(hours: u64) -> Self {
        Self(Duration::from_secs(hours * 3600))
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0.as_secs()
    }

    /// Aufgerundet, damit „noch 30 Sekunden“ nicht als „0 Minuten“ angezeigt wird.
    #[must_use]
    pub const fn as_mins_ceil(self) -> u64 {
        self.0.as_secs().div_ceil(60)
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    /// Sättigende Subtraktion — Kontingente werden nie negativ.
    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl From<Duration> for DurationSpec {
    fn from(d: Duration) -> Self {
        Self(d)
    }
}

impl From<DurationSpec> for Duration {
    fn from(d: DurationSpec) -> Self {
        d.0
    }
}

/// Fehler beim Parsen einer Dauer aus Text.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseDurationError {
    #[error("leere Zeitangabe")]
    Empty,
    #[error("unbekannte Einheit `{0}` (erlaubt: h, m, s)")]
    UnknownUnit(char),
    #[error("Zahl fehlt vor Einheit `{0}`")]
    MissingValue(char),
    #[error("Zeitangabe `{0}` ist zu groß")]
    Overflow(String),
    #[error("unerwartetes Zeichen `{0}`")]
    Unexpected(char),
    #[error("Einheit `{0}` steht in falscher Reihenfolge oder doppelt")]
    OutOfOrder(char),
}

impl FromStr for DurationSpec {
    type Err = ParseDurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseDurationError::Empty);
        }
        // Bloße Zahl bedeutet Minuten — das ist die Einheit, in der Eltern denken.
        if let Ok(mins) = s.parse::<u64>() {
            return Ok(Self::from_mins(mins));
        }

        let mut total: u64 = 0;
        let mut value: Option<u64> = None;
        // Einheiten müssen absteigend sein; verhindert `1m1h` und `1h1h`.
        let mut last_rank = 0u8;

        for ch in s.chars() {
            if ch.is_ascii_digit() {
                let digit = u64::from(ch as u8 - b'0');
                value = Some(
                    value
                        .unwrap_or(0)
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(digit))
                        .ok_or_else(|| ParseDurationError::Overflow(s.to_owned()))?,
                );
                continue;
            }
            if ch.is_whitespace() {
                continue;
            }
            let (rank, factor) = match ch {
                'h' => (3u8, 3600u64),
                'm' => (2, 60),
                's' => (1, 1),
                'd' | 'w' | 'y' => return Err(ParseDurationError::UnknownUnit(ch)),
                c if c.is_alphabetic() => return Err(ParseDurationError::UnknownUnit(c)),
                c => return Err(ParseDurationError::Unexpected(c)),
            };
            if last_rank != 0 && rank >= last_rank {
                return Err(ParseDurationError::OutOfOrder(ch));
            }
            last_rank = rank;
            let v = value.take().ok_or(ParseDurationError::MissingValue(ch))?;
            total = v
                .checked_mul(factor)
                .and_then(|x| total.checked_add(x))
                .ok_or_else(|| ParseDurationError::Overflow(s.to_owned()))?;
        }

        if value.is_some() {
            // Nachlaufende Ziffern ohne Einheit, z. B. `1h30`.
            return Err(ParseDurationError::Unexpected('0'));
        }
        Ok(Self::from_secs(total))
    }
}

impl fmt::Display for DurationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.as_secs();
        if secs == 0 {
            return f.write_str("0s");
        }
        let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        if h > 0 {
            write!(f, "{h}h")?;
        }
        if m > 0 {
            write!(f, "{m}m")?;
        }
        if s > 0 {
            write!(f, "{s}s")?;
        }
        Ok(())
    }
}

impl Serialize for DurationSpec {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = DurationSpec;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("Dauer als String (`1h30m`) oder Minuten als Zahl")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                v.parse().map_err(de::Error::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(DurationSpec::from_mins(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v)
                    .map(DurationSpec::from_mins)
                    .map_err(|_| de::Error::custom("negative Dauer"))
            }
        }
        de.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parst_gaengige_schreibweisen() {
        assert_eq!(
            "2h".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_hours(2)
        );
        assert_eq!(
            "90m".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_mins(90)
        );
        assert_eq!(
            "1h30m".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_mins(90)
        );
        assert_eq!(
            "45s".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_secs(45)
        );
        assert_eq!(
            "1h30m15s".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_secs(5415)
        );
        assert_eq!("0s".parse::<DurationSpec>().unwrap(), DurationSpec::ZERO);
    }

    #[test]
    fn blosse_zahl_bedeutet_minuten() {
        assert_eq!(
            "30".parse::<DurationSpec>().unwrap(),
            DurationSpec::from_mins(30)
        );
    }

    #[test]
    fn lehnt_unsinn_ab() {
        use ParseDurationError as E;
        assert_eq!("".parse::<DurationSpec>(), Err(E::Empty));
        assert_eq!("2d".parse::<DurationSpec>(), Err(E::UnknownUnit('d')));
        assert_eq!("h".parse::<DurationSpec>(), Err(E::MissingValue('h')));
        // Reihenfolge und Duplikate: `30m1h` und `1h1h` sind Tippfehler, keine Summen.
        assert_eq!("30m1h".parse::<DurationSpec>(), Err(E::OutOfOrder('h')));
        assert_eq!("1h1h".parse::<DurationSpec>(), Err(E::OutOfOrder('h')));
        // Nachlaufende Ziffer ohne Einheit ist mehrdeutig.
        assert!("1h30".parse::<DurationSpec>().is_err());
    }

    #[test]
    fn display_ist_round_trip_faehig() {
        for secs in [0u64, 1, 59, 60, 61, 3599, 3600, 5415, 86_400] {
            let d = DurationSpec::from_secs(secs);
            assert_eq!(
                d.to_string().parse::<DurationSpec>().unwrap(),
                d,
                "bei {secs}s"
            );
        }
    }

    #[test]
    fn minuten_werden_aufgerundet() {
        assert_eq!(DurationSpec::from_secs(0).as_mins_ceil(), 0);
        assert_eq!(DurationSpec::from_secs(1).as_mins_ceil(), 1);
        assert_eq!(DurationSpec::from_secs(60).as_mins_ceil(), 1);
        assert_eq!(DurationSpec::from_secs(61).as_mins_ceil(), 2);
    }

    #[test]
    fn serde_nutzt_string_form() {
        let json = serde_json::to_string(&DurationSpec::from_mins(90)).unwrap();
        assert_eq!(json, r#""1h30m""#);
        let back: DurationSpec = serde_json::from_str(r#""1h30m""#).unwrap();
        assert_eq!(back, DurationSpec::from_mins(90));
        // Zahlen werden als Minuten akzeptiert, damit die PWA es einfach hat.
        let from_num: DurationSpec = serde_json::from_str("45").unwrap();
        assert_eq!(from_num, DurationSpec::from_mins(45));
    }
}
