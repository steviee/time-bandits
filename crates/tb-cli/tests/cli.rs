// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end tests that run the real `tbctl` binary.
//!
//! The unit tests cover the parsing, the formatting and the file edits in
//! isolation. What they cannot cover is the wiring: whether an argument
//! actually reaches the code that handles it, whether a change is persisted,
//! and whether the exit code says what happened. Every bug those tests would
//! miss is one a person hits on their first command.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jiff::civil;
use tb_core::appid::{AppId, AppIdSource};
use tb_core::usage::UsageSegment;
use tb_daemon::store::Store;
use uuid::Uuid;

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    pam_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("state.db");
        Store::open(&db).expect("create database");

        let pam_root = dir.path().join("pam.d");
        std::fs::create_dir_all(&pam_root).unwrap();
        // Two of the three managed services, so the "not installed" path is
        // exercised as well.
        std::fs::write(
            pam_root.join("kde"),
            "#%PAM-1.0\nauth        include      system-auth\naccount     include      system-auth\n",
        )
        .unwrap();
        std::fs::write(
            pam_root.join("sddm"),
            "#%PAM-1.0\n@include common-auth\n@include common-account\n",
        )
        .unwrap();

        Self {
            _dir: dir,
            db,
            pam_root,
        }
    }

    fn store(&self) -> Store {
        Store::open(&self.db).expect("open database")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tbctl"))
            .arg("--database")
            .arg(&self.db)
            .args(args)
            .output()
            .expect("run tbctl")
    }

    /// Runs and asserts success, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "tbctl {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs and asserts failure, returning stderr.
    fn err(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "tbctl {args:?} unexpectedly succeeded: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// Books usage for today, so the tests do not depend on the wall clock
    /// beyond "now".
    fn record_usage(&self, user: &str, app: &str, minutes: i64) {
        let end = jiff::Timestamp::now();
        let start = end
            .checked_sub(jiff::SignedDuration::from_secs(minutes * 60))
            .unwrap();
        self.store()
            .insert_segment(&UsageSegment {
                id: Uuid::now_v7(),
                subject: user.to_owned(),
                app: AppId::new(app),
                source: AppIdSource::DesktopFile,
                start,
                end,
                title: None,
            })
            .expect("record usage");
    }
}

/// The timezone the tests configure, so "today" means the same to tbctl and
/// to the fixture.
const TZ: &str = "Europe/Berlin";

fn setup_kid(f: &Fixture) {
    f.ok(&[
        "policy",
        "set",
        "kid",
        "--enforcement",
        "true",
        "--daily",
        "2h",
        "--timezone",
        TZ,
    ]);
}

#[test]
fn setting_a_policy_creates_and_persists_it() {
    let f = Fixture::new();
    let out = f.ok(&[
        "policy",
        "set",
        "kid",
        "--enforcement",
        "true",
        "--daily",
        "2h",
        "--daily",
        "sat=3h",
        "--timezone",
        TZ,
    ]);
    assert!(out.contains("version 2"), "{out}");

    // Persisted, not just printed.
    let stored = f.store().load_policy("kid").unwrap().expect("stored");
    assert!(stored.enforcement);
    assert_eq!(stored.timezone, TZ);

    let shown = f.ok(&["policy", "show", "kid"]);
    assert!(shown.contains("saturday   3 h"), "{shown}");
    assert!(shown.contains("monday     2 h"), "{shown}");
}

#[test]
fn a_policy_set_with_no_changes_is_refused() {
    // Silently doing nothing and reporting success would be worse: the parent
    // walks away believing something changed.
    let f = Fixture::new();
    setup_kid(&f);
    let err = f.err(&["policy", "set", "kid"]);
    assert!(err.contains("nothing to change"), "{err}");
}

#[test]
fn an_invalid_policy_is_rejected_and_nothing_is_stored() {
    let f = Fixture::new();
    setup_kid(&f);
    let before = f.store().load_policy("kid").unwrap().unwrap();

    let err = f.err(&["policy", "set", "kid", "--timezone", "Mars/Olympus_Mons"]);
    assert!(err.contains("Mars"), "{err}");

    let after = f.store().load_policy("kid").unwrap().unwrap();
    assert_eq!(after, before, "a rejected edit must leave no trace");
}

#[test]
fn contradictory_arguments_are_refused() {
    let f = Fixture::new();
    setup_kid(&f);
    let err = f.err(&[
        "policy",
        "set",
        "kid",
        "--daily",
        "mon=0s",
        "--window",
        "mon=15:00-19:00",
    ]);
    assert!(err.contains("contradictory"), "{err}");
}

#[test]
fn status_reports_remaining_time_and_then_the_block() {
    let f = Fixture::new();
    setup_kid(&f);

    f.record_usage("kid", "firefox", 90);
    let out = f.ok(&["status", "kid"]);
    assert!(out.contains("used today   1 h 30 min"), "{out}");
    assert!(out.contains("remaining    30 min"), "{out}");
    assert!(!out.contains("BLOCKED"), "{out}");

    f.record_usage("kid", "firefox", 45);
    let out = f.ok(&["status", "kid"]);
    assert!(out.contains("BLOCKED"), "{out}");
    assert!(out.contains("daily quota used up"), "{out}");
}

#[test]
fn granting_bonus_lifts_the_block_immediately() {
    // The whole point of the command: a parent says yes and the child is back
    // in, without restarting anything.
    let f = Fixture::new();
    setup_kid(&f);
    f.record_usage("kid", "firefox", 130);
    assert!(f.ok(&["status", "kid"]).contains("BLOCKED"));

    let out = f.ok(&["grant-bonus", "kid", "30m"]);
    assert!(out.contains("granted 30 min"), "{out}");
    assert!(!out.contains("BLOCKED"), "status after granting: {out}");

    let after = f.ok(&["status", "kid"]);
    assert!(after.contains("bonus today  30 min"), "{after}");
    assert!(!after.contains("BLOCKED"), "{after}");
}

#[test]
fn usage_breaks_time_down_by_application() {
    let f = Fixture::new();
    setup_kid(&f);
    f.record_usage("kid", "firefox", 60);
    f.record_usage("kid", "org.kde.konsole", 20);

    let out = f.ok(&["usage", "kid"]);
    let lines: Vec<&str> = out.lines().collect();
    let firefox = lines
        .iter()
        .position(|l| l.contains("firefox"))
        .expect("firefox");
    let konsole = lines
        .iter()
        .position(|l| l.contains("konsole"))
        .expect("konsole");
    assert!(firefox < konsole, "longest first:\n{out}");
    assert!(out.contains("TOTAL"), "{out}");
}

#[test]
fn a_policy_survives_an_export_import_round_trip() {
    let f = Fixture::new();
    f.ok(&[
        "policy",
        "set",
        "kid",
        "--enforcement",
        "true",
        "--daily",
        "2h",
        "--window",
        "mon=15:00-19:00",
        "--timezone",
        TZ,
    ]);
    let exported = f.ok(&["policy", "export", "kid"]);
    assert!(exported.contains("subject = \"kid\""), "{exported}");

    let file = f._dir.path().join("kid.toml");
    std::fs::write(&file, &exported).unwrap();
    let out = f.ok(&["policy", "import", "kid", "--input", file.to_str().unwrap()]);
    assert!(out.contains("imported"), "{out}");

    let shown = f.ok(&["policy", "show", "kid"]);
    assert!(shown.contains("15:00-19:00"), "{shown}");
}

#[test]
fn importing_one_childs_rules_onto_another_is_refused() {
    // A silent rewrite here hands one child the other's limits.
    let f = Fixture::new();
    setup_kid(&f);
    let exported = f.ok(&["policy", "export", "kid"]);
    let file = f._dir.path().join("kid.toml");
    std::fs::write(&file, &exported).unwrap();

    let err = f.err(&[
        "policy",
        "import",
        "sibling",
        "--input",
        file.to_str().unwrap(),
    ]);
    assert!(err.contains("describes `kid`"), "{err}");
}

#[test]
fn an_unknown_user_is_an_error_not_an_empty_report() {
    let f = Fixture::new();
    let err = f.err(&["status", "nobody"]);
    assert!(err.contains("no policy"), "{err}");
}

#[test]
fn status_without_a_user_covers_everyone_managed() {
    let f = Fixture::new();
    setup_kid(&f);
    f.ok(&[
        "policy",
        "set",
        "sibling",
        "--enforcement",
        "true",
        "--daily",
        "1h",
        "--timezone",
        TZ,
    ]);

    let out = f.ok(&["status"]);
    assert!(out.contains("kid"), "{out}");
    assert!(out.contains("sibling"), "{out}");
}

#[test]
fn pam_enable_is_reversible_and_a_dry_run_touches_nothing() {
    let f = Fixture::new();
    let root = f.pam_root.to_str().unwrap();
    let before = std::fs::read_to_string(f.pam_root.join("kde")).unwrap();

    let dry = f.ok(&["pam", "enable", "--dry-run", "--root", root]);
    assert!(dry.contains("dry run"), "{dry}");
    assert_eq!(
        std::fs::read_to_string(f.pam_root.join("kde")).unwrap(),
        before
    );

    f.ok(&["pam", "enable", "--root", root]);
    let after = std::fs::read_to_string(f.pam_root.join("kde")).unwrap();
    assert!(after.contains("pam_timebandits.so"), "{after}");

    let status = f.ok(&["pam", "status", "--root", root]);
    assert!(status.contains("configured"), "{status}");
    assert!(
        status.contains("not installed"),
        "login is absent: {status}"
    );

    f.ok(&["pam", "disable", "--root", root]);
    assert_eq!(
        std::fs::read_to_string(f.pam_root.join("kde")).unwrap(),
        before,
        "disable must restore the file byte for byte"
    );
}

#[test]
fn pam_commands_work_without_a_database() {
    // The moment someone most needs `pam disable` is when things are broken,
    // which may well include the daemon never having run.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pam.d");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("kde"), "auth include system-auth\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_tbctl"))
        .args([
            "--database",
            "/nonexistent/state.db",
            "pam",
            "status",
            "--root",
        ])
        .arg(&root)
        .output()
        .expect("run tbctl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn doctor_fails_on_an_unconfigured_system_and_says_why() {
    let f = Fixture::new();
    setup_kid(&f);
    let out = f.run(&[
        "doctor",
        "--pam-root",
        f.pam_root.to_str().unwrap(),
        "--socket",
        "/nonexistent/pam.sock",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "must exit non-zero: {text}");
    assert!(text.contains("[FAIL] daemon"), "{text}");
    assert!(
        text.contains("lock screen"),
        "the callout that matters: {text}"
    );
}

#[test]
fn doctor_warns_about_observe_only_policies() {
    // A policy that records but limits nothing is the likeliest way to believe
    // you are protected when you are not.
    let f = Fixture::new();
    f.ok(&["policy", "set", "kid", "--daily", "2h", "--timezone", TZ]);
    let out = f.run(&["doctor", "--pam-root", f.pam_root.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("observe only"), "{text}");
}

#[test]
fn the_day_boundary_is_the_policy_day_not_midnight() {
    // Usage from before today's 04:00 boundary must not count against today.
    let f = Fixture::new();
    setup_kid(&f);

    let tz = jiff::tz::TimeZone::get(TZ).unwrap();
    let now = jiff::Zoned::now().with_time_zone(tz.clone());
    let today = tb_core::schedule::policy_day(&now, civil::time(4, 0, 0, 0));
    let boundary = tb_core::schedule::policy_day_end(today, civil::time(4, 0, 0, 0), &tz)
        .checked_sub(jiff::Span::new().days(1))
        .unwrap();

    // Two hours ending right before this policy day began.
    let end = boundary.timestamp();
    let start = end
        .checked_sub(jiff::SignedDuration::from_hours(2))
        .unwrap();
    f.store()
        .insert_segment(&UsageSegment {
            id: Uuid::now_v7(),
            subject: "kid".to_owned(),
            app: AppId::new("firefox"),
            source: AppIdSource::DesktopFile,
            start,
            end,
            title: None,
        })
        .unwrap();

    let out = f.ok(&["status", "kid"]);
    assert!(
        out.contains("used today   none"),
        "yesterday's usage leaked into today:\n{out}"
    );
    assert!(!out.contains("BLOCKED"), "{out}");
}

#[test]
fn help_and_version_work() {
    // Cheap, but the first thing anyone runs.
    for args in [["--help"], ["--version"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_tbctl"))
            .args(args)
            .output()
            .expect("run tbctl");
        assert!(out.status.success(), "tbctl {args:?}");
        assert!(!out.stdout.is_empty());
    }
    let _ = Path::new("");
}
