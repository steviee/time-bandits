// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Editing `/etc/pam.d`.
//!
//! This is the most dangerous code in the project. A wrong line here does not
//! produce a bug report — it produces a family that cannot log in. Three things
//! follow from that, and they shape the whole module:
//!
//! * The text transformations are pure functions over strings, so every case
//!   can be tested without a filesystem, let alone a real `/etc`.
//! * The file operations take a root directory, so the tests that *do* touch
//!   files never go near the real one.
//! * Nothing is written without a backup, and everything is written atomically.
//!   A crash mid-write must not leave a truncated PAM stack behind.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Markers wrapping everything this tool adds. Package uninstall scriptlets
/// key on the same strings; changing them here without changing them there
/// leaves lines behind that point at a module that no longer exists.
pub const BEGIN: &str = "# >>> time-bandits >>>";
pub const END: &str = "# <<< time-bandits <<<";

/// Suffix for the copy taken before the first modification.
pub const BACKUP_SUFFIX: &str = ".timebandits-backup";

/// Which PAM stack a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    Auth,
    Account,
}

impl Stack {
    const fn keyword(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Account => "account",
        }
    }
}

impl fmt::Display for Stack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, so the generated rule lines up like the rest of the file.
        f.pad(self.keyword())
    }
}

/// One service file this tool manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSpec {
    pub service: &'static str,
    pub stack: Stack,
    pub control: &'static str,
    pub why: &'static str,
}

/// `requisite` semantics, but survivable when the module is not installed.
///
/// A plain `requisite pam_timebandits.so` locks **everybody** out the moment
/// the module goes missing — after removing the package without running
/// `tbctl pam disable`, or on an image-based system where a system extension
/// failed to merge. The leading-dash form does not help: it only silences the
/// log entry, which was measured on Fedora 44 rather than assumed.
///
/// `module_unknown=ignore` steps over a module that is not there, while
/// `default=die` still lets a module that *is* there refuse, and stops the
/// stack at once so the child sees the reason instead of being asked for a
/// password that cannot work.
const AUTH_CONTROL: &str =
    "[success=ok new_authtok_reqd=ok ignore=ignore module_unknown=ignore default=die]";

/// `required` semantics, with the same protection against a missing module.
const ACCOUNT_CONTROL: &str =
    "[success=ok new_authtok_reqd=ok ignore=ignore module_unknown=ignore default=bad]";

/// The services enforcement needs, and why each one.
pub const MANAGED: &[ServiceSpec] = &[
    ServiceSpec {
        service: "kde",
        stack: Stack::Auth,
        control: AUTH_CONTROL,
        why: "the lock screen; KScreenLocker evaluates only the auth stack",
    },
    ServiceSpec {
        service: "sddm",
        stack: Stack::Account,
        control: ACCOUNT_CONTROL,
        why: "the display manager; refuses a fresh session",
    },
    ServiceSpec {
        service: "plasmalogin",
        stack: Stack::Account,
        control: ACCOUNT_CONTROL,
        // Plasma 6.5 replaced SDDM with this, and Fedora 44 KDE and Bazzite
        // ship it. A machine covering only `sddm` covers no display manager at
        // all there: the child is locked out of their session and logs
        // straight back in.
        why: "the display manager on Plasma 6.5 and later",
    },
    ServiceSpec {
        service: "kcheckpass",
        stack: Stack::Auth,
        control: AUTH_CONTROL,
        // Present alongside `kde` on Fedora. Which one kscreenlocker consults
        // has moved between releases, and covering both costs nothing while
        // missing the live one costs everything.
        why: "the lock screen's password helper on some builds",
    },
    ServiceSpec {
        service: "login",
        stack: Stack::Account,
        control: ACCOUNT_CONTROL,
        why: "text console login",
    },
];

/// The block this tool writes for a service.
#[must_use]
pub fn block(spec: &ServiceSpec) -> String {
    format!(
        "{BEGIN}\n\
         # Managed by tbctl. Do not edit between the markers; run `tbctl pam disable`.\n\
         # {}\n\
         #\n\
         # The bracket form rather than a plain control word: `module_unknown=ignore`\n\
         # steps over the module if it is ever not installed, instead of locking\n\
         # everybody out of the machine.\n\
         {} {} pam_timebandits.so\n\
         {END}\n",
        spec.why, spec.stack, spec.control
    )
}

/// Which stack a configuration line belongs to, if any.
///
/// Handles the three spellings that occur in the wild: a plain `auth ...` rule,
/// the leading-dash form `-auth ...` that marks a module as optional to load,
/// and Debian's `@include common-auth`.
#[must_use]
pub fn line_stack(line: &str) -> Option<Stack> {
    let line = line.trim();
    for stack in [Stack::Auth, Stack::Account] {
        let kw = stack.keyword();
        if line.starts_with(kw) || line.starts_with(&format!("-{kw}")) {
            return Some(stack);
        }
        if let Some(rest) = line.strip_prefix("@include ")
            && rest.trim() == format!("common-{kw}")
        {
            return Some(stack);
        }
    }
    None
}

/// Is our block already present?
#[must_use]
pub fn has_block(content: &str) -> bool {
    content.lines().any(|l| l.trim() == BEGIN)
}

/// Removes our block, leaving everything else untouched.
///
/// Tolerates a missing end marker: a half-written file must still be
/// recoverable, and refusing to clean it up would be the worst possible time to
/// be strict.
#[must_use]
pub fn remove_block(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inside = false;
    for line in content.lines() {
        let t = line.trim();
        if t == BEGIN {
            inside = true;
            continue;
        }
        if t == END {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Where our block goes, and what to do if there is nowhere obvious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Inserted before the first line of the target stack, which is where it has
    /// to be: after another module has already authenticated, refusing is too
    /// late to be useful.
    BeforeFirstStackLine(usize),
    /// The file has no line for this stack at all. Appending is a guess, so the
    /// caller is expected to warn rather than proceed silently.
    AppendedWithNoStackPresent,
    /// The service only existed in the vendor directory, so `/etc/pam.d` would
    /// gain a copy of it first. Only reported by a dry run — a real one goes on
    /// to say where the rule landed in that copy.
    AdoptedFromVendor,
}

/// Inserts (or replaces) our block, reporting where it landed.
pub fn insert_block(content: &str, spec: &ServiceSpec) -> (String, Placement) {
    // Replacing rather than stacking makes running `tbctl pam enable` twice a
    // no-op, which matters because package upgrades will do exactly that.
    let cleaned = remove_block(content);
    let lines: Vec<&str> = cleaned.lines().collect();

    let target = lines.iter().position(|l| line_stack(l) == Some(spec.stack));

    let mut out = String::with_capacity(cleaned.len() + 256);
    if let Some(idx) = target {
        for line in &lines[..idx] {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&block(spec));
        for line in &lines[idx..] {
            out.push_str(line);
            out.push('\n');
        }
        (out, Placement::BeforeFirstStackLine(idx))
    } else {
        out.push_str(&cleaned);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&block(spec));
        (out, Placement::AppendedWithNoStackPresent)
    }
}

// --- filesystem side ----------------------------------------------------

/// What happened, or would happen, to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The file does not exist; this service is not installed here.
    ServiceAbsent { service: String },
    /// Already correct.
    AlreadyPresent { service: String },
    /// Block added or removed.
    Modified {
        service: String,
        path: PathBuf,
        placement: Option<Placement>,
    },
    /// Nothing to remove.
    NothingToRemove { service: String },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceAbsent { service } => {
                write!(f, "  skipped  {service}: not installed on this system")
            }
            Self::AlreadyPresent { service } => {
                write!(f, "  ok       {service}: already configured")
            }
            Self::Modified {
                service, placement, ..
            } => match placement {
                Some(Placement::AppendedWithNoStackPresent) => write!(
                    f,
                    "  WARNING  {service}: no matching stack found, appended at the end"
                ),
                Some(Placement::BeforeFirstStackLine(_)) => write!(f, "  changed  {service}"),
                Some(Placement::AdoptedFromVendor) => write!(
                    f,
                    "  changed  {service}: would be copied from {VENDOR_DIR} into /etc/pam.d first"
                ),
                None => write!(f, "  removed  {service}"),
            },
            Self::NothingToRemove { service } => {
                write!(f, "  ok       {service}: nothing to remove")
            }
        }
    }
}

/// Where a distribution ships its own copies of the service files.
///
/// Fedora 44 keeps `plasmalogin` here and not in `/etc/pam.d` at all. A tool
/// that only looks in `/etc/pam.d` decides the display manager "is not
/// installed on this system" and configures nothing — while the machine is
/// running it. Files in `/etc/pam.d` override same-named ones here, which is
/// also the documented way to customise them.
pub const VENDOR_DIR: &str = "/usr/lib/pam.d";

/// Everything under one directory, so tests never touch the real `/etc/pam.d`.
#[derive(Debug, Clone)]
pub struct PamDir {
    root: PathBuf,
    vendor: Option<PathBuf>,
}

impl Default for PamDir {
    fn default() -> Self {
        Self::new("/etc/pam.d").with_vendor(VENDOR_DIR)
    }
}

impl PamDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            vendor: None,
        }
    }

    /// Also consider a vendor directory when a service is not in `/etc/pam.d`.
    #[must_use]
    pub fn with_vendor(mut self, dir: impl Into<PathBuf>) -> Self {
        self.vendor = Some(dir.into());
        self
    }

    fn path(&self, service: &str) -> PathBuf {
        self.root.join(service)
    }

    fn vendor_path(&self, service: &str) -> Option<PathBuf> {
        let p = self.vendor.as_ref()?.join(service);
        p.exists().then_some(p)
    }

    /// The file to edit, and whether it had to be adopted from the vendor
    /// directory first.
    ///
    /// Copying rather than editing in place: the vendor file belongs to a
    /// package and would be replaced on the next update, silently taking our
    /// rule with it.
    fn resolve(&self, service: &str, dry_run: bool) -> Result<Option<(PathBuf, bool)>> {
        let path = self.path(service);
        if path.exists() {
            return Ok(Some((path, false)));
        }
        let Some(vendor) = self.vendor_path(service) else {
            return Ok(None);
        };
        if !dry_run {
            fs::create_dir_all(&self.root)
                .with_context(|| format!("creating {}", self.root.display()))?;
            let body = fs::read_to_string(&vendor)
                .with_context(|| format!("reading {}", vendor.display()))?;
            write_atomically(
                &path,
                &format!(
                    "# Copied from {} by tbctl, because a rule added to the vendor file\n\
                     # would be lost on the next package update. Files here override\n\
                     # same-named ones there.\n{body}",
                    vendor.display()
                ),
            )?;
        }
        Ok(Some((path, true)))
    }

    /// The file a service lives in.
    #[must_use]
    pub fn service_path(&self, service: &str) -> PathBuf {
        self.path(service)
    }

    /// Adds the module to every managed service present on this system.
    pub fn enable(&self, dry_run: bool) -> Result<Vec<Change>> {
        MANAGED
            .iter()
            .map(|s| self.enable_one(s, dry_run))
            .collect()
    }

    fn enable_one(&self, spec: &ServiceSpec, dry_run: bool) -> Result<Change> {
        let Some((path, adopted)) = self.resolve(spec.service, dry_run)? else {
            return Ok(Change::ServiceAbsent {
                service: spec.service.to_owned(),
            });
        };
        if adopted && dry_run {
            return Ok(Change::Modified {
                service: spec.service.to_owned(),
                path,
                placement: Some(Placement::AdoptedFromVendor),
            });
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (updated, placement) = insert_block(&content, spec);
        if updated == content {
            return Ok(Change::AlreadyPresent {
                service: spec.service.to_owned(),
            });
        }
        if !dry_run {
            Self::backup_once(&path)?;
            write_atomically(&path, &updated)?;
        }
        Ok(Change::Modified {
            service: spec.service.to_owned(),
            path,
            placement: Some(placement),
        })
    }

    /// Removes the module from every managed service.
    pub fn disable(&self, dry_run: bool) -> Result<Vec<Change>> {
        MANAGED
            .iter()
            .map(|s| self.disable_one(s, dry_run))
            .collect()
    }

    fn disable_one(&self, spec: &ServiceSpec, dry_run: bool) -> Result<Change> {
        let path = self.path(spec.service);
        if !path.exists() {
            return Ok(Change::ServiceAbsent {
                service: spec.service.to_owned(),
            });
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        if !has_block(&content) {
            return Ok(Change::NothingToRemove {
                service: spec.service.to_owned(),
            });
        }
        if !dry_run {
            Self::backup_once(&path)?;
            write_atomically(&path, &remove_block(&content))?;
        }
        Ok(Change::Modified {
            service: spec.service.to_owned(),
            path,
            placement: None,
        })
    }

    /// Reports which managed services currently carry the module.
    pub fn status(&self) -> Result<Vec<(&'static ServiceSpec, ServiceState)>> {
        MANAGED
            .iter()
            .map(|spec| {
                let path = if self.path(spec.service).exists() {
                    self.path(spec.service)
                } else {
                    self.vendor_path(spec.service)
                        .unwrap_or_else(|| self.path(spec.service))
                };
                let state = if path.exists() {
                    let content = fs::read_to_string(&path)?;
                    if has_block(&content) {
                        ServiceState::Configured
                    } else {
                        ServiceState::NotConfigured
                    }
                } else {
                    ServiceState::Absent
                };
                Ok((spec, state))
            })
            .collect()
    }

    /// Takes a backup, but only the first time — so the backup always shows the
    /// system as it was before this tool touched it, not as it was one edit ago.
    fn backup_once(path: &Path) -> Result<()> {
        let backup = path.with_extension(path.extension().map_or_else(
            || BACKUP_SUFFIX.trim_start_matches('.').to_owned(),
            |e| format!("{}{BACKUP_SUFFIX}", e.to_string_lossy()),
        ));
        if backup.exists() {
            return Ok(());
        }
        fs::copy(path, &backup)
            .with_context(|| format!("backing up {} to {}", path.display(), backup.display()))?;
        Ok(())
    }
}

/// Whether a service carries the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Configured,
    NotConfigured,
    Absent,
}

/// Writes via a temporary file and a rename.
///
/// A partially written PAM stack is not a corrupted config file, it is a
/// machine nobody can log into. `rename` within the same directory is atomic,
/// so a reader sees either the old file or the new one.
fn write_atomically(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tbctl-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));

    let mode = fs::metadata(path).map_or(0o644, |m| m.permissions().mode());
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;

    // Without syncing the directory the rename itself can be lost in a crash,
    // which would undo the edit while the caller believes it succeeded.
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KDE_FEDORA: &str = "\
#%PAM-1.0
auth        include      system-auth
account     include      system-auth
password    include      system-auth
session     include      system-auth
";

    const SDDM_DEBIAN: &str = "\
#%PAM-1.0
@include common-auth
-auth   optional  pam_kwallet5.so
@include common-account
@include common-session
";

    fn spec(service: &str) -> &'static ServiceSpec {
        MANAGED
            .iter()
            .find(|s| s.service == service)
            .expect("known service")
    }

    #[test]
    fn recognises_the_three_ways_a_stack_is_written() {
        assert_eq!(line_stack("auth  include system-auth"), Some(Stack::Auth));
        assert_eq!(
            line_stack("-auth optional pam_kwallet5.so"),
            Some(Stack::Auth)
        );
        assert_eq!(line_stack("@include common-auth"), Some(Stack::Auth));
        assert_eq!(
            line_stack("account required pam_unix.so"),
            Some(Stack::Account)
        );
        assert_eq!(line_stack("@include common-account"), Some(Stack::Account));
        assert_eq!(line_stack("session optional pam_foo.so"), None);
        assert_eq!(line_stack("# a comment"), None);
        assert_eq!(line_stack(""), None);
    }

    #[test]
    fn the_block_lands_before_the_first_line_of_its_stack() {
        // Placing it after the module that checks the password would be useless:
        // by then authentication has already succeeded.
        let (out, placement) = insert_block(KDE_FEDORA, spec("kde"));
        assert_eq!(placement, Placement::BeforeFirstStackLine(1));

        let lines: Vec<&str> = out.lines().collect();
        let marker = lines.iter().position(|l| l.trim() == BEGIN).unwrap();
        let first_auth = lines
            .iter()
            .position(|l| line_stack(l) == Some(Stack::Auth))
            .unwrap();
        assert!(marker < first_auth, "block must precede the auth stack");
    }

    #[test]
    fn debians_include_style_is_understood() {
        let (out, placement) = insert_block(SDDM_DEBIAN, spec("sddm"));
        assert!(matches!(placement, Placement::BeforeFirstStackLine(_)));
        let lines: Vec<&str> = out.lines().collect();
        let marker = lines.iter().position(|l| l.trim() == BEGIN).unwrap();
        let first_account = lines
            .iter()
            .position(|l| l.trim() == "@include common-account")
            .unwrap();
        assert!(marker < first_account);
    }

    #[test]
    fn enabling_twice_changes_nothing() {
        // Package upgrades will do exactly this.
        let (once, _) = insert_block(KDE_FEDORA, spec("kde"));
        let (twice, _) = insert_block(&once, spec("kde"));
        assert_eq!(once, twice);
        assert_eq!(once.matches(BEGIN).count(), 1);
    }

    #[test]
    fn removing_restores_the_original_exactly() {
        for original in [KDE_FEDORA, SDDM_DEBIAN] {
            let (with, _) = insert_block(original, spec("kde"));
            assert_eq!(remove_block(&with), original);
        }
    }

    #[test]
    fn a_half_written_block_can_still_be_removed() {
        // An interrupted edit must not leave a file only a human can repair.
        let broken = format!("{KDE_FEDORA}{BEGIN}\nauth requisite pam_timebandits.so\n");
        let cleaned = remove_block(&broken);
        assert!(!cleaned.contains("pam_timebandits"));
        assert!(!cleaned.contains(BEGIN));
    }

    #[test]
    fn a_file_without_the_stack_is_flagged_not_guessed_at() {
        let session_only = "session optional pam_foo.so\n";
        let (out, placement) = insert_block(session_only, spec("kde"));
        assert_eq!(placement, Placement::AppendedWithNoStackPresent);
        assert!(out.contains("pam_timebandits.so"));
    }

    /// The rule line a block writes, with the comments stripped.
    fn rule_for(service: &str) -> String {
        let (out, _) = insert_block(KDE_FEDORA, spec(service));
        out.lines()
            .find(|l| l.contains("pam_timebandits.so") && !l.trim_start().starts_with('#'))
            .expect("the rule")
            .to_owned()
    }

    #[test]
    fn the_written_line_is_the_one_we_intend() {
        let rule = rule_for("kde");
        assert!(rule.starts_with("auth "), "{rule}");
        assert!(rule.ends_with(" pam_timebandits.so"), "{rule}");
        // `die` rather than `bad`: the lock screen should stop at the refusal
        // and show it, not go on to ask for a password that cannot work.
        assert!(rule.contains("default=die"), "{rule}");
    }

    #[test]
    fn account_services_get_the_account_stack() {
        let rule = rule_for("sddm");
        assert!(rule.starts_with("account "), "{rule}");
        assert!(rule.contains("default=bad"), "{rule}");
    }

    #[test]
    fn a_missing_module_can_never_lock_anybody_out() {
        // Measured on Fedora 44, not assumed: with a plain `requisite` or
        // `required` control word, deleting the module locks every account out
        // of the machine, and the leading-dash form does not help — it only
        // silences the log entry. This is the line that makes an uninstall, or
        // a system extension that failed to merge, survivable.
        for service in ["kde", "sddm", "login"] {
            let rule = rule_for(service);
            assert!(
                rule.contains("module_unknown=ignore"),
                "{service}: a missing module would lock everyone out — {rule}"
            );
            let control = rule
                .split_once('[')
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(c, _)| c)
                .unwrap_or_default();
            assert!(
                !control.is_empty(),
                "{service}: the control must be the bracket form, not a keyword — {rule}"
            );
        }
    }

    #[test]
    fn the_rule_still_parses_as_its_own_stack() {
        // `pam disable` and the placement logic both read these lines back.
        assert_eq!(line_stack(&rule_for("kde")), Some(Stack::Auth));
        assert_eq!(line_stack(&rule_for("sddm")), Some(Stack::Account));
    }

    // --- filesystem behaviour, against a throwaway root --------------------

    fn fixture() -> (tempfile::TempDir, PamDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pam.d");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("kde"), KDE_FEDORA).unwrap();
        fs::write(root.join("sddm"), SDDM_DEBIAN).unwrap();
        // `login` deliberately absent: not every system has every service.
        let pam = PamDir::new(&root);
        (dir, pam)
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let (_d, pam) = fixture();
        let before = fs::read_to_string(pam.path("kde")).unwrap();
        let changes = pam.enable(true).unwrap();
        assert!(changes.iter().any(|c| matches!(c, Change::Modified { .. })));
        assert_eq!(fs::read_to_string(pam.path("kde")).unwrap(), before);
        assert!(!pam.path("kde.timebandits-backup").exists());
    }

    #[test]
    fn enable_then_disable_leaves_the_files_as_they_were() {
        let (_d, pam) = fixture();
        let before: Vec<String> = ["kde", "sddm"]
            .iter()
            .map(|s| fs::read_to_string(pam.path(s)).unwrap())
            .collect();

        pam.enable(false).unwrap();
        assert!(
            fs::read_to_string(pam.path("kde"))
                .unwrap()
                .contains("pam_timebandits.so")
        );

        pam.disable(false).unwrap();
        for (i, s) in ["kde", "sddm"].iter().enumerate() {
            assert_eq!(fs::read_to_string(pam.path(s)).unwrap(), before[i], "{s}");
        }
    }

    #[test]
    fn a_missing_service_is_skipped_not_created() {
        let (_d, pam) = fixture();
        let changes = pam.enable(false).unwrap();
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::ServiceAbsent { service } if service == "login"))
        );
        assert!(!pam.path("login").exists(), "must not invent a PAM service");
    }

    #[test]
    fn the_backup_captures_the_state_before_any_edit() {
        let (_d, pam) = fixture();
        let pristine = fs::read_to_string(pam.path("kde")).unwrap();

        pam.enable(false).unwrap();
        pam.disable(false).unwrap();
        pam.enable(false).unwrap();

        // Three edits later the backup still shows the untouched original,
        // which is the only version worth restoring.
        let backup = fs::read_to_string(pam.path("kde.timebandits-backup")).unwrap();
        assert_eq!(backup, pristine);
    }

    #[test]
    fn file_permissions_survive_an_edit() {
        let (_d, pam) = fixture();
        fs::set_permissions(pam.path("kde"), fs::Permissions::from_mode(0o600)).unwrap();
        pam.enable(false).unwrap();
        let mode = fs::metadata(pam.path("kde")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a widened PAM file would be its own problem");
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let (_d, pam) = fixture();
        pam.enable(false).unwrap();
        let strays: Vec<_> = fs::read_dir(pam.path("").parent().unwrap().join("pam.d"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tbctl-tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    /// The state of one named service, so these tests do not have to be
    /// rewritten every time the managed list grows.
    fn state_of(states: &[(&ServiceSpec, ServiceState)], service: &str) -> ServiceState {
        states
            .iter()
            .find(|(s, _)| s.service == service)
            .map_or_else(|| panic!("`{service}` is not managed"), |(_, st)| *st)
    }

    #[test]
    fn status_reports_each_service_accurately() {
        let (_d, pam) = fixture();
        let before = pam.status().unwrap();
        assert_eq!(state_of(&before, "kde"), ServiceState::NotConfigured);
        assert_eq!(state_of(&before, "sddm"), ServiceState::NotConfigured);
        assert_eq!(state_of(&before, "login"), ServiceState::Absent);

        pam.enable(false).unwrap();
        let after = pam.status().unwrap();
        assert_eq!(state_of(&after, "kde"), ServiceState::Configured);
        assert_eq!(state_of(&after, "sddm"), ServiceState::Configured);
        assert_eq!(state_of(&after, "login"), ServiceState::Absent);
    }

    #[test]
    fn every_display_manager_we_know_about_is_covered() {
        // Plasma 6.5 replaced SDDM with plasmalogin, and Fedora 44 KDE and
        // Bazzite ship it. Covering only sddm there means covering no login
        // screen at all: the child is locked out of the session and logs
        // straight back in.
        let managed: Vec<&str> = MANAGED.iter().map(|s| s.service).collect();
        for dm in ["sddm", "plasmalogin"] {
            assert!(managed.contains(&dm), "{dm} is not covered: {managed:?}");
        }
        for spec in MANAGED
            .iter()
            .filter(|s| s.service != "kde" && s.service != "kcheckpass")
        {
            assert_eq!(
                spec.stack,
                Stack::Account,
                "{} guards a login, which is the account stack",
                spec.service
            );
        }
    }

    #[test]
    fn a_service_that_only_exists_in_the_vendor_directory_is_adopted() {
        // Fedora 44 keeps plasmalogin in /usr/lib/pam.d and nowhere else. A
        // tool that only reads /etc/pam.d calls the running display manager
        // "not installed" and configures nothing.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pam.d");
        let vendor = dir.path().join("vendor-pam.d");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("plasmalogin"), SDDM_DEBIAN).unwrap();
        let pam = PamDir::new(&root).with_vendor(&vendor);

        assert_eq!(
            state_of(&pam.status().unwrap(), "plasmalogin"),
            ServiceState::NotConfigured,
            "a vendor file is present, not absent"
        );

        pam.enable(false).unwrap();
        let written = fs::read_to_string(root.join("plasmalogin")).expect("adopted into /etc");
        assert!(has_block(&written), "{written}");
        assert!(
            written.contains("Copied from"),
            "says where it came from: {written}"
        );
        // The vendor file belongs to a package and must be left alone, or the
        // next update silently takes our rule with it.
        assert!(!has_block(
            &fs::read_to_string(vendor.join("plasmalogin")).unwrap()
        ));
    }

    #[test]
    fn a_dry_run_adopts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pam.d");
        let vendor = dir.path().join("vendor-pam.d");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("plasmalogin"), SDDM_DEBIAN).unwrap();

        PamDir::new(&root)
            .with_vendor(&vendor)
            .enable(true)
            .unwrap();
        assert!(!root.join("plasmalogin").exists(), "nothing was written");
    }
}
