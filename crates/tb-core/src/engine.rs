// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Die Entscheidungs-Engine: Policy + bisherige Nutzung + Uhrzeit → Urteil.
//!
//! Bewusst eine reine Funktion ohne Uhr, Dateisystem oder Netz. Genau dieselbe
//! Funktion beantwortet drei Fragen, die sonst auseinanderlaufen würden:
//!
//! * Daemon, jede Sekunde: „muss ich jetzt sperren?“
//! * PAM-Modul, beim Anmelden: „darf sich dieser Benutzer anmelden?“
//! * Plasmoid/PWA: „wieviel Zeit ist noch übrig?“

use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{Span, Zoned};
use serde::{Deserialize, Serialize};

use crate::duration::DurationSpec;
use crate::policy::{Policy, Quota};
use crate::schedule::{Day, PolicyDay, TimeWindow, policy_day, policy_day_end};

/// Die Nutzungsdaten, gegen die eine Policy ausgewertet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Angerechnete Zeit im laufenden Policy-Tag.
    pub used_today: DurationSpec,
    /// Angerechnete Zeit in der laufenden Policy-Woche (Montag–Sonntag).
    pub used_this_week: DurationSpec,
    /// Für heute gewährte Bonuszeit. Zählt nur auf das Tageskontingent.
    pub bonus_today: DurationSpec,
}

/// Welche Regel die Restzeit gerade begrenzt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitedBy {
    /// Nichts begrenzt — Beobachtungsmodus oder ausschließlich unbegrenzte Kontingente.
    Nothing,
    DailyQuota,
    WeeklyQuota,
    /// Das laufende Zeitfenster endet vor dem Kontingent.
    Window,
}

/// Warum eine Anmeldung oder Sitzung verweigert wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    DailyQuotaExhausted,
    WeeklyQuotaExhausted,
    OutsideAllowedWindow,
}

impl DenyReason {
    /// Kurzschlüssel für Übersetzungen. Der angezeigte Text wird in der jeweiligen
    /// Oberfläche lokalisiert; das PAM-Modul hat eine eigene, knappe Fassung.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::DailyQuotaExhausted => "deny.daily_quota",
            Self::WeeklyQuotaExhausted => "deny.weekly_quota",
            Self::OutsideAllowedWindow => "deny.outside_window",
        }
    }
}

/// Das Ergebnis einer Auswertung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed(Allowance),
    Denied(Denial),
}

/// Nutzung ist erlaubt — mit dieser Restzeit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowance {
    /// Kleinste Restzeit über alle greifenden Regeln. `None` = unbegrenzt.
    pub remaining: Option<DurationSpec>,
    pub limited_by: LimitedBy,
    /// Zeitpunkt, zu dem die Restzeit ausläuft. `None` = unbegrenzt.
    pub expires_at: Option<Zoned>,
    /// Nächste Warnschwelle, die noch aussteht.
    pub next_warning: Option<DurationSpec>,
}

/// Nutzung ist gesperrt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub reason: DenyReason,
    /// Wann es wieder erlaubt ist. `None`, wenn das nicht bestimmbar ist.
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

    /// Restzeit, oder `ZERO` bei Sperre.
    #[must_use]
    pub fn remaining(&self) -> Option<DurationSpec> {
        match self {
            Self::Allowed(a) => a.remaining,
            Self::Denied(_) => Some(DurationSpec::ZERO),
        }
    }
}

/// Ein Zeitfenster als konkretes Intervall auf der Zeitachse.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Interval {
    start: Zoned,
    end: Zoned,
}

/// Rechnet die Wanduhr-Fenster eines Policy-Tages in echte Zeitpunkte um.
///
/// Ein Fenster, dessen Startzeit vor `day_start` liegt, gehört zur *zweiten*
/// Hälfte des Policy-Tages und damit auf den Folgetag im Kalender. Dadurch
/// funktionieren Fenster über Mitternacht (22:00–01:00) ohne Sonderfall.
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
            // Fenster über Mitternacht: das Ende liegt einen Tag später.
            if end <= start {
                end = end.checked_add(Span::new().days(1)).ok()?;
            }
            Some(Interval { start, end })
        })
        .collect();

    out.sort_by(|a, b| a.start.cmp(&b.start));

    // Aneinandergrenzende oder überlappende Fenster verschmelzen, sonst würde
    // bei 15:00–17:00 plus 17:00–19:00 um 17:00 fälschlich „Fenster zu Ende“
    // gemeldet und die Sitzung gesperrt.
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

/// Sammelt Fenster über mehrere Policy-Tage, um „wann wieder?“ beantworten zu können.
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

/// Beginn der Policy-Woche (Montag) für einen Policy-Tag.
fn week_start(pd: PolicyDay) -> civil::Date {
    let back = i32::from(pd.date.weekday().to_monday_zero_offset());
    pd.date
        .checked_sub(Span::new().days(back))
        .unwrap_or(pd.date)
}

/// Wertet eine Policy gegen Nutzung und Zeitpunkt aus.
///
/// Schlägt die Zeitzonen-Auflösung fehl, wird auf UTC zurückgefallen statt zu
/// scheitern: eine unlesbare Zeitzone darf kein Kind aussperren, und die Policy
/// wurde beim Laden ohnehin schon validiert.
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

    // --- 1. Zeitfenster (Bettzeit) -------------------------------------------
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

    // --- 2. Tageskontingent (inklusive Bonus) --------------------------------
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

    // --- 3. Wochenkontingent -------------------------------------------------
    let weekly_remaining = match policy.weekly_quota {
        Quota::Unlimited => None,
        Quota::Limited(l) => Some(l.saturating_sub(usage.used_this_week)),
    };
    if weekly_remaining == Some(DurationSpec::ZERO) {
        // Wieder frei am Montag zum Tagesbeginn der neuen Woche.
        let retry_at = week_start(pd)
            .checked_add(Span::new().days(7))
            .ok()
            .and_then(|d| d.to_datetime(policy.day_start).to_zoned(tz.clone()).ok());
        return Verdict::Denied(Denial {
            reason: DenyReason::WeeklyQuotaExhausted,
            retry_at,
        });
    }

    // --- 4. Kleinste greifende Schranke bestimmen ----------------------------
    // `duration_until` statt Span-Subtraktion: ein `Span` ist kalendarisch und
    // balanciert sich auf Stunden/Minuten auf, `get_seconds()` liefert dann nur
    // die Sekunden-Komponente — nicht die Gesamtdauer.
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

    /// Policy mit aktiver Durchsetzung, 2 h täglich, sonst keine Einschränkung.
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
    fn beobachtungsmodus_erlaubt_immer() {
        let p = Policy::permissive("kid"); // enforcement = false
        let v = evaluate(&p, &used(10_000), &at(2026, 8, 19, 3, 0));
        assert!(v.is_allowed());
        assert_eq!(v.remaining(), None, "unbegrenzt");
    }

    #[test]
    fn restzeit_ergibt_sich_aus_tageskontingent() {
        let v = evaluate(&policy_2h(), &used(90), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else {
            panic!("erwartet erlaubt")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        assert_eq!(a.limited_by, LimitedBy::DailyQuota);
    }

    #[test]
    fn aufgebrauchtes_tageskontingent_sperrt_bis_tagesbeginn() {
        let v = evaluate(&policy_2h(), &used(120), &at(2026, 8, 19, 16, 0));
        let d = v.denial().expect("erwartet gesperrt");
        assert_eq!(d.reason, DenyReason::DailyQuotaExhausted);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 20));
        assert_eq!(retry.hour(), 4);
    }

    #[test]
    fn bonus_hebt_die_sperre_sofort_auf() {
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_mins(120),
            bonus_today: DurationSpec::from_mins(30),
            ..UsageSnapshot::default()
        };
        let v = evaluate(&policy_2h(), &usage, &at(2026, 8, 19, 16, 0));
        assert_eq!(v.remaining(), Some(DurationSpec::from_mins(30)));
    }

    #[test]
    fn ausserhalb_des_zeitfensters_wird_gesperrt() {
        let mut p = policy_2h();
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(15, 0, 0, 0),
            civil::time(19, 0, 0, 0),
        )]);
        // 20:00 liegt hinter dem Fenster → gesperrt bis morgen 15:00.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 20, 0));
        let d = v.denial().expect("erwartet gesperrt");
        assert_eq!(d.reason, DenyReason::OutsideAllowedWindow);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 20));
        assert_eq!(retry.hour(), 15);
    }

    #[test]
    fn fensterende_begrenzt_die_restzeit() {
        let mut p = policy_2h();
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(15, 0, 0, 0),
            civil::time(19, 0, 0, 0),
        )]);
        // 18:30, noch 2 h Kontingent — aber das Fenster endet in 30 Minuten.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 18, 30));
        let Verdict::Allowed(a) = v else {
            panic!("erwartet erlaubt")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        assert_eq!(a.limited_by, LimitedBy::Window);
    }

    #[test]
    fn angrenzende_fenster_werden_verschmolzen() {
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        p.allowed_windows = WeekSchedule::uniform(vec![
            TimeWindow::new(civil::time(15, 0, 0, 0), civil::time(17, 0, 0, 0)),
            TimeWindow::new(civil::time(17, 0, 0, 0), civil::time(19, 0, 0, 0)),
        ]);
        // Genau an der Nahtstelle darf nicht gesperrt werden.
        let v = evaluate(&p, &used(0), &at(2026, 8, 19, 17, 0));
        let Verdict::Allowed(a) = v else {
            panic!("Naht zwischen Fenstern darf nicht sperren")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_hours(2)), "bis 19:00");
    }

    #[test]
    fn fenster_ueber_mitternacht_gilt_im_selben_policy_tag() {
        let mut p = policy_2h();
        p.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        p.allowed_windows = WeekSchedule::uniform(vec![TimeWindow::new(
            civil::time(22, 0, 0, 0),
            civil::time(1, 0, 0, 0),
        )]);
        // 00:30 am 20.08. gehört zum Policy-Tag 19.08. und liegt im Fenster.
        let v = evaluate(&p, &used(0), &at(2026, 8, 20, 0, 30));
        let Verdict::Allowed(a) = v else {
            panic!("erwartet erlaubt")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(30)));
        // 01:30 liegt dahinter.
        assert!(!evaluate(&p, &used(0), &at(2026, 8, 20, 1, 30)).is_allowed());
    }

    #[test]
    fn wochenkontingent_greift_vor_tageskontingent() {
        let mut p = policy_2h();
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        let usage = UsageSnapshot {
            used_today: DurationSpec::from_mins(30),
            used_this_week: DurationSpec::from_mins(9 * 60 + 45),
            ..UsageSnapshot::default()
        };
        let v = evaluate(&p, &usage, &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else {
            panic!("erwartet erlaubt")
        };
        assert_eq!(a.remaining, Some(DurationSpec::from_mins(15)));
        assert_eq!(a.limited_by, LimitedBy::WeeklyQuota);
    }

    #[test]
    fn aufgebrauchtes_wochenkontingent_sperrt_bis_montag() {
        let mut p = policy_2h();
        p.weekly_quota = Quota::Limited(DurationSpec::from_hours(10));
        let usage = UsageSnapshot {
            used_this_week: DurationSpec::from_hours(10),
            ..UsageSnapshot::default()
        };
        // 19.08.2026 ist ein Mittwoch → nächster Montag ist der 24.08.
        let v = evaluate(&p, &usage, &at(2026, 8, 19, 16, 0));
        let d = v.denial().expect("erwartet gesperrt");
        assert_eq!(d.reason, DenyReason::WeeklyQuotaExhausted);
        let retry = d.retry_at.as_ref().unwrap();
        assert_eq!(retry.date(), civil::date(2026, 8, 24));
        assert_eq!(retry.weekday(), civil::Weekday::Monday);
        assert_eq!(retry.hour(), 4);
    }

    #[test]
    fn naechste_warnschwelle_liegt_unter_der_restzeit() {
        // 2 h Kontingent, 1 h 50 min verbraucht → 10 min übrig, nächste Warnung bei 5 min.
        let v = evaluate(&policy_2h(), &used(110), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else { panic!() };
        assert_eq!(a.next_warning, Some(DurationSpec::from_mins(5)));
        // 25 min übrig → nächste Warnung bei 15 min.
        let v = evaluate(&policy_2h(), &used(95), &at(2026, 8, 19, 16, 0));
        let Verdict::Allowed(a) = v else { panic!() };
        assert_eq!(a.next_warning, Some(DurationSpec::from_mins(15)));
    }

    #[test]
    fn nutzung_ueber_dem_kontingent_sperrt_statt_zu_ueberlaufen() {
        // Falls der Daemon offline war und mehr Zeit gebucht wurde als erlaubt,
        // darf die sättigende Subtraktion nicht plötzlich wieder Zeit gewähren.
        let v = evaluate(&policy_2h(), &used(500), &at(2026, 8, 19, 16, 0));
        assert_eq!(
            v.denial().map(|d| d.reason),
            Some(DenyReason::DailyQuotaExhausted)
        );
    }

    #[test]
    fn tag_mit_kontingent_null_sperrt_ganztaegig() {
        let mut p = policy_2h();
        p.daily_quota
            .set(Day::Monday, Quota::Limited(DurationSpec::ZERO));
        // 24.08.2026 ist ein Montag.
        assert!(!evaluate(&p, &used(0), &at(2026, 8, 24, 10, 0)).is_allowed());
        // Dienstag wieder normal.
        assert!(evaluate(&p, &used(0), &at(2026, 8, 25, 10, 0)).is_allowed());
    }
}
