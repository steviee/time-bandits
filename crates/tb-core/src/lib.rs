// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain model shared by every Time Bandits component.
//!
//! This crate deliberately contains no I/O: no clock, no filesystem, no D-Bus.
//! Everything here is a pure function or a plain data type, which is what makes
//! the enforcement rules testable without a running desktop session.

pub mod appid;
pub mod duration;
pub mod engine;
pub mod policy;
pub mod schedule;
pub mod usage;

pub use appid::{AppId, AppIdSource, AppObservation};
pub use duration::DurationSpec;
pub use engine::{Allowance, Denial, DenyReason, LimitedBy, UsageSnapshot, Verdict, evaluate};
pub use policy::{LockAction, Policy, PolicyError, Quota, TamperResponse};
pub use schedule::{Day, PolicyDay, TimeWindow, WeekSchedule};
pub use usage::{ClosedSegment, SegmentBuilder, SegmentConfig, Tick, UsageSegment};
