// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the rules live: one TOML file per child under `/etc/timebandits/policy.d/`.
//!
//! Rules are configuration and usage is data, so they are kept apart. A parent
//! can read a rule with `cat` and change it with any editor; the file survives
//! in a backup as something a person can still understand years later. Usage —
//! append-heavy, queried by time range, growing without bound — stays in
//! SQLite, where that shape belongs.
//!
//! There is no cache. A policy file is under a kilobyte and the tick loop reads
//! it every few seconds, which costs nothing and removes an entire class of
//! staleness bug: an edit takes effect on the next tick, with no watcher, no
//! reload command, and no way for the daemon to act on a rule that is no longer
//! written down.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tb_core::policy::Policy;

/// Where policies live by default.
pub const DEFAULT_DIR: &str = "/etc/timebandits/policy.d";

#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a usable policy: {message}")]
    Malformed { path: PathBuf, message: String },
    #[error("`{0}` is not a usable file name for a policy")]
    UnusableSubject(String),
}

/// Policies as files.
#[derive(Debug, Clone)]
pub struct PolicyStore {
    dir: PathBuf,
}

impl PolicyStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a subject's policy lives in.
    ///
    /// Rejects anything that could escape the directory or collide with a
    /// different name. A user name comes from the passwd database and from
    /// command lines, and one of those is typed by a human.
    pub fn path_for(&self, subject: &str) -> Result<PathBuf, PolicyStoreError> {
        let usable = !subject.is_empty()
            && subject.len() <= 64
            && subject
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !subject.starts_with('.');
        if !usable {
            return Err(PolicyStoreError::UnusableSubject(subject.to_owned()));
        }
        Ok(self.dir.join(format!("{subject}.toml")))
    }

    /// Reads one policy, or `None` when the user is not managed here.
    ///
    /// A malformed file is an error rather than a silent absence. Both mean
    /// "no rules apply", but only one of them should be quiet about it — a
    /// typo that switches enforcement off without saying so is exactly the
    /// failure this project exists to avoid.
    pub fn load(&self, subject: &str) -> Result<Option<Policy>, PolicyStoreError> {
        let path = self.path_for(subject)?;
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PolicyStoreError::Read { path, source }),
        };
        let policy: Policy = toml::from_str(&text).map_err(|e| PolicyStoreError::Malformed {
            path: path.clone(),
            message: e.to_string(),
        })?;
        // A file naming a different subject is a copy somebody forgot to edit.
        // Applying it under the file's name would enforce one child's rules on
        // another.
        if policy.subject != subject {
            return Err(PolicyStoreError::Malformed {
                path,
                message: format!(
                    "the file is named for `{subject}` but describes `{}`",
                    policy.subject
                ),
            });
        }
        policy.validate().map_err(|e| PolicyStoreError::Malformed {
            path,
            message: e.to_string(),
        })?;
        Ok(Some(policy))
    }

    /// Writes a policy, replacing any previous one.
    ///
    /// Validated first and written atomically: a half-written policy file is
    /// not a corrupt config, it is a child whose limits silently stopped
    /// applying.
    pub fn save(&self, policy: &Policy) -> Result<PathBuf, PolicyStoreError> {
        policy.validate().map_err(|e| PolicyStoreError::Malformed {
            path: self.dir.join("<new>"),
            message: e.to_string(),
        })?;
        let path = self.path_for(&policy.subject)?;

        fs::create_dir_all(&self.dir).map_err(|source| PolicyStoreError::Write {
            path: self.dir.clone(),
            source,
        })?;

        let body = toml::to_string_pretty(policy).map_err(|e| PolicyStoreError::Malformed {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let text = format!(
            "# Time Bandits rules for {}.\n\
             #\n\
             # Edit freely — the daemon re-reads this file within a few seconds, and\n\
             # `tbctl policy show {}` prints what it made of it. A file it cannot\n\
             # parse is refused rather than ignored, so a typo cannot quietly switch\n\
             # enforcement off.\n\n{body}",
            policy.subject, policy.subject
        );
        write_atomically(&path, &text)?;
        Ok(path)
    }

    /// Every user with rules here.
    pub fn subjects(&self) -> Result<Vec<String>, PolicyStoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            // No directory means nobody is managed yet, which is a normal
            // state on a fresh installation.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(PolicyStoreError::Read {
                    path: self.dir.clone(),
                    source,
                });
            }
        };
        let mut subjects: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".toml").map(str::to_owned)
            })
            .filter(|s| !s.is_empty() && !s.starts_with('.'))
            .collect();
        subjects.sort_unstable();
        Ok(subjects)
    }

    /// Removes a user's rules. Returns whether there was anything to remove.
    pub fn remove(&self, subject: &str) -> Result<bool, PolicyStoreError> {
        let path = self.path_for(subject)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(PolicyStoreError::Write { path, source }),
        }
    }
}

/// Writes via a temporary file and a rename, so a reader sees either the old
/// policy or the new one and never half of either.
fn write_atomically(path: &Path, content: &str) -> Result<(), PolicyStoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tb-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let fail = |source| PolicyStoreError::Write {
        path: path.to_path_buf(),
        source,
    };

    {
        let mut file = fs::File::create(&tmp).map_err(fail)?;
        file.write_all(content.as_bytes()).map_err(fail)?;
        file.sync_all().map_err(fail)?;
    }
    // Readable by everyone: the child's own agent has no business reading it,
    // but tbctl run by a parent does, and 0644 is what /etc expects.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
    }
    fs::rename(&tmp, path).map_err(fail)?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_core::duration::DurationSpec;
    use tb_core::policy::Quota;
    use tb_core::schedule::WeekSchedule;

    fn store() -> (tempfile::TempDir, PolicyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = PolicyStore::new(dir.path().join("policy.d"));
        (dir, store)
    }

    fn policy(subject: &str) -> Policy {
        let mut p = Policy::permissive(subject);
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    #[test]
    fn a_policy_survives_a_round_trip() {
        let (_d, s) = store();
        let p = policy("alice");
        s.save(&p).unwrap();
        assert_eq!(s.load("alice").unwrap(), Some(p));
    }

    #[test]
    fn an_unmanaged_user_is_absent_not_an_error() {
        let (_d, s) = store();
        assert_eq!(s.load("nobody").unwrap(), None);
        assert!(s.subjects().unwrap().is_empty());
    }

    #[test]
    fn the_file_is_something_a_parent_can_read() {
        // The whole reason for choosing files over a blob in a database.
        let (_d, s) = store();
        let path = s.save(&policy("alice")).unwrap();
        let text = fs::read_to_string(&path).unwrap();

        assert!(path.ends_with("alice.toml"), "{path:?}");
        assert!(text.contains("subject = \"alice\""), "{text}");
        assert!(text.contains("2h"), "durations stay readable: {text}");
        assert!(
            text.starts_with("# Time Bandits rules for alice."),
            "{text}"
        );
        assert!(
            text.contains("re-reads this file"),
            "says how to use it: {text}"
        );
    }

    #[test]
    fn an_edit_is_picked_up_without_anything_being_reloaded() {
        // No cache, so the next read is the current file. A parent editing by
        // hand should not have to know a restart command exists.
        let (_d, s) = store();
        s.save(&policy("alice")).unwrap();
        let path = s.path_for("alice").unwrap();

        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("\"2h\"", "\"45m\"");
        fs::write(&path, text).unwrap();

        let reloaded = s.load("alice").unwrap().unwrap();
        assert_eq!(
            *reloaded.daily_quota.get(tb_core::Day::Monday),
            Quota::Limited(DurationSpec::from_mins(45))
        );
    }

    #[test]
    fn a_malformed_file_is_refused_rather_than_ignored() {
        // Both outcomes mean no rules apply, but only one of them is quiet
        // about it, and a typo that silently switches enforcement off is the
        // failure this project exists to avoid.
        let (_d, s) = store();
        s.save(&policy("alice")).unwrap();
        fs::write(s.path_for("alice").unwrap(), "this is not toml {{{").unwrap();

        let err = s.load("alice").unwrap_err();
        assert!(matches!(err, PolicyStoreError::Malformed { .. }), "{err}");
    }

    #[test]
    fn a_file_naming_someone_else_is_refused() {
        // A copied file somebody forgot to edit would otherwise enforce one
        // child's rules on another.
        let (_d, s) = store();
        s.save(&policy("alice")).unwrap();
        let text = fs::read_to_string(s.path_for("alice").unwrap()).unwrap();
        fs::write(s.path_for("bob").unwrap(), text).unwrap();

        let err = s.load("bob").unwrap_err();
        assert!(err.to_string().contains("describes `alice`"), "{err}");
    }

    #[test]
    fn an_invalid_policy_is_refused_on_the_way_in_and_out() {
        let (_d, s) = store();
        let mut broken = policy("alice");
        broken.timezone = "Mars/Olympus_Mons".to_owned();
        assert!(s.save(&broken).is_err(), "not written");
        assert_eq!(s.load("alice").unwrap(), None, "nothing was written");
    }

    #[test]
    fn a_user_name_cannot_escape_the_directory() {
        let (_d, s) = store();
        for bad in ["../etc/passwd", "a/b", "", ".hidden", "with space", "a\0b"] {
            assert!(s.path_for(bad).is_err(), "accepted {bad:?}");
        }
        assert!(s.path_for("alice").is_ok());
        assert!(s.path_for("alice-2").is_ok());
    }

    #[test]
    fn subjects_lists_every_managed_user_in_order() {
        let (_d, s) = store();
        for name in ["chloe", "alice", "ben"] {
            s.save(&policy(name)).unwrap();
        }
        // Something that is not a policy must not become a phantom child.
        fs::write(s.dir().join("notes.txt"), "ignore me").unwrap();
        assert_eq!(s.subjects().unwrap(), ["alice", "ben", "chloe"]);
    }

    #[test]
    fn removing_rules_is_reported_honestly() {
        let (_d, s) = store();
        s.save(&policy("alice")).unwrap();
        assert!(s.remove("alice").unwrap());
        assert!(!s.remove("alice").unwrap(), "already gone");
        assert_eq!(s.load("alice").unwrap(), None);
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        let (_d, s) = store();
        s.save(&policy("alice")).unwrap();
        let strays: Vec<String> = fs::read_dir(s.dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tb-tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }
}
