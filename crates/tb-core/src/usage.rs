// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Aus einzelnen Beobachtungen werden Nutzungs-Segmente.
//!
//! Der Daemon beobachtet im Sekundentakt, speichert aber nicht im Sekundentakt.
//! Aufeinanderfolgende Ticks derselben Anwendung werden zu einem Segment
//! zusammengefasst — das ist die Einheit, die in der Datenbank landet und zum
//! Hub synchronisiert wird.
//!
//! Zwei Fälle machen den Unterschied zwischen richtiger und plausibel falscher
//! Zeiterfassung aus, und beide werden hier behandelt:
//!
//! * **Lücken**: Nach Suspend, Kernel-Panik oder einem angehaltenen Daemon
//!   liegen zwischen zwei Ticks Stunden. Diese Zeit war keine Nutzung.
//! * **Uhr-Sprünge**: Läuft die Wanduhr rückwärts (NTP-Korrektur), darf daraus
//!   kein negatives oder überlanges Segment entstehen.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::appid::{AppId, AppIdSource, AppObservation};
use crate::duration::DurationSpec;

/// Ein abgeschlossener Nutzungsabschnitt.
///
/// Die `id` wird beim Erzeugen vergeben und nie geändert. Sie macht die
/// Synchronisation zum Hub idempotent: mehrfach übertragene Segmente
/// überschreiben sich selbst, statt sich zu addieren.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSegment {
    pub id: Uuid,
    pub subject: String,
    pub app: AppId,
    pub source: AppIdSource,
    pub start: Timestamp,
    pub end: Timestamp,
    /// Fenstertitel, nur wenn die Policy es ausdrücklich erlaubt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl UsageSegment {
    #[must_use]
    pub fn duration(&self) -> DurationSpec {
        let secs = self.start.duration_until(self.end).as_secs();
        DurationSpec::from_secs(u64::try_from(secs).unwrap_or(0))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Was der Daemon in einem Tick sieht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Benutzer ist aktiv an dieser Anwendung.
    Active {
        at: Timestamp,
        app: AppObservation,
        title: Option<String>,
    },
    /// Keine anrechenbare Nutzung: untätig, gesperrt, abgemeldet.
    ///
    /// `at` ist der Zeitpunkt, ab dem *nicht mehr* angerechnet wird — bei
    /// Untätigkeit also der Beginn der Untätigkeit, nicht der Moment ihrer
    /// Feststellung. Der Aufrufer rechnet `jetzt - idle_seconds` und begrenzt
    /// das Ergebnis nach unten auf den letzten Tick.
    Idle { at: Timestamp },
}

impl Tick {
    #[must_use]
    pub const fn at(&self) -> Timestamp {
        match self {
            Self::Active { at, .. } | Self::Idle { at } => *at,
        }
    }
}

/// Einstellungen für die Segmentbildung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentConfig {
    /// Erwarteter Abstand zwischen zwei Ticks.
    pub tick_interval: DurationSpec,
    /// Ab dieser Lücke gilt die Zwischenzeit als nicht genutzt (Suspend, Absturz).
    pub max_gap: DurationSpec,
    /// Segmente werden spätestens nach dieser Dauer geschnitten, damit auch bei
    /// einem Absturz höchstens dieser Zeitraum ungespeichert verloren geht.
    pub max_segment: DurationSpec,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            tick_interval: DurationSpec::from_secs(5),
            max_gap: DurationSpec::from_secs(30),
            max_segment: DurationSpec::from_mins(15),
        }
    }
}

/// Warum ein Segment geschlossen wurde. Rein informativ, aber in Tests und
/// Fehlersuche sehr nützlich.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Andere Anwendung im Fokus.
    AppChanged,
    /// Untätig, gesperrt oder abgemeldet.
    BecameIdle,
    /// Lücke im Tick-Strom (Suspend, angehaltener Daemon).
    Gap,
    /// Uhr ist rückwärts gesprungen.
    ClockWentBackwards,
    /// Höchstlänge erreicht, wird nahtlos fortgesetzt.
    MaxLength,
    /// Ausdrücklich abgeschlossen (Herunterfahren, Tagesgrenze).
    Flushed,
}

/// Ein geschlossenes Segment samt Grund.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedSegment {
    pub segment: UsageSegment,
    pub reason: CloseReason,
}

#[derive(Debug, Clone)]
struct OpenSegment {
    id: Uuid,
    app: AppId,
    source: AppIdSource,
    title: Option<String>,
    start: Timestamp,
    /// Letzter Tick, der zu diesem Segment gehört. Das Segment endet hier — nicht
    /// beim aktuellen Tick, sonst würde die Lücke davor mitgezählt.
    last_seen: Timestamp,
}

/// Formt aus einem Tick-Strom Segmente.
#[derive(Debug)]
pub struct SegmentBuilder {
    subject: String,
    config: SegmentConfig,
    open: Option<OpenSegment>,
}

impl SegmentBuilder {
    #[must_use]
    pub fn new(subject: impl Into<String>, config: SegmentConfig) -> Self {
        Self {
            subject: subject.into(),
            config,
            open: None,
        }
    }

    /// Läuft gerade ein Segment?
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Verarbeitet einen Tick und liefert das dabei geschlossene Segment, falls eines endet.
    pub fn observe(&mut self, tick: &Tick) -> Option<ClosedSegment> {
        let now = tick.at();

        // Uhr rückwärts: alles Offene abschließen und neu beginnen. Ein Segment
        // mit Ende vor dem Start wäre in der Datenbank dauerhaft kaputt.
        if let Some(open) = &self.open
            && now < open.last_seen
        {
            let closed = self.close(CloseReason::ClockWentBackwards, None);
            self.start_if_active(tick);
            return closed;
        }

        // Lücke: Die Zeit dazwischen war keine Nutzung, das Segment endet beim
        // letzten gesehenen Tick.
        if let Some(open) = &self.open {
            let gap = open.last_seen.duration_until(now).as_secs();
            if u64::try_from(gap).unwrap_or(0) > self.config.max_gap.as_secs() {
                let closed = self.close(CloseReason::Gap, None);
                self.start_if_active(tick);
                return closed;
            }
        }

        match tick {
            Tick::Idle { at } => self.close(CloseReason::BecameIdle, Some(*at)),
            Tick::Active { app, title, .. } => {
                let Some(open) = &mut self.open else {
                    self.start_if_active(tick);
                    return None;
                };

                if open.app != app.app {
                    let closed = self.close(CloseReason::AppChanged, Some(now));
                    self.start_if_active(tick);
                    return closed;
                }

                // Höchstlänge: schneiden und nahtlos fortsetzen.
                let len = open.start.duration_until(now).as_secs();
                if u64::try_from(len).unwrap_or(0) >= self.config.max_segment.as_secs() {
                    let closed = self.close(CloseReason::MaxLength, Some(now));
                    self.start_if_active(tick);
                    return closed;
                }

                // Dieselbe App weiter im Fokus: Segment verlängern. Eine genauere
                // Quelle (z. B. desktopFileName nach anfänglichem Scope-Treffer)
                // darf die Herkunft nachträglich aufwerten.
                if app.source > open.source {
                    open.source = app.source;
                }
                if title.is_some() {
                    open.title.clone_from(title);
                }
                open.last_seen = now;
                None
            }
        }
    }

    /// Schließt ein laufendes Segment ausdrücklich ab — beim Herunterfahren, an
    /// der Tagesgrenze oder vor dem Speichern.
    pub fn flush(&mut self, at: Timestamp) -> Option<ClosedSegment> {
        // Liegt der Abschluss weit hinter dem letzten Tick, war die Zwischenzeit
        // keine Nutzung — dann endet das Segment beim letzten Tick.
        let within_gap = self.open.as_ref().is_some_and(|open| {
            let gap = open.last_seen.duration_until(at).as_secs();
            u64::try_from(gap).unwrap_or(0) <= self.config.max_gap.as_secs()
        });
        self.close(CloseReason::Flushed, within_gap.then_some(at))
    }

    fn start_if_active(&mut self, tick: &Tick) {
        if let Tick::Active { at, app, title } = tick {
            self.open = Some(OpenSegment {
                id: Uuid::now_v7(),
                app: app.app.clone(),
                source: app.source,
                title: title.clone(),
                start: *at,
                last_seen: *at,
            });
        }
    }

    /// Schließt das offene Segment.
    ///
    /// `end_at` ist der Zeitpunkt des auslösenden Ereignisses — beim
    /// Anwendungswechsel also der Tick, an dem die *neue* App im Fokus war. Die
    /// Zeit zwischen dem letzten eigenen Tick und dem Wechsel gehört der alten
    /// App: sonst versickert bei jedem Wechsel ein Tick-Intervall und die Summe
    /// aller Segmente unterschreitet die tatsächlich verbrachte Zeit.
    ///
    /// Bei Lücken und Uhr-Sprüngen wird `None` übergeben — dort ist das
    /// Gegenteil richtig, die Zwischenzeit war nachweislich keine Nutzung.
    fn close(&mut self, reason: CloseReason, end_at: Option<Timestamp>) -> Option<ClosedSegment> {
        let open = self.open.take()?;
        let mut end = match end_at {
            Some(t) if t > open.last_seen => t,
            _ => open.last_seen,
        };
        // Ein Segment aus einem einzigen Tick, das sofort abgeschlossen wird,
        // hätte sonst die Dauer null und ginge verloren.
        if end <= open.start {
            end = open
                .start
                .checked_add(jiff::SignedDuration::from_secs(
                    i64::try_from(self.config.tick_interval.as_secs()).unwrap_or(1),
                ))
                .unwrap_or(open.start);
        }

        let segment = UsageSegment {
            id: open.id,
            subject: self.subject.clone(),
            app: open.app,
            source: open.source,
            start: open.start,
            end,
            title: open.title,
        };
        if segment.is_empty() {
            return None;
        }
        Some(ClosedSegment { segment, reason })
    }
}

/// Summiert Segmente je Anwendung, absteigend nach Dauer.
#[must_use]
pub fn totals_by_app(segments: &[UsageSegment]) -> Vec<(AppId, DurationSpec)> {
    use std::collections::HashMap;
    let mut map: HashMap<&AppId, u64> = HashMap::new();
    for s in segments {
        *map.entry(&s.app).or_default() += s.duration().as_secs();
    }
    let mut out: Vec<(AppId, DurationSpec)> = map
        .into_iter()
        .map(|(a, secs)| (a.clone(), DurationSpec::from_secs(secs)))
        .collect();
    // Nach Dauer absteigend, bei Gleichstand nach Name — sonst wackelt die
    // Reihenfolge in Berichten zwischen zwei Abrufen.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Gesamtdauer aller Segmente.
#[must_use]
pub fn total(segments: &[UsageSegment]) -> DurationSpec {
    DurationSpec::from_secs(segments.iter().map(|s| s.duration().as_secs()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(1_756_000_000 + secs).expect("gültiger Zeitstempel")
    }

    fn active(secs: i64, app: &str) -> Tick {
        Tick::Active {
            at: ts(secs),
            app: AppObservation {
                app: AppId::new(app),
                source: AppIdSource::DesktopFile,
            },
            title: None,
        }
    }

    fn builder() -> SegmentBuilder {
        SegmentBuilder::new("kid", SegmentConfig::default())
    }

    #[test]
    fn gleiche_app_wird_zu_einem_segment() {
        let mut b = builder();
        for t in [0, 5, 10, 15] {
            assert!(b.observe(&active(t, "firefox")).is_none());
        }
        let closed = b.flush(ts(20)).expect("Segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(20));
        assert_eq!(closed.segment.app.as_str(), "firefox");
    }

    #[test]
    fn anwendungswechsel_schliesst_das_segment() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        let closed = b.observe(&active(10, "konsole")).expect("Segment");
        assert_eq!(closed.reason, CloseReason::AppChanged);
        assert_eq!(closed.segment.app.as_str(), "firefox");
        // Die Zeit bis zum Wechsel gehört Firefox — sonst fehlen in der Summe
        // pro Anwendungswechsel fünf Sekunden.
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(10));
        assert!(b.is_open(), "Konsole läuft jetzt");
    }

    #[test]
    fn untaetigkeit_schliesst_das_segment() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        let closed = b.observe(&Tick::Idle { at: ts(10) }).expect("Segment");
        assert_eq!(closed.reason, CloseReason::BecameIdle);
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(10));
        assert!(!b.is_open());
        // Weitere Idle-Ticks erzeugen nichts.
        assert!(b.observe(&Tick::Idle { at: ts(15) }).is_none());
    }

    #[test]
    fn suspend_wird_nicht_als_nutzung_gezaehlt() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        // Deckel zu, vier Stunden später wieder auf.
        let closed = b.observe(&active(4 * 3600, "firefox")).expect("Segment");
        assert_eq!(closed.reason, CloseReason::Gap);
        assert_eq!(
            closed.segment.duration(),
            DurationSpec::from_secs(5),
            "nur die tatsächlich beobachteten 5 Sekunden"
        );
        // Danach läuft ein neues Segment ab dem Aufwachen.
        let next = b.flush(ts(4 * 3600 + 10)).expect("Segment");
        assert_eq!(next.segment.duration(), DurationSpec::from_secs(10));
    }

    #[test]
    fn rueckwaerts_springende_uhr_erzeugt_kein_kaputtes_segment() {
        let mut b = builder();
        b.observe(&active(1000, "firefox"));
        b.observe(&active(1005, "firefox"));
        let closed = b.observe(&active(0, "firefox")).expect("Segment");
        assert_eq!(closed.reason, CloseReason::ClockWentBackwards);
        assert!(!closed.segment.is_empty());
        assert!(closed.segment.end > closed.segment.start);
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn lange_nutzung_wird_in_stuecke_geschnitten() {
        let mut b = SegmentBuilder::new(
            "kid",
            SegmentConfig {
                max_segment: DurationSpec::from_secs(20),
                ..SegmentConfig::default()
            },
        );
        let mut closed = Vec::new();
        for t in (0..=60).step_by(5) {
            if let Some(c) = b.observe(&active(t, "firefox")) {
                closed.push(c);
            }
        }
        assert!(
            closed.len() >= 3,
            "mindestens drei Stücke, war {}",
            closed.len()
        );
        assert!(closed.iter().all(|c| c.reason == CloseReason::MaxLength));
        // Die Stücke müssen lückenlos aneinander anschließen.
        for pair in closed.windows(2) {
            assert_eq!(pair[0].segment.end, pair[1].segment.start);
        }
    }

    #[test]
    fn schneller_wechsel_verschluckt_keine_zeit() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let closed = b.observe(&active(5, "konsole")).expect("Segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn einzelner_tick_ohne_dauer_bekommt_das_tick_intervall() {
        // Fährt der Rechner unmittelbar nach dem ersten Tick herunter, hätte das
        // Segment die Dauer null und würde verworfen.
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let closed = b.flush(ts(0)).expect("Segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn genauere_quelle_wertet_das_laufende_segment_auf() {
        let mut b = builder();
        b.observe(&Tick::Active {
            at: ts(0),
            app: AppObservation {
                app: AppId::new("firefox"),
                source: AppIdSource::SystemdScope,
            },
            title: None,
        });
        b.observe(&Tick::Active {
            at: ts(5),
            app: AppObservation {
                app: AppId::new("firefox"),
                source: AppIdSource::DesktopFile,
            },
            title: None,
        });
        let closed = b.flush(ts(10)).expect("Segment");
        assert_eq!(closed.segment.source, AppIdSource::DesktopFile);
    }

    #[test]
    fn jedes_segment_bekommt_eine_eigene_id() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let a = b.observe(&active(5, "konsole")).unwrap().segment.id;
        let c = b.flush(ts(10)).unwrap().segment.id;
        assert_ne!(a, c);
    }

    #[test]
    fn summiert_je_anwendung_stabil_sortiert() {
        let mut b = builder();
        let mut segs = Vec::new();
        let mut push = |c: Option<ClosedSegment>| {
            if let Some(c) = c {
                segs.push(c.segment);
            }
        };
        push(b.observe(&active(0, "firefox")));
        push(b.observe(&active(5, "firefox")));
        push(b.observe(&active(10, "firefox")));
        push(b.observe(&active(15, "konsole"))); // firefox: 15 s
        push(b.observe(&active(20, "konsole")));
        push(b.flush(ts(25))); // konsole: 10 s

        let totals = totals_by_app(&segs);
        assert_eq!(totals[0].0.as_str(), "firefox");
        assert_eq!(totals[0].1, DurationSpec::from_secs(15));
        assert_eq!(totals[1].0.as_str(), "konsole");
        assert_eq!(totals[1].1, DurationSpec::from_secs(10));
        assert_eq!(total(&segs), DurationSpec::from_secs(25));
    }
}
