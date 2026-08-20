// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Daemon configuration, read from `/etc/timebandits/daemon.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tb_core::duration::DurationSpec;

/// Path checked before anything else. Its mere existence disables enforcement.
///
/// This is the emergency brake: it works without the daemon being reachable,
/// without D-Bus, and without a working policy — a parent with a rescue shell
/// can always get the household back to a normal machine.
pub const DISABLE_FLAG: &str = "/etc/timebandits/disable";

pub const DEFAULT_CONFIG: &str = "/etc/timebandits/daemon.toml";
pub const DEFAULT_STATE_DIR: &str = "/var/lib/timebandits";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where the usage database lives.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,

    /// Where the rules live, one TOML file per child.
    ///
    /// Separate from `state_dir` on purpose: these are configuration a parent
    /// reads and edits, so they belong under `/etc` with the rest of the
    /// machine's settings, not among the daemon's working data.
    #[serde(default = "default_policy_dir")]
    pub policy_dir: PathBuf,

    /// Unix socket the PAM module connects to. Root only.
    #[serde(default = "default_pam_socket")]
    pub pam_socket: PathBuf,

    /// Unix socket the session agents connect to. Reachable by every user;
    /// the daemon identifies callers by their peer credentials.
    #[serde(default = "default_agent_socket")]
    pub agent_socket: PathBuf,

    /// How often usage is sampled.
    #[serde(default = "default_tick")]
    pub tick_interval: DurationSpec,

    /// Household server, if any. Absent means single-machine operation.
    #[serde(default)]
    pub hub_url: Option<String>,

    /// Users to manage even without a stored policy, so a new child is tracked
    /// from first login rather than silently ignored.
    #[serde(default)]
    pub managed_group: Option<String>,
}

fn default_state_dir() -> PathBuf {
    PathBuf::from(DEFAULT_STATE_DIR)
}
fn default_policy_dir() -> PathBuf {
    PathBuf::from(crate::policystore::DEFAULT_DIR)
}
fn default_pam_socket() -> PathBuf {
    PathBuf::from(tb_proto::pam::SOCKET_PATH)
}
fn default_agent_socket() -> PathBuf {
    PathBuf::from(tb_proto::agent::SOCKET_PATH)
}
fn default_tick() -> DurationSpec {
    DurationSpec::from_secs(5)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            policy_dir: default_policy_dir(),
            pam_socket: default_pam_socket(),
            agent_socket: default_agent_socket(),
            tick_interval: default_tick(),
            hub_url: None,
            managed_group: Some("kids".to_owned()),
        }
    }
}

impl Config {
    /// Loads the configuration, falling back to defaults when the file is absent.
    ///
    /// A *malformed* file is an error rather than a silent fallback: running
    /// with defaults when an administrator believes their settings apply is how
    /// a child ends up with no limits and nobody notices.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.state_dir.join("state.db")
    }

    /// Where a user's rules are written.
    #[must_use]
    pub fn policies(&self) -> crate::policystore::PolicyStore {
        crate::policystore::PolicyStore::new(&self.policy_dir)
    }

    /// Is the emergency brake pulled?
    #[must_use]
    pub fn enforcement_disabled() -> bool {
        Path::new(DISABLE_FLAG).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_yields_defaults() {
        let cfg = Config::load(Path::new("/nonexistent/timebandits.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        std::fs::write(&path, "tick_interval = \"10s\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.tick_interval, DurationSpec::from_secs(10));
        assert_eq!(cfg.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
    }

    #[test]
    fn a_typo_is_an_error_not_a_silent_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        // `tick_intervall` with two l's: the administrator believes this applies.
        std::fs::write(&path, "tick_intervall = \"10s\"\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn the_database_lives_under_the_state_directory() {
        let cfg = Config::default();
        assert_eq!(
            cfg.database_path(),
            PathBuf::from("/var/lib/timebandits/state.db")
        );
        assert_eq!(
            cfg.policy_dir,
            PathBuf::from("/etc/timebandits/policy.d"),
            "rules are configuration and belong under /etc"
        );
    }
}
