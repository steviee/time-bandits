// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Module configuration and the decision itself.
//!
//! Kept free of PAM and socket types so the rules that decide whether a family
//! gets locked out can be tested exhaustively without a login stack.

use std::path::PathBuf;
use std::time::Duration;

use tb_proto::pam::{Answer, ClientQuery, Decision, Phase};

use crate::client::ClientError;

/// What to do when the daemon cannot be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fallback {
    /// Refuse for managed users. The default: a child who found a way to stop
    /// the daemon must not be rewarded with unlimited time.
    #[default]
    Deny,
    /// Allow. For households that would rather risk extra screen time than a
    /// machine nobody can log into.
    Allow,
}

/// Options given on the `pam_timebandits.so` line in `/etc/pam.d/…`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub socket: PathBuf,
    pub timeout: Duration,
    pub fallback: Fallback,
    /// Users in this group are subject to the fallback. Everyone else is ignored.
    pub managed_group: String,
    /// Users in this group are never touched, whatever the daemon says.
    pub exempt_group: String,
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: PathBuf::from(tb_proto::pam::SOCKET_PATH),
            timeout: Duration::from_millis(300),
            fallback: Fallback::Deny,
            managed_group: "kids".to_owned(),
            exempt_group: "parents".to_owned(),
            debug: false,
        }
    }
}

impl Config {
    /// Parses module arguments. Unknown or malformed options are ignored rather
    /// than fatal — a typo in `/etc/pam.d/sddm` must not break logging in.
    #[must_use]
    pub fn from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Self {
        let mut cfg = Self::default();
        for arg in args {
            let (key, value) = arg.split_once('=').unwrap_or((arg, ""));
            match key {
                "socket" if !value.is_empty() => cfg.socket = PathBuf::from(value),
                "timeout_ms" => {
                    if let Ok(ms) = value.parse::<u64>() {
                        // Clamped: below 50 ms even a healthy daemon loses the
                        // race, above 5 s the greeter looks frozen.
                        cfg.timeout = Duration::from_millis(ms.clamp(50, 5_000));
                    }
                }
                "fallback" => {
                    cfg.fallback = match value {
                        "allow" => Fallback::Allow,
                        _ => Fallback::Deny,
                    }
                }
                "managed_group" if !value.is_empty() => value.clone_into(&mut cfg.managed_group),
                "exempt_group" if !value.is_empty() => value.clone_into(&mut cfg.exempt_group),
                "debug" => cfg.debug = true,
                _ => {}
            }
        }
        cfg
    }
}

/// What the module tells PAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Stay out of it — the rest of the stack decides.
    Ignore,
    /// Refuse, showing this message.
    Deny(String),
}

/// Everything the decision needs from the outside world.
///
/// A trait so the tests can drive every branch, including the ones that only
/// occur when NSS or the daemon misbehaves.
pub trait Environment {
    fn is_root(&self, user: &str) -> bool;
    /// `None` means the lookup failed, which is not the same as "no".
    fn user_in_group(&self, user: &str, group: &str) -> Option<bool>;
    fn ask(&self, query: &ClientQuery) -> Result<Answer, ClientError>;
    fn log(&self, _message: &str) {}
}

/// The decision.
///
/// Note what is missing: there is no outcome that *grants* access. The module is
/// a veto and nothing else, so no misconfiguration of its control flag in
/// `/etc/pam.d` can turn it into a way around the password check.
pub fn decide(
    cfg: &Config,
    env: &dyn Environment,
    user: &str,
    service: &str,
    phase: Phase,
) -> Outcome {
    // 1. root, always and before anything else. A daemon bug, a corrupt policy
    //    or a broken socket must never cost the administrator their machine.
    if env.is_root(user) {
        return Outcome::Ignore;
    }

    // 2. Parents likewise. A failed lookup counts as exempt here: locking a
    //    parent out because NSS hiccuped is the worse error by far.
    if env.user_in_group(user, &cfg.exempt_group) != Some(false) {
        return Outcome::Ignore;
    }

    // 3. Ask the daemon.
    let query = ClientQuery::new(user, service, phase);
    match env.ask(&query) {
        Ok(answer) => match answer.decision {
            Decision::Allow | Decision::Ignore => Outcome::Ignore,
            Decision::Deny => Outcome::Deny(
                answer
                    .message
                    .unwrap_or_else(|| "Screen time limit reached.".to_owned()),
            ),
        },
        Err(err) => {
            env.log(&format!("{err}; applying fallback"));
            // 4. Fallback. Only users who are actually managed are affected;
            //    for everyone else an unreachable daemon is none of our business.
            let managed = env.user_in_group(user, &cfg.managed_group) == Some(true);
            match (managed, cfg.fallback) {
                (true, Fallback::Deny) => Outcome::Deny(
                    "Screen time service is unavailable. Please ask a parent.".to_owned(),
                ),
                _ => Outcome::Ignore,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeEnv {
        root: Vec<String>,
        groups: Vec<(String, String)>,
        lookup_fails_for: Vec<String>,
        answer: Option<Result<Answer, ClientError>>,
        asked: RefCell<Vec<ClientQuery>>,
    }

    impl FakeEnv {
        fn kid() -> Self {
            Self {
                groups: vec![("kid".into(), "kids".into())],
                ..Self::default()
            }
        }
        fn answering(mut self, a: Result<Answer, ClientError>) -> Self {
            self.answer = Some(a);
            self
        }
    }

    impl Environment for FakeEnv {
        fn is_root(&self, user: &str) -> bool {
            self.root.iter().any(|u| u == user)
        }
        fn user_in_group(&self, user: &str, group: &str) -> Option<bool> {
            if self.lookup_fails_for.iter().any(|u| u == user) {
                return None;
            }
            Some(self.groups.iter().any(|(u, g)| u == user && g == group))
        }
        fn ask(&self, query: &ClientQuery) -> Result<Answer, ClientError> {
            self.asked.borrow_mut().push(query.clone());
            self.answer
                .clone()
                .unwrap_or(Err(ClientError::Unreachable("no stub".into())))
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn root_is_never_touched() {
        let env = FakeEnv {
            root: vec!["root".into()],
            answer: Some(Ok(Answer::deny("should not be asked"))),
            ..FakeEnv::default()
        };
        assert_eq!(
            decide(&cfg(), &env, "root", "login", Phase::Account),
            Outcome::Ignore
        );
        assert!(
            env.asked.borrow().is_empty(),
            "root must not even be looked up"
        );
    }

    #[test]
    fn parents_are_never_touched() {
        let env = FakeEnv {
            groups: vec![("mum".into(), "parents".into())],
            answer: Some(Ok(Answer::deny("should not be asked"))),
            ..FakeEnv::default()
        };
        assert_eq!(
            decide(&cfg(), &env, "mum", "sddm", Phase::Account),
            Outcome::Ignore
        );
        assert!(env.asked.borrow().is_empty());
    }

    #[test]
    fn a_failed_exemption_lookup_errs_towards_letting_people_in() {
        let env = FakeEnv {
            lookup_fails_for: vec!["mum".into()],
            answer: Some(Ok(Answer::deny("out of time"))),
            ..FakeEnv::default()
        };
        assert_eq!(
            decide(&cfg(), &env, "mum", "sddm", Phase::Account),
            Outcome::Ignore,
            "a broken NSS lookup must not lock a parent out"
        );
    }

    #[test]
    fn a_denial_from_the_daemon_is_passed_through_with_its_message() {
        let env = FakeEnv::kid().answering(Ok(Answer::deny("Time is up until 07:00.")));
        assert_eq!(
            decide(&cfg(), &env, "kid", "kde", Phase::Auth),
            Outcome::Deny("Time is up until 07:00.".to_owned())
        );
    }

    #[test]
    fn a_denial_without_a_message_still_says_something() {
        let mut answer = Answer::deny("");
        answer.message = None;
        let env = FakeEnv::kid().answering(Ok(answer));
        let Outcome::Deny(msg) = decide(&cfg(), &env, "kid", "kde", Phase::Auth) else {
            panic!("expected denial")
        };
        assert!(!msg.is_empty());
    }

    #[test]
    fn permission_is_never_granted_only_withheld() {
        // Both "allow" and "ignore" from the daemon end in Ignore, so the module
        // can never substitute for the password check.
        for decision in [Decision::Allow, Decision::Ignore] {
            let answer = Answer {
                decision,
                ..Answer::allow()
            };
            let env = FakeEnv::kid().answering(Ok(answer));
            assert_eq!(
                decide(&cfg(), &env, "kid", "kde", Phase::Auth),
                Outcome::Ignore,
                "for {decision:?}"
            );
        }
    }

    #[test]
    fn an_unreachable_daemon_blocks_a_managed_child() {
        let env = FakeEnv::kid().answering(Err(ClientError::TimedOut));
        assert!(matches!(
            decide(&cfg(), &env, "kid", "sddm", Phase::Account),
            Outcome::Deny(_)
        ));
    }

    #[test]
    fn an_unreachable_daemon_ignores_unmanaged_users() {
        // A guest account that is in neither group is none of our business.
        let env = FakeEnv::default().answering(Err(ClientError::TimedOut));
        assert_eq!(
            decide(&cfg(), &env, "guest", "sddm", Phase::Account),
            Outcome::Ignore
        );
    }

    #[test]
    fn the_fallback_can_be_switched_to_allow() {
        let env = FakeEnv::kid().answering(Err(ClientError::TimedOut));
        let cfg = Config {
            fallback: Fallback::Allow,
            ..Config::default()
        };
        assert_eq!(
            decide(&cfg, &env, "kid", "sddm", Phase::Account),
            Outcome::Ignore
        );
    }

    #[test]
    fn a_garbled_answer_falls_back_rather_than_being_believed() {
        let env = FakeEnv::kid().answering(Err(ClientError::BadAnswer("nonsense".into())));
        assert!(matches!(
            decide(&cfg(), &env, "kid", "sddm", Phase::Account),
            Outcome::Deny(_)
        ));
    }

    #[test]
    fn the_service_and_phase_reach_the_daemon() {
        let env = FakeEnv::kid().answering(Ok(Answer::allow()));
        decide(&cfg(), &env, "kid", "kde", Phase::Auth);
        let asked = env.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].service, "kde");
        assert_eq!(asked[0].phase, Phase::Auth);
        assert_eq!(asked[0].user, "kid");
    }

    #[test]
    fn arguments_are_parsed_and_clamped() {
        let c = Config::from_args([
            "socket=/tmp/x.sock",
            "timeout_ms=10",
            "fallback=allow",
            "managed_group=children",
            "debug",
        ]);
        assert_eq!(c.socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!(
            c.timeout,
            Duration::from_millis(50),
            "clamped up from 10 ms"
        );
        assert_eq!(c.fallback, Fallback::Allow);
        assert_eq!(c.managed_group, "children");
        assert!(c.debug);
        assert_eq!(c.exempt_group, "parents", "untouched options keep defaults");
    }

    #[test]
    fn nonsense_arguments_do_not_break_the_module() {
        let c = Config::from_args(["", "=", "wat", "timeout_ms=abc", "fallback=maybe"]);
        assert_eq!(
            c,
            Config {
                fallback: Fallback::Deny,
                ..Config::default()
            }
        );
    }
}
