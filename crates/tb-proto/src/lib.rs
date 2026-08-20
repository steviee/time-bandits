// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Wire formats shared between Time Bandits components.
//!
//! Kept separate from `tb-core` so the PAM module can depend on the protocol
//! without pulling in the whole domain model and its time-zone database.

pub mod agent;
pub mod pam;
pub mod text;
