// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's internals, exposed as a library.
//!
//! Splitting the binary from a library is not ceremony: `tbctl` needs to read
//! the same database and understand the same configuration, and duplicating
//! either would be how the two drift into disagreeing about how much time a
//! child has left.

pub mod config;
pub mod pamserver;
pub mod store;
