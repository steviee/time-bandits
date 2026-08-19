// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Minimal bindings to Linux-PAM.
//!
//! Hand-written rather than taken from a binding crate: this is the only unsafe
//! code in the project, it is tiny, and it sits in the login path — being able
//! to read all of it in one screen is worth more than the convenience.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

/// Opaque PAM handle.
#[repr(C)]
#[derive(Debug)]
pub struct PamHandle {
    _private: [u8; 0],
}

// Return codes, from <security/_pam_types.h>.
pub const PAM_SUCCESS: c_int = 0;
pub const PAM_PERM_DENIED: c_int = 6;
pub const PAM_AUTH_ERR: c_int = 7;
pub const PAM_IGNORE: c_int = 25;

// Item types.
pub const PAM_SERVICE: c_int = 1;
pub const PAM_CONV: c_int = 5;

// Message styles.
pub const PAM_ERROR_MSG: c_int = 3;

/// Longest message PAM accepts; anything beyond is truncated by us rather than
/// risking whatever the conversation function does with an oversized string.
pub const PAM_MAX_MSG_SIZE: usize = 512;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<ConvFn>,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_get_user(pamh: *mut PamHandle, user: *mut *const c_char, prompt: *const c_char)
    -> c_int;
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
}

/// The user PAM is currently working on.
///
/// # Safety
/// `pamh` must be the handle PAM passed into the module entry point.
pub unsafe fn get_user(pamh: *mut PamHandle) -> Option<String> {
    let mut raw: *const c_char = ptr::null();
    // SAFETY: `pamh` is valid per the contract above; `raw` is a valid out-param.
    let rc = unsafe { pam_get_user(pamh, &raw mut raw, ptr::null()) };
    if rc != PAM_SUCCESS || raw.is_null() {
        return None;
    }
    // SAFETY: PAM guarantees a NUL-terminated string owned by PAM.
    unsafe { CStr::from_ptr(raw) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// The PAM service name (`sddm`, `kde`, `login`, …).
///
/// # Safety
/// `pamh` must be the handle PAM passed into the module entry point.
pub unsafe fn get_service(pamh: *mut PamHandle) -> Option<String> {
    let mut item: *const c_void = ptr::null();
    // SAFETY: as above.
    let rc = unsafe { pam_get_item(pamh, PAM_SERVICE, &raw mut item) };
    if rc != PAM_SUCCESS || item.is_null() {
        return None;
    }
    // SAFETY: PAM_SERVICE is documented to be a NUL-terminated string.
    unsafe { CStr::from_ptr(item.cast::<c_char>()) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Shows an error message on whatever front end is driving this PAM stack —
/// the SDDM greeter, the lock screen, or a text console.
///
/// Failures are swallowed on purpose. Not being able to display *why* access was
/// refused is unfortunate; refusing to refuse because the message did not render
/// would be a bug.
///
/// # Safety
/// `pamh` must be the handle PAM passed into the module entry point.
pub unsafe fn show_error(pamh: *mut PamHandle, text: &str) {
    let mut item: *const c_void = ptr::null();
    // SAFETY: valid handle, valid out-param.
    if unsafe { pam_get_item(pamh, PAM_CONV, &raw mut item) } != PAM_SUCCESS || item.is_null() {
        return;
    }
    // SAFETY: PAM_CONV yields a `struct pam_conv` owned by PAM.
    let conv = unsafe { &*item.cast::<PamConv>() };
    let Some(conv_fn) = conv.conv else { return };

    // Truncate on a character boundary so the CString conversion cannot split
    // a multi-byte character.
    let mut truncated = text;
    while truncated.len() >= PAM_MAX_MSG_SIZE {
        truncated = &truncated[..truncated
            .char_indices()
            .rev()
            .find(|(i, _)| *i < PAM_MAX_MSG_SIZE - 1)
            .map_or(0, |(i, _)| i)];
    }
    let Ok(c_text) = CString::new(truncated) else {
        return; // Interior NUL — never from our own messages, but be defensive.
    };

    let message = PamMessage {
        msg_style: PAM_ERROR_MSG,
        msg: c_text.as_ptr(),
    };
    let mut msg_ptr: *const PamMessage = &raw const message;
    let mut resp: *mut PamResponse = ptr::null_mut();

    // SAFETY: one message, pointers valid for the duration of the call.
    let rc = unsafe { conv_fn(1, &raw mut msg_ptr, &raw mut resp, conv.appdata_ptr) };

    // The conversation may allocate a response even for an error message; PAM's
    // contract puts freeing it on the caller.
    if !resp.is_null() {
        // SAFETY: `resp` was allocated by the conversation with malloc.
        unsafe {
            if !(*resp).resp.is_null() {
                libc::free((*resp).resp.cast::<c_void>());
            }
            libc::free(resp.cast::<c_void>());
        }
    }
    let _ = rc;
}

/// Is this user `root`?
///
/// Resolved through NSS rather than by comparing the name, so an aliased root
/// account is still recognised.
#[must_use]
pub fn is_root(user: &str) -> bool {
    let Ok(c_user) = CString::new(user) else {
        return false;
    };
    // SAFETY: `getpwnam` takes a NUL-terminated string and returns a pointer to
    // static storage or NULL. We only read `pw_uid` before any further NSS call.
    let uid = unsafe {
        let pw = libc::getpwnam(c_user.as_ptr());
        if pw.is_null() {
            return false;
        }
        (*pw).pw_uid
    };
    uid == 0
}

/// Is `user` a member of `group`, counting the primary group?
///
/// Returns `None` when the lookup itself fails — the caller must not read that
/// as "no". A failed NSS lookup and a definite non-membership call for
/// different decisions.
#[must_use]
pub fn user_in_group(user: &str, group: &str) -> Option<bool> {
    let c_user = CString::new(user).ok()?;
    let c_group = CString::new(group).ok()?;

    // SAFETY: both pointers are NUL-terminated; returned pointers are read
    // immediately and not retained across further NSS calls.
    let target_gid = unsafe {
        let gr = libc::getgrnam(c_group.as_ptr());
        if gr.is_null() {
            // The group does not exist. That is a definite answer, not a failure.
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

    // `getgrouplist` reports how many groups it needs when the buffer is too
    // small, so ask twice rather than guessing a size.
    let mut ngroups: c_int = 32;
    let mut groups: Vec<libc::gid_t> = vec![0; usize::try_from(ngroups).unwrap_or(32)];
    // SAFETY: buffer and count agree; the call only writes within `ngroups`.
    let rc = unsafe {
        libc::getgrouplist(
            c_user.as_ptr(),
            primary_gid,
            groups.as_mut_ptr(),
            &raw mut ngroups,
        )
    };
    if rc < 0 {
        if ngroups <= 0 || ngroups > 65_536 {
            return None;
        }
        groups = vec![0; usize::try_from(ngroups).unwrap_or(0)];
        // SAFETY: buffer resized to the size the previous call asked for.
        let rc = unsafe {
            libc::getgrouplist(
                c_user.as_ptr(),
                primary_gid,
                groups.as_mut_ptr(),
                &raw mut ngroups,
            )
        };
        if rc < 0 {
            return None;
        }
    }
    groups.truncate(usize::try_from(ngroups).unwrap_or(0));
    Some(groups.contains(&target_gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_recognised() {
        assert!(is_root("root"));
    }

    #[test]
    fn a_nonexistent_user_is_not_root() {
        assert!(!is_root("tb-definitely-no-such-user"));
    }

    #[test]
    fn a_nonexistent_group_is_a_definite_no() {
        // Not `None`: the lookup succeeded, the group simply is not there.
        assert_eq!(
            user_in_group("root", "tb-definitely-no-such-group"),
            Some(false)
        );
    }

    #[test]
    fn a_nonexistent_user_yields_no_answer() {
        assert_eq!(user_in_group("tb-definitely-no-such-user", "root"), None);
    }

    #[test]
    fn membership_via_the_primary_group_counts() {
        // root's primary group is root (gid 0) on every distribution we target.
        assert_eq!(user_in_group("root", "root"), Some(true));
    }
}
