// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The session agent's internals, exposed as a library.
//!
//! Split from the binary for the same reason as the daemon: the parts worth
//! testing should not have to be reached through `main`.

pub mod client;
pub mod dbus;
pub mod idle;
pub mod state;
