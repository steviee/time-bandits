// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `tbctl doctor` — is this installation actually going to do anything?
//!
//! A half-configured setup is the worst outcome here, because it looks like a
//! working one. The daemon runs, the package is installed, the config file is
//! in place — and nothing is enforced because a PAM line is missing or every
//! policy is still in observe-only mode. Every check below exists because it
//! is a way to be quietly ineffective.

use std::fmt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use tb_core::policy::Policy;

use crate::pamconf::{PamDir, ServiceState};

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Fine.
    Ok,
    /// Works, but not the way the administrator probably intends.
    Warn,
    /// Enforcement is not happening.
    Fail,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok  ",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        })
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub level: Level,
    pub detail: String,
}

impl Check {
    fn new(name: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            level,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  [{}] {:<22} {}", self.level, self.name, self.detail)
    }
}

/// Where to look. Injectable so the tests never inspect the real system.
#[derive(Debug, Clone)]
pub struct Environment {
    pub pam: PamDir,
    pub socket: PathBuf,
    pub database: PathBuf,
    pub disable_flag: PathBuf,
    /// The group a user must be in before anything is enforced against them.
    pub managed_group: String,
    /// Directories that might hold the PAM module, in the order to try them.
    pub module_dirs: Vec<PathBuf>,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            pam: PamDir::default(),
            socket: PathBuf::from(tb_proto::pam::SOCKET_PATH),
            database: PathBuf::from("/var/lib/timebandits/state.db"),
            disable_flag: PathBuf::from("/etc/timebandits/disable"),
            managed_group: "kids".to_owned(),
            // Every distribution puts it somewhere else; see packaging/README.md.
            module_dirs: vec![
                PathBuf::from("/usr/lib64/security"),
                PathBuf::from("/usr/lib/security"),
                PathBuf::from("/usr/lib/x86_64-linux-gnu/security"),
                PathBuf::from("/usr/lib/aarch64-linux-gnu/security"),
            ],
        }
    }
}

/// Runs every check. Policies come from the caller, which already has the
/// database open.
#[must_use]
pub fn run(env: &Environment, policies: &[Policy]) -> Vec<Check> {
    run_with(env, policies, &tb_daemon::users::SystemGroups)
}

/// As [`run`], with the group lookup injected so the tests never consult the
/// machine's own passwd database.
#[must_use]
pub fn run_with(
    env: &Environment,
    policies: &[Policy],
    groups: &dyn tb_daemon::users::Membership,
) -> Vec<Check> {
    let mut checks = vec![
        check_disable_flag(&env.disable_flag),
        check_module(&env.module_dirs),
        check_daemon(&env.socket),
        check_database(&env.database),
    ];
    checks.extend(check_pam(&env.pam));
    checks.extend(check_policies(policies, &env.managed_group, groups));
    checks
}

/// The highest severity found, for the process exit code.
#[must_use]
pub fn worst(checks: &[Check]) -> Level {
    checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok)
}

fn check_disable_flag(flag: &Path) -> Check {
    if flag.exists() {
        Check::new(
            "emergency override",
            Level::Warn,
            format!("{} exists — nothing is being enforced", flag.display()),
        )
    } else {
        Check::new("emergency override", Level::Ok, "not set")
    }
}

fn check_module(dirs: &[PathBuf]) -> Check {
    match dirs
        .iter()
        .map(|d| d.join("pam_timebandits.so"))
        .find(|p| p.exists())
    {
        Some(found) => Check::new("PAM module", Level::Ok, found.display().to_string()),
        None => Check::new(
            "PAM module",
            Level::Fail,
            "pam_timebandits.so not found in any known directory",
        ),
    }
}

fn check_daemon(socket: &Path) -> Check {
    if !socket.exists() {
        return Check::new(
            "daemon",
            Level::Fail,
            format!("{} is missing — is timebanditsd running?", socket.display()),
        );
    }
    // Existing is not the same as listening: a socket left behind by a killed
    // daemon looks identical until something tries to connect.
    match UnixStream::connect(socket) {
        Ok(_) => Check::new("daemon", Level::Ok, "running and answering"),
        Err(e) => Check::new(
            "daemon",
            Level::Fail,
            format!("socket exists but will not accept a connection: {e}"),
        ),
    }
}

fn check_database(db: &Path) -> Check {
    if db.exists() {
        Check::new("database", Level::Ok, db.display().to_string())
    } else {
        Check::new(
            "database",
            Level::Warn,
            format!("{} does not exist yet", db.display()),
        )
    }
}

fn check_pam(pam: &PamDir) -> Vec<Check> {
    let Ok(states) = pam.status() else {
        return vec![Check::new(
            "PAM configuration",
            Level::Fail,
            "cannot read /etc/pam.d",
        )];
    };

    let mut checks: Vec<Check> = states
        .iter()
        .map(|(spec, state)| {
            let name = format!("pam.d/{}", spec.service);
            match state {
                ServiceState::Configured => Check::new(&name, Level::Ok, spec.why),
                ServiceState::Absent => Check::new(&name, Level::Ok, "service not installed here"),
                ServiceState::NotConfigured => {
                    Check::new(&name, Level::Fail, format!("not configured — {}", spec.why))
                }
            }
        })
        .collect();

    // The lock screen is the one that matters most. Without it a child simply
    // unlocks again with their own password, and every other measure is
    // decoration.
    if states
        .iter()
        .any(|(spec, state)| spec.service == "kde" && *state == ServiceState::NotConfigured)
    {
        checks.push(Check::new(
            "lock screen",
            Level::Fail,
            "without pam.d/kde a locked session can be unlocked again by the child",
        ));
    }
    checks
}

fn check_policies(
    policies: &[Policy],
    managed_group: &str,
    groups: &dyn tb_daemon::users::Membership,
) -> Vec<Check> {
    if policies.is_empty() {
        return vec![Check::new(
            "policies",
            Level::Warn,
            "no users are managed on this machine",
        )];
    }

    let mut checks = Vec::new();
    for p in policies {
        let name = format!("policy/{}", p.subject);
        if let Err(e) = p.validate() {
            checks.push(Check::new(&name, Level::Fail, e.to_string()));
        } else if p.enforcement {
            // A policy is not permission. Without the group, the daemon
            // records and nothing else — and this is the first place anybody
            // looks when "it isn't doing anything".
            match groups.is_member(&p.subject, managed_group) {
                Some(true) => checks.push(Check::new(&name, Level::Ok, "enforcing")),
                Some(false) => checks.push(Check::new(
                    &name,
                    Level::Fail,
                    format!(
                        "set to enforce, but `{}` is not in `{managed_group}` — nothing will be \
                         limited. Run: usermod -aG {managed_group} {}",
                        p.subject, p.subject
                    ),
                )),
                None => checks.push(Check::new(
                    &name,
                    Level::Warn,
                    format!(
                        "cannot tell whether `{}` is in `{managed_group}`",
                        p.subject
                    ),
                )),
            }
        } else {
            checks.push(Check::new(
                &name,
                Level::Warn,
                "observe only — usage is recorded but nothing is limited",
            ));
        }
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env_in(dir: &Path) -> Environment {
        let pam_root = dir.join("pam.d");
        fs::create_dir_all(&pam_root).unwrap();
        Environment {
            managed_group: "kids".to_owned(),
            pam: PamDir::new(&pam_root),
            socket: dir.join("pam.sock"),
            database: dir.join("state.db"),
            disable_flag: dir.join("disable"),
            module_dirs: vec![dir.join("security")],
        }
    }

    /// A fixed answer to "is this user managed?", so the tests never touch the
    /// machine's own passwd database.
    #[derive(Debug)]
    struct Groups(Option<bool>);
    impl tb_daemon::users::Membership for Groups {
        fn is_member(&self, _user: &str, _group: &str) -> Option<bool> {
            self.0
        }
    }
    const IN_KIDS: Groups = Groups(Some(true));

    fn enforcing(subject: &str) -> Policy {
        let mut p = Policy::permissive(subject);
        p.enforcement = true;
        p
    }

    #[test]
    fn a_bare_system_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let checks = run(&env_in(dir.path()), &[]);
        assert_eq!(worst(&checks), Level::Fail);
        assert!(
            checks
                .iter()
                .any(|c| c.name == "PAM module" && c.level == Level::Fail)
        );
        assert!(
            checks
                .iter()
                .any(|c| c.name == "daemon" && c.level == Level::Fail)
        );
    }

    #[test]
    fn a_socket_nobody_listens_on_is_not_mistaken_for_a_running_daemon() {
        // A killed daemon leaves the socket file behind. Checking existence
        // alone would report a healthy system that enforces nothing.
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        fs::write(&env.socket, b"not a socket").unwrap();

        let check = check_daemon(&env.socket);
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains("will not accept"), "{}", check.detail);
    }

    #[test]
    fn a_listening_socket_passes() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        let _listener = std::os::unix::net::UnixListener::bind(&env.socket).unwrap();
        assert_eq!(check_daemon(&env.socket).level, Level::Ok);
    }

    #[test]
    fn the_emergency_override_is_reported_not_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        fs::write(&env.disable_flag, b"").unwrap();
        let check = check_disable_flag(&env.disable_flag);
        assert_eq!(check.level, Level::Warn);
        assert!(check.detail.contains("nothing is being enforced"));
    }

    #[test]
    fn a_missing_lock_screen_rule_is_called_out_separately() {
        // It is the difference between a limit and a suggestion.
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        fs::write(env.pam.service_path("kde"), "auth include system-auth\n").unwrap();

        let checks = check_pam(&env.pam);
        assert!(
            checks
                .iter()
                .any(|c| c.name == "pam.d/kde" && c.level == Level::Fail)
        );
        let extra = checks
            .iter()
            .find(|c| c.name == "lock screen")
            .expect("callout");
        assert!(extra.detail.contains("unlocked again by the child"));
    }

    #[test]
    fn a_configured_lock_screen_produces_no_callout() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        fs::write(env.pam.service_path("kde"), "auth include system-auth\n").unwrap();
        env.pam.enable(false).unwrap();

        let checks = check_pam(&env.pam);
        assert!(!checks.iter().any(|c| c.name == "lock screen"));
        assert!(
            checks
                .iter()
                .any(|c| c.name == "pam.d/kde" && c.level == Level::Ok)
        );
    }

    #[test]
    fn observe_only_policies_are_a_warning_not_a_pass() {
        // The most likely way to believe you are protected while you are not.
        let checks = check_policies(&[Policy::permissive("kid")], "kids", &IN_KIDS);
        assert_eq!(checks[0].level, Level::Warn);
        assert!(checks[0].detail.contains("nothing is limited"));
    }

    #[test]
    fn an_enforcing_policy_passes_and_a_broken_one_fails() {
        let mut broken = enforcing("sibling");
        broken.timezone = "Mars/Olympus_Mons".to_owned();
        let checks = check_policies(&[enforcing("kid"), broken], "kids", &IN_KIDS);
        assert_eq!(checks[0].level, Level::Ok);
        assert_eq!(checks[1].level, Level::Fail);
        assert!(checks[1].detail.contains("Mars"));
    }

    #[test]
    fn no_managed_users_is_worth_saying() {
        let checks = check_policies(&[], "kids", &IN_KIDS);
        assert_eq!(checks[0].level, Level::Warn);
        assert!(checks[0].detail.contains("no users are managed"));
    }

    #[test]
    fn a_fully_set_up_system_reports_clean() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_in(dir.path());
        fs::create_dir_all(dir.path().join("security")).unwrap();
        fs::write(dir.path().join("security/pam_timebandits.so"), b"").unwrap();
        fs::write(&env.database, b"").unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&env.socket).unwrap();
        for service in ["kde", "sddm", "login"] {
            fs::write(
                env.pam.service_path(service),
                "auth include system-auth\naccount include system-auth\n",
            )
            .unwrap();
        }
        env.pam.enable(false).unwrap();

        let checks = run_with(&env, &[enforcing("kid")], &IN_KIDS);
        let bad: Vec<&Check> = checks.iter().filter(|c| c.level != Level::Ok).collect();
        assert!(bad.is_empty(), "unexpected findings: {bad:?}");
        assert_eq!(worst(&checks), Level::Ok);
    }

    #[test]
    fn an_enforcing_policy_without_the_group_is_the_headline_failure() {
        // The most common "why is it not doing anything": the rules are set,
        // the daemon is fine, and the user was never put in the group. Nothing
        // else in this report would say so.
        let checks = check_policies(&[enforcing("kid")], "kids", &Groups(Some(false)));
        assert_eq!(checks[0].level, Level::Fail);
        assert!(
            checks[0].detail.contains("usermod -aG kids kid"),
            "and says how to fix it: {}",
            checks[0].detail
        );
    }

    #[test]
    fn a_broken_group_lookup_is_a_warning_not_a_verdict() {
        let checks = check_policies(&[enforcing("kid")], "kids", &Groups(None));
        assert_eq!(checks[0].level, Level::Warn);
    }
}
