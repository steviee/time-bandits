// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turns individual observations into usage segments.
//!
//! The daemon observes once per tick but does not store once per tick.
//! Consecutive ticks of the same application collapse into one segment — the
//! unit that lands in the database and syncs to the hub.
//!
//! Two cases separate correct accounting from plausible-looking nonsense, and
//! both are handled here:
//!
//! * **Gaps**: after suspend, a kernel panic or a stopped daemon, hours pass
//!   between two ticks. That time was not usage.
//! * **Clock jumps**: if the wall clock runs backwards (an NTP correction), it
//!   must not produce a negative or absurdly long segment.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::appid::{AppId, AppIdSource, AppObservation};
use crate::duration::DurationSpec;

/// One finished stretch of usage.
///
/// The `id` is assigned on creation and never changes. It makes syncing to the
/// hub idempotent: a segment delivered twice overwrites itself instead of being
/// counted twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSegment {
    pub id: Uuid,
    pub subject: String,
    pub app: AppId,
    pub source: AppIdSource,
    pub start: Timestamp,
    pub end: Timestamp,
    /// Window title, only when the policy explicitly allows recording it.
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

/// What the daemon sees in one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// The user is actively working in this application.
    Active {
        at: Timestamp,
        app: AppObservation,
        title: Option<String>,
    },
    /// Nothing creditable: idle, locked, or logged out.
    ///
    /// `at` is the moment crediting *stops* — for inactivity that is when the
    /// inactivity began, not when it was noticed. The caller computes
    /// `now - idle_seconds` and clamps the result to the last tick.
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

/// Knobs for segment building.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentConfig {
    /// Expected spacing between two ticks.
    pub tick_interval: DurationSpec,
    /// Beyond this gap the intervening time counts as unused (suspend, crash).
    pub max_gap: DurationSpec,
    /// Segments are cut at this length, bounding how much is lost if the daemon
    /// dies before writing.
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

/// Why a segment was closed. Informational, but invaluable in tests and when
/// diagnosing odd reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A different application took focus.
    AppChanged,
    /// Idle, locked or logged out.
    BecameIdle,
    /// A hole in the tick stream (suspend, stopped daemon).
    Gap,
    /// The clock jumped backwards.
    ClockWentBackwards,
    /// Maximum length reached; continues seamlessly.
    MaxLength,
    /// Closed explicitly (shutdown, day boundary).
    Flushed,
}

/// A closed segment together with the reason.
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
    /// The last tick belonging to this segment.
    last_seen: Timestamp,
}

/// Shapes a tick stream into segments.
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

    /// Is a segment currently open?
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Processes one tick and returns the segment it closed, if any.
    pub fn observe(&mut self, tick: &Tick) -> Option<ClosedSegment> {
        let now = tick.at();

        // Clock ran backwards: close what is open and start over. A segment
        // ending before it starts would be permanently broken in the database.
        if let Some(open) = &self.open
            && now < open.last_seen
        {
            let closed = self.close(CloseReason::ClockWentBackwards, None);
            self.start_if_active(tick);
            return closed;
        }

        // Gap: the time in between was not usage, so the segment ends at the
        // last tick actually observed.
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

                // Maximum length: cut here and continue seamlessly.
                let len = open.start.duration_until(now).as_secs();
                if u64::try_from(len).unwrap_or(0) >= self.config.max_segment.as_secs() {
                    let closed = self.close(CloseReason::MaxLength, Some(now));
                    self.start_if_active(tick);
                    return closed;
                }

                // Same application still focused: extend the segment. A more
                // precise source (desktopFileName arriving after an initial
                // scope match) may upgrade the recorded provenance.
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

    /// Closes an open segment explicitly — on shutdown, at the day boundary, or
    /// before persisting.
    pub fn flush(&mut self, at: Timestamp) -> Option<ClosedSegment> {
        // If the flush happens long after the last tick, that time was not
        // usage, and the segment ends at the last tick instead.
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

    /// Closes the open segment.
    ///
    /// `end_at` is the instant of the triggering event — for an application
    /// switch, the tick at which the *new* application held focus. The time
    /// between the last own tick and the switch belongs to the old application:
    /// otherwise one tick interval evaporates on every switch and the sum of all
    /// segments falls short of the time actually spent.
    ///
    /// For gaps and clock jumps `None` is passed — there the opposite is true,
    /// the intervening time demonstrably was not usage.
    fn close(&mut self, reason: CloseReason, end_at: Option<Timestamp>) -> Option<ClosedSegment> {
        let open = self.open.take()?;
        let mut end = match end_at {
            Some(t) if t > open.last_seen => t,
            _ => open.last_seen,
        };
        // A single-tick segment closed immediately would have zero duration and
        // be dropped.
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

/// Sums segments per application, longest first.
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
    // Longest first, ties broken by name — otherwise report ordering wobbles
    // between two refreshes.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Total duration across all segments.
#[must_use]
pub fn total(segments: &[UsageSegment]) -> DurationSpec {
    DurationSpec::from_secs(segments.iter().map(|s| s.duration().as_secs()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(1_756_000_000 + secs).expect("valid timestamp")
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
    fn one_application_collapses_into_one_segment() {
        let mut b = builder();
        for t in [0, 5, 10, 15] {
            assert!(b.observe(&active(t, "firefox")).is_none());
        }
        let closed = b.flush(ts(20)).expect("a segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(20));
        assert_eq!(closed.segment.app.as_str(), "firefox");
    }

    #[test]
    fn switching_application_closes_the_segment() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        let closed = b.observe(&active(10, "konsole")).expect("a segment");
        assert_eq!(closed.reason, CloseReason::AppChanged);
        assert_eq!(closed.segment.app.as_str(), "firefox");
        // The time up to the switch belongs to Firefox — otherwise five seconds
        // go missing on every switch.
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(10));
        assert!(b.is_open(), "konsole is running now");
    }

    #[test]
    fn going_idle_closes_the_segment() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        let closed = b.observe(&Tick::Idle { at: ts(10) }).expect("a segment");
        assert_eq!(closed.reason, CloseReason::BecameIdle);
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(10));
        assert!(!b.is_open());
        // Further idle ticks produce nothing.
        assert!(b.observe(&Tick::Idle { at: ts(15) }).is_none());
    }

    #[test]
    fn suspend_is_not_counted_as_usage() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        b.observe(&active(5, "firefox"));
        // Lid closed, reopened four hours later.
        let closed = b.observe(&active(4 * 3600, "firefox")).expect("a segment");
        assert_eq!(closed.reason, CloseReason::Gap);
        assert_eq!(
            closed.segment.duration(),
            DurationSpec::from_secs(5),
            "only the five seconds actually observed"
        );
        // A fresh segment then runs from the moment of waking.
        let next = b.flush(ts(4 * 3600 + 10)).expect("a segment");
        assert_eq!(next.segment.duration(), DurationSpec::from_secs(10));
    }

    #[test]
    fn a_backwards_clock_jump_produces_no_broken_segment() {
        let mut b = builder();
        b.observe(&active(1000, "firefox"));
        b.observe(&active(1005, "firefox"));
        let closed = b.observe(&active(0, "firefox")).expect("a segment");
        assert_eq!(closed.reason, CloseReason::ClockWentBackwards);
        assert!(!closed.segment.is_empty());
        assert!(closed.segment.end > closed.segment.start);
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn long_usage_is_cut_into_pieces() {
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
            "at least three pieces, got {}",
            closed.len()
        );
        assert!(closed.iter().all(|c| c.reason == CloseReason::MaxLength));
        // The pieces must join without a hole.
        for pair in closed.windows(2) {
            assert_eq!(pair[0].segment.end, pair[1].segment.start);
        }
    }

    #[test]
    fn a_quick_switch_loses_no_time() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let closed = b.observe(&active(5, "konsole")).expect("a segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn a_single_tick_with_no_duration_is_credited_one_interval() {
        // If the machine shuts down right after the first tick, the segment
        // would otherwise have zero duration and be discarded.
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let closed = b.flush(ts(0)).expect("a segment");
        assert_eq!(closed.segment.duration(), DurationSpec::from_secs(5));
    }

    #[test]
    fn a_more_precise_source_upgrades_the_open_segment() {
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
        let closed = b.flush(ts(10)).expect("a segment");
        assert_eq!(closed.segment.source, AppIdSource::DesktopFile);
    }

    #[test]
    fn every_segment_gets_its_own_id() {
        let mut b = builder();
        b.observe(&active(0, "firefox"));
        let a = b.observe(&active(5, "konsole")).unwrap().segment.id;
        let c = b.flush(ts(10)).unwrap().segment.id;
        assert_ne!(a, c);
    }

    #[test]
    fn totals_per_application_sort_stably() {
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
