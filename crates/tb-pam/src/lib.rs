// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `pam_timebandits.so` — refuses logins and screen unlocks once a child's
//! screen time is used up.
//!
//! # Where this belongs in `/etc/pam.d`
//!
//! ```text
//! # /etc/pam.d/kde  — the lock screen. KScreenLocker evaluates only `auth`,
//! # which is precisely why an expired quota can be enforced here at all.
//! auth     requisite   pam_timebandits.so
//!
//! # /etc/pam.d/sddm, /etc/pam.d/login — a fresh session.
//! account  required    pam_timebandits.so
//! ```
//!
//! # Two properties worth stating outright
//!
//! **The module can only ever refuse.** There is no code path that returns
//! `PAM_SUCCESS`; a positive answer becomes `PAM_IGNORE` and the real
//! authentication modules do their job. Even listed as `sufficient` by mistake,
//! it cannot become a way past the password prompt.
//!
//! **A bug here must not lock the household out.** Every entry point catches
//! panics and returns `PAM_IGNORE`. That is a deliberate asymmetry: an
//! unreachable daemon is an adversarial condition and fails *closed* for
//! managed users, while a crash in our own code is our fault and fails *open*.

#![allow(unsafe_code)]
// `argc` and `argv` are fixed by the PAM module ABI; renaming them to satisfy
// the similar-names lint would make the entry points harder to recognise.
#![allow(clippy::similar_names)]

mod client;
mod decide;
mod ffi;

use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

use tb_proto::pam::{Answer, ClientQuery, Phase};

use client::ClientError;
use decide::{Config, Environment, Outcome, decide};
use ffi::PamHandle;

/// The only three values this module may hand back to libpam.
///
/// Making the permitted results an enum is what actually enforces the "can only
/// refuse" property: `PAM_SUCCESS` has no variant, so no future edit to an entry
/// point can accidentally introduce a way past the password check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleResult {
    /// Stay out of it; the rest of the stack decides.
    Ignore,
    /// Refusal in the `auth` stack, where it must look like a failed unlock.
    AuthError,
    /// Refusal in the `account` stack.
    PermissionDenied,
}

impl ModuleResult {
    const fn code(self) -> c_int {
        match self {
            Self::Ignore => ffi::PAM_IGNORE,
            Self::AuthError => ffi::PAM_AUTH_ERR,
            Self::PermissionDenied => ffi::PAM_PERM_DENIED,
        }
    }
}

/// The production implementation of [`Environment`].
struct RealEnvironment<'a> {
    cfg: &'a Config,
}

impl Environment for RealEnvironment<'_> {
    fn is_root(&self, user: &str) -> bool {
        ffi::is_root(user)
    }

    fn user_in_group(&self, user: &str, group: &str) -> Option<bool> {
        ffi::user_in_group(user, group)
    }

    fn ask(&self, query: &ClientQuery) -> Result<Answer, ClientError> {
        client::ask(&self.cfg.socket, query, self.cfg.timeout)
    }

    fn log(&self, message: &str) {
        if self.cfg.debug {
            syslog(message);
        }
    }
}

/// Writes to the system log. Best effort — logging must never affect the outcome.
fn syslog(message: &str) {
    const LOG_AUTHPRIV: c_int = 10 << 3;
    const LOG_WARNING: c_int = 4;
    let Ok(msg) = CString::new(message) else {
        return;
    };
    let Ok(fmt) = CString::new("pam_timebandits: %s") else {
        return;
    };
    // SAFETY: both pointers are NUL-terminated and outlive the call; the format
    // string has exactly one `%s` matching the single argument.
    unsafe {
        libc::syslog(LOG_AUTHPRIV | LOG_WARNING, fmt.as_ptr(), msg.as_ptr());
    }
}

/// Collects module arguments from the `/etc/pam.d` line.
///
/// # Safety
/// `argv` must point to `argc` valid NUL-terminated strings, as PAM guarantees.
unsafe fn collect_args(argc: c_int, argv: *const *const c_char) -> Vec<String> {
    if argv.is_null() || argc <= 0 {
        return Vec::new();
    }
    (0..argc as isize)
        .filter_map(|i| {
            // SAFETY: index is within `argc`, per the contract above.
            let p = unsafe { *argv.offset(i) };
            if p.is_null() {
                return None;
            }
            // SAFETY: PAM hands out NUL-terminated strings.
            unsafe { CStr::from_ptr(p) }
                .to_str()
                .ok()
                .map(str::to_owned)
        })
        .collect()
}

/// The body shared by both entry points.
///
/// # Safety
/// `pamh` and `argv` must be what PAM passed in.
unsafe fn run(
    pamh: *mut PamHandle,
    argc: c_int,
    argv: *const *const c_char,
    phase: Phase,
) -> ModuleResult {
    let args = unsafe { collect_args(argc, argv) };
    let cfg = Config::from_args(args.iter().map(String::as_str));

    let Some(user) = (unsafe { ffi::get_user(pamh) }) else {
        // Without a user there is nothing to decide about.
        return ModuleResult::Ignore;
    };
    let service = unsafe { ffi::get_service(pamh) }.unwrap_or_else(|| "unknown".to_owned());

    let env = RealEnvironment { cfg: &cfg };
    match decide(&cfg, &env, &user, &service, phase) {
        Outcome::Ignore => ModuleResult::Ignore,
        Outcome::Deny(message) => {
            unsafe { ffi::show_error(pamh, &message) };
            syslog(&format!("denied {user} on service {service}: {message}"));
            match phase {
                // In the auth stack a refusal has to look like a failed
                // authentication, or KScreenLocker will not treat it as final.
                Phase::Auth => ModuleResult::AuthError,
                Phase::Account => ModuleResult::PermissionDenied,
            }
        }
    }
}

/// Wraps an entry point so no panic can reach libpam.
fn guard(f: impl FnOnce() -> ModuleResult) -> c_int {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| {
            syslog("internal error (panic); ignoring this module");
            ModuleResult::Ignore
        })
        .code()
}

/// `auth` stack — used by KScreenLocker to authorise an unlock.
///
/// # Safety
/// Called by libpam with a valid handle and argument vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    guard(|| unsafe { run(pamh, argc, argv, Phase::Auth) })
}

/// Required whenever a module provides `auth`. We hold no credentials.
///
/// # Safety
/// Called by libpam.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    ModuleResult::Ignore.code()
}

/// `account` stack — used by display managers and `login` before a new session.
///
/// # Safety
/// Called by libpam with a valid handle and argument vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_acct_mgmt(
    pamh: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    guard(|| unsafe { run(pamh, argc, argv, Phase::Account) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_turns_a_panic_into_ignore() {
        assert_eq!(guard(|| panic!("boom")), ffi::PAM_IGNORE);
    }

    #[test]
    fn the_guard_passes_normal_results_through() {
        assert_eq!(guard(|| ModuleResult::AuthError), ffi::PAM_AUTH_ERR);
    }

    #[test]
    fn setcred_never_grants_anything() {
        let rc = unsafe { pam_sm_setcred(std::ptr::null_mut(), 0, 0, std::ptr::null()) };
        assert_eq!(rc, ffi::PAM_IGNORE);
    }

    #[test]
    fn no_result_the_module_can_produce_means_success() {
        // The property the module's safety rests on. The destructuring below
        // is exhaustive on purpose: adding a fourth variant fails to compile
        // here rather than silently weakening the guarantee.
        let all = [
            ModuleResult::Ignore,
            ModuleResult::AuthError,
            ModuleResult::PermissionDenied,
        ];
        for result in all {
            match result {
                ModuleResult::Ignore | ModuleResult::AuthError | ModuleResult::PermissionDenied => {
                }
            }
            assert_ne!(
                result.code(),
                ffi::PAM_SUCCESS,
                "{result:?} must not mean success"
            );
        }
    }
}
