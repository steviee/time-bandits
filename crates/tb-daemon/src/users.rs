// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Who the daemon is allowed to enforce against.
//!
//! A policy is not permission. Enforcement additionally requires the user to be
//! in the managed group, because the two are set by different people at
//! different times: a policy can arrive from a hub, be restored from a backup,
//! or be created by a mistyped `tbctl` command, while group membership is a
//! deliberate act by whoever administers the machine.
//!
//! Without this, one wrong policy locks out an adult who was never meant to be
//! managed. That is not hypothetical — it is how this module came to exist.

use std::ffi::CString;

/// Whether a user is subject to enforcement.
///
/// A trait so the tick loop can be tested without a real passwd database, and
/// so a household with a different group name only configures it once.
pub trait Membership: std::fmt::Debug {
    /// `None` when the lookup itself failed, which is not the same as "no".
    fn is_member(&self, user: &str, group: &str) -> Option<bool>;
}

/// The real answer, from NSS.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGroups;

impl Membership for SystemGroups {
    fn is_member(&self, user: &str, group: &str) -> Option<bool> {
        user_in_group(user, group)
    }
}

/// Is `user` in `group`, counting the primary group?
///
/// Returns `None` when the lookup fails, so a caller can tell a broken NSS
/// apart from a definite no. The two deserve different answers: a definite no
/// means do not enforce, while a failure means something is wrong that a person
/// should hear about.
#[allow(unsafe_code)]
#[must_use]
pub fn user_in_group(user: &str, group: &str) -> Option<bool> {
    let c_user = CString::new(user).ok()?;
    let c_group = CString::new(group).ok()?;

    // SAFETY: both strings are NUL-terminated; the returned pointers are read
    // immediately and not held across further NSS calls.
    let target_gid = unsafe {
        let gr = libc::getgrnam(c_group.as_ptr());
        if gr.is_null() {
            // The group does not exist. A definite answer: nobody is in it.
            return Some(false);
        }
        (*gr).gr_gid
    };

    // SAFETY: as above.
    let primary_gid = unsafe {
        let pw = libc::getpwnam(c_user.as_ptr());
        if pw.is_null() {
            return None;
        }
        (*pw).pw_gid
    };
    if primary_gid == target_gid {
        return Some(true);
    }

    let mut count: libc::c_int = 32;
    let mut groups: Vec<libc::gid_t> = vec![0; 32];
    // SAFETY: buffer and count agree; the call writes only within `count`.
    let rc = unsafe {
        libc::getgrouplist(
            c_user.as_ptr(),
            primary_gid,
            groups.as_mut_ptr(),
            &raw mut count,
        )
    };
    if rc < 0 {
        // The buffer was too small; the call reported how much it needs.
        let needed = usize::try_from(count).ok().filter(|n| *n <= 65_536)?;
        groups = vec![0; needed];
        // SAFETY: buffer resized to what the previous call asked for.
        let rc = unsafe {
            libc::getgrouplist(
                c_user.as_ptr(),
                primary_gid,
                groups.as_mut_ptr(),
                &raw mut count,
            )
        };
        if rc < 0 {
            return None;
        }
    }
    groups.truncate(usize::try_from(count).unwrap_or(0));
    Some(groups.contains(&target_gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonexistent_group_is_a_definite_no() {
        assert_eq!(
            user_in_group("root", "tb-definitely-no-such-group"),
            Some(false)
        );
    }

    #[test]
    fn a_nonexistent_user_yields_no_answer() {
        // Not `false`: the caller must be able to tell this apart from a user
        // who exists and simply is not managed.
        assert_eq!(user_in_group("tb-definitely-no-such-user", "root"), None);
    }

    #[test]
    fn membership_via_the_primary_group_counts() {
        assert_eq!(user_in_group("root", "root"), Some(true));
    }
}
