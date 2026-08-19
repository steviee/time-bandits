// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The protocol between `pam_timebandits.so` and `timebanditsd`.
//!
//! This runs in the login path, so the design is deliberately boring: one line
//! of JSON in, one line of JSON out, over a Unix stream socket. No D-Bus, no
//! async runtime, no TLS, no retries. Anything that could hang has a hard
//! timeout, and every failure has a defined answer.
//!
//! The socket lives at [`SOCKET_PATH`] and is world-connectable — the daemon
//! learns the caller's identity from the peer credentials, not from the request.

use serde::{Deserialize, Serialize};

/// Where the daemon listens for PAM queries.
pub const SOCKET_PATH: &str = "/run/timebandits/pam.sock";

/// Protocol version. Bumped only on incompatible changes; the daemon must keep
/// answering older modules, because the module and the daemon are separate
/// files that a partial package upgrade can leave out of step.
pub const VERSION: u32 = 1;

/// The maximum size of a single message. Generous for the payload, small enough
/// that a hostile or broken peer cannot make the login path allocate.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;

/// Which PAM stack is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// `pam_sm_authenticate` — used by KScreenLocker, which evaluates only the
    /// `auth` stack. This is what stops a child unlocking with their own
    /// password once their time is up.
    Auth,
    /// `pam_sm_acct_mgmt` — used by display managers and `login` to refuse a
    /// fresh session.
    Account,
}

/// A question from the PAM module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub v: u32,
    /// The user trying to authenticate.
    pub user: String,
    /// PAM service name (`sddm`, `kde`, `login`, …).
    pub service: String,
    pub phase: Phase,
}

/// Alias used by the PAM module, where `Query` alone reads ambiguously next
/// to the daemon's own request types.
pub use self::Query as ClientQuery;

impl Query {
    #[must_use]
    pub fn new(user: impl Into<String>, service: impl Into<String>, phase: Phase) -> Self {
        Self {
            v: VERSION,
            user: user.into(),
            service: service.into(),
            phase,
        }
    }
}

/// What the module should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Let the rest of the stack decide. Returned for unmanaged users.
    Ignore,
    /// The user may proceed.
    Allow,
    /// Refuse, showing `message`.
    Deny,
}

/// The daemon's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub v: u32,
    pub decision: Decision,
    /// Text shown to the person at the keyboard. Already localized by the
    /// daemon, which knows the configured locale; the module does no formatting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Seconds until access is expected back, for a "try again at" hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_secs: Option<u64>,
}

impl Answer {
    #[must_use]
    pub fn allow() -> Self {
        Self {
            v: VERSION,
            decision: Decision::Allow,
            message: None,
            retry_in_secs: None,
        }
    }

    #[must_use]
    pub fn ignore() -> Self {
        Self {
            v: VERSION,
            decision: Decision::Ignore,
            message: None,
            retry_in_secs: None,
        }
    }

    #[must_use]
    pub fn deny(message: impl Into<String>) -> Self {
        Self {
            v: VERSION,
            decision: Decision::Deny,
            message: Some(message.into()),
            retry_in_secs: None,
        }
    }

    #[must_use]
    pub fn with_retry_in(mut self, secs: u64) -> Self {
        self.retry_in_secs = Some(secs);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_is_one_line_of_json() {
        let q = Query::new("kid", "kde", Phase::Auth);
        let line = serde_json::to_string(&q).unwrap();
        assert!(!line.contains('\n'), "must stay on one line");
        assert_eq!(
            line,
            r#"{"v":1,"user":"kid","service":"kde","phase":"auth"}"#
        );
    }

    #[test]
    fn an_answer_round_trips() {
        let a = Answer::deny("Screen time is used up until 07:00.").with_retry_in(3600);
        let line = serde_json::to_string(&a).unwrap();
        let back: Answer = serde_json::from_str(&line).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.decision, Decision::Deny);
    }

    #[test]
    fn optional_fields_stay_out_of_the_wire_format() {
        let line = serde_json::to_string(&Answer::allow()).unwrap();
        assert_eq!(line, r#"{"v":1,"decision":"allow"}"#);
    }

    #[test]
    fn an_answer_from_a_newer_daemon_still_parses() {
        // A daemon that learned new optional fields must not break an older
        // module, which is what a partial package upgrade produces.
        let line = r#"{"v":1,"decision":"deny","message":"nope","grace_hint":42}"#;
        let a: Answer = serde_json::from_str(line).expect("unknown fields are ignored");
        assert_eq!(a.decision, Decision::Deny);
    }
}
