// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Local persistence.
//!
//! The local database is authoritative, not a cache. A household server is
//! optional and may be unreachable for a week; enforcement has to keep working
//! from what is on disk. Everything here is therefore written to survive an
//! abrupt power loss and to be replayable to the hub later.
//!
//! `rusqlite` is used **without** the `bundled` feature on purpose: linking the
//! system SQLite keeps the package acceptable to distributions, which do not
//! allow bundled copies of libraries.

use std::path::Path;

use jiff::{Timestamp, Zoned};
use rusqlite::{Connection, OptionalExtension, params};
use tb_core::appid::{AppId, AppIdSource};
use tb_core::duration::DurationSpec;
use tb_core::engine::UsageSnapshot;
use tb_core::policy::Policy;
use tb_core::schedule::{PolicyDay, policy_day, policy_day_end};
use tb_core::usage::UsageSegment;
use uuid::Uuid;

/// Bumped whenever the schema changes; migrations run on open.
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored policy for `{subject}` is unreadable: {source}")]
    CorruptPolicy {
        subject: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("database was written by a newer version (schema {found}, we understand {ours})")]
    SchemaTooNew { found: i64, ours: i64 },
}

/// Something worth remembering that is not usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Session locked because time ran out.
    Locked,
    /// A login or unlock was refused.
    AccessDenied,
    /// Tracking looks tampered with: agent gone, or focus reports stopped.
    Tamper,
    /// The wall clock moved in a way that plain drift does not explain.
    ClockJump,
    /// Bonus time granted by a parent.
    BonusGranted,
}

impl EventKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::AccessDenied => "access_denied",
            Self::Tamper => "tamper",
            Self::ClockJump => "clock_jump",
            Self::BonusGranted => "bonus_granted",
        }
    }
}

/// The on-disk state of one machine.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (and if necessary creates) the database at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// An in-memory database, for tests.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        // WAL survives an unclean shutdown without losing committed writes, and
        // lets a reader (tbctl, the D-Bus API) run while the tick loop writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        let found: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if found > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found,
                ours: SCHEMA_VERSION,
            });
        }
        if found == SCHEMA_VERSION {
            return Ok(());
        }

        self.conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS policy (
                subject     TEXT PRIMARY KEY,
                version     INTEGER NOT NULL,
                json        TEXT NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS usage_segment (
                id        BLOB PRIMARY KEY,
                subject   TEXT NOT NULL,
                app       TEXT NOT NULL,
                source    TEXT NOT NULL,
                start_ts  INTEGER NOT NULL,
                end_ts    INTEGER NOT NULL,
                title     TEXT,
                synced    INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS usage_segment_subject_time
                ON usage_segment (subject, start_ts);
            CREATE INDEX IF NOT EXISTS usage_segment_unsynced
                ON usage_segment (synced) WHERE synced = 0;

            CREATE TABLE IF NOT EXISTS bonus_grant (
                id          BLOB PRIMARY KEY,
                subject     TEXT NOT NULL,
                policy_date TEXT NOT NULL,
                seconds     INTEGER NOT NULL,
                granted_by  TEXT NOT NULL,
                granted_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS bonus_grant_subject_date
                ON bonus_grant (subject, policy_date);

            CREATE TABLE IF NOT EXISTS event (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                subject  TEXT NOT NULL,
                kind     TEXT NOT NULL,
                detail   TEXT,
                at       INTEGER NOT NULL,
                synced   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS event_subject_time ON event (subject, at);
            ",
        )?;
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    // --- policies ----------------------------------------------------------

    /// Stores a policy, but never replaces a newer one with an older one.
    ///
    /// Returns whether it was actually written. Out-of-order delivery from the
    /// hub is normal after a reconnect, and quietly regressing to a stale policy
    /// would hand back time a parent already took away.
    pub fn save_policy(&self, policy: &Policy) -> Result<bool, StoreError> {
        let json = serde_json::to_string(policy).map_err(|source| StoreError::CorruptPolicy {
            subject: policy.subject.clone(),
            source,
        })?;
        let changed = self.conn.execute(
            "INSERT INTO policy (subject, version, json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(subject) DO UPDATE SET
                 version    = excluded.version,
                 json       = excluded.json,
                 updated_at = excluded.updated_at
             WHERE excluded.version > policy.version",
            params![
                policy.subject,
                i64::try_from(policy.version).unwrap_or(i64::MAX),
                json,
                Timestamp::now().as_second()
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn load_policy(&self, subject: &str) -> Result<Option<Policy>, StoreError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM policy WHERE subject = ?1",
                params![subject],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|j| {
            serde_json::from_str(&j).map_err(|source| StoreError::CorruptPolicy {
                subject: subject.to_owned(),
                source,
            })
        })
        .transpose()
    }

    /// Every user a policy exists for.
    pub fn subjects(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT subject FROM policy ORDER BY subject")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // --- usage -------------------------------------------------------------

    /// Records a finished segment. Writing the same segment twice is harmless,
    /// which is what makes replaying a sync queue safe.
    pub fn insert_segment(&self, seg: &UsageSegment) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO usage_segment (id, subject, app, source, start_ts, end_ts, title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO NOTHING",
            params![
                seg.id.as_bytes().as_slice(),
                seg.subject,
                seg.app.as_str(),
                format!("{:?}", seg.source),
                seg.start.as_second(),
                seg.end.as_second(),
                seg.title,
            ],
        )?;
        Ok(())
    }

    /// Time credited to `subject` that falls inside `[from, to)`.
    ///
    /// Segments are clipped rather than counted whole, so a session running
    /// across the policy-day boundary contributes to both days correctly.
    pub fn usage_between(
        &self,
        subject: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<DurationSpec, StoreError> {
        let (from, to) = (from.as_second(), to.as_second());
        let secs: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(MIN(end_ts, ?2) - MAX(start_ts, ?3)), 0)
             FROM usage_segment
             WHERE subject = ?1 AND end_ts > ?3 AND start_ts < ?2",
            params![subject, to, from],
            |r| r.get(0),
        )?;
        Ok(DurationSpec::from_secs(u64::try_from(secs).unwrap_or(0)))
    }

    /// Segments overlapping a range, newest first. For reports and sync.
    pub fn segments_between(
        &self,
        subject: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<UsageSegment>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, subject, app, source, start_ts, end_ts, title
             FROM usage_segment
             WHERE subject = ?1 AND end_ts > ?2 AND start_ts < ?3
             ORDER BY start_ts DESC",
        )?;
        let rows = stmt.query_map(params![subject, from.as_second(), to.as_second()], |r| {
            let id: Vec<u8> = r.get(0)?;
            let source: String = r.get(3)?;
            Ok(UsageSegment {
                id: Uuid::from_slice(&id).unwrap_or(Uuid::nil()),
                subject: r.get(1)?,
                app: AppId::new(&r.get::<_, String>(2)?),
                source: parse_source(&source),
                start: Timestamp::from_second(r.get(4)?).unwrap_or(Timestamp::UNIX_EPOCH),
                end: Timestamp::from_second(r.get(5)?).unwrap_or(Timestamp::UNIX_EPOCH),
                title: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // --- bonus -------------------------------------------------------------

    /// Grants extra time for one policy day.
    pub fn add_bonus(
        &self,
        subject: &str,
        day: PolicyDay,
        amount: DurationSpec,
        granted_by: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO bonus_grant (id, subject, policy_date, seconds, granted_by, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::now_v7().as_bytes().as_slice(),
                subject,
                day.date.to_string(),
                i64::try_from(amount.as_secs()).unwrap_or(i64::MAX),
                granted_by,
                Timestamp::now().as_second(),
            ],
        )?;
        self.record_event(
            subject,
            EventKind::BonusGranted,
            Some(&format!("{amount} by {granted_by}")),
        )
    }

    /// Total bonus granted for one policy day.
    pub fn bonus_for(&self, subject: &str, day: PolicyDay) -> Result<DurationSpec, StoreError> {
        let secs: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(seconds), 0) FROM bonus_grant
             WHERE subject = ?1 AND policy_date = ?2",
            params![subject, day.date.to_string()],
            |r| r.get(0),
        )?;
        Ok(DurationSpec::from_secs(u64::try_from(secs).unwrap_or(0)))
    }

    // --- events ------------------------------------------------------------

    pub fn record_event(
        &self,
        subject: &str,
        kind: EventKind,
        detail: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO event (subject, kind, detail, at) VALUES (?1, ?2, ?3, ?4)",
            params![subject, kind.as_str(), detail, Timestamp::now().as_second()],
        )?;
        Ok(())
    }

    pub fn event_count(&self, subject: &str, kind: EventKind) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM event WHERE subject = ?1 AND kind = ?2",
            params![subject, kind.as_str()],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    // --- the figures the engine needs --------------------------------------

    /// Assembles the usage snapshot for one user at one instant.
    ///
    /// This is the bridge between what is on disk and [`tb_core::evaluate`]:
    /// it resolves the policy day, clips segments to the day and week windows,
    /// and adds any bonus granted for today.
    pub fn snapshot(
        &self,
        policy: &Policy,
        now: &Zoned,
    ) -> Result<(UsageSnapshot, PolicyDay), StoreError> {
        let tz = jiff::tz::TimeZone::get(&policy.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
        let now = now.with_time_zone(tz.clone());
        let today = policy_day(&now, policy.day_start);

        // The policy day runs from its own start to the start of the next one.
        let day_end = policy_day_end(today, policy.day_start, &tz);
        let day_start_instant = day_end
            .checked_sub(jiff::Span::new().days(1))
            .unwrap_or_else(|_| day_end.clone());

        // The policy week starts on the Monday of this policy day.
        let back = i64::from(today.date.weekday().to_monday_zero_offset());
        let week_start = today
            .date
            .checked_sub(jiff::Span::new().days(back))
            .unwrap_or(today.date)
            .to_datetime(policy.day_start)
            .to_zoned(tz)
            .unwrap_or_else(|_| day_start_instant.clone());

        let now_ts = now.timestamp();
        Ok((
            UsageSnapshot {
                used_today: self.usage_between(
                    &policy.subject,
                    day_start_instant.timestamp(),
                    now_ts,
                )?,
                used_this_week: self.usage_between(
                    &policy.subject,
                    week_start.timestamp(),
                    now_ts,
                )?,
                bonus_today: self.bonus_for(&policy.subject, today)?,
            },
            today,
        ))
    }
}

fn parse_source(s: &str) -> AppIdSource {
    match s {
        "DesktopFile" => AppIdSource::DesktopFile,
        "WindowClass" => AppIdSource::WindowClass,
        "SystemdScope" => AppIdSource::SystemdScope,
        _ => AppIdSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;
    use tb_core::policy::Quota;
    use tb_core::schedule::WeekSchedule;

    fn tz() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("Europe/Berlin").unwrap()
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        civil::date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .unwrap()
    }

    fn seg(subject: &str, app: &str, from: &Zoned, mins: i64) -> UsageSegment {
        let start = from.timestamp();
        UsageSegment {
            id: Uuid::now_v7(),
            subject: subject.to_owned(),
            app: AppId::new(app),
            source: AppIdSource::DesktopFile,
            start,
            end: start
                .checked_add(jiff::SignedDuration::from_secs(mins * 60))
                .unwrap(),
            title: None,
        }
    }

    fn policy_2h() -> Policy {
        let mut p = Policy::permissive("kid");
        p.enforcement = true;
        p.timezone = "Europe/Berlin".to_owned();
        p.daily_quota = WeekSchedule::uniform(Quota::Limited(DurationSpec::from_hours(2)));
        p
    }

    #[test]
    fn a_fresh_database_is_usable() {
        let s = Store::in_memory().unwrap();
        assert!(s.subjects().unwrap().is_empty());
        assert_eq!(
            s.usage_between("kid", Timestamp::UNIX_EPOCH, Timestamp::now())
                .unwrap(),
            DurationSpec::ZERO
        );
    }

    #[test]
    fn opening_twice_does_not_re_run_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let s = Store::open(&path).unwrap();
            s.save_policy(&policy_2h()).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.subjects().unwrap(), vec!["kid".to_owned()]);
    }

    #[test]
    fn a_policy_round_trips() {
        let s = Store::in_memory().unwrap();
        let p = policy_2h();
        assert!(s.save_policy(&p).unwrap());
        let back = s.load_policy("kid").unwrap().expect("stored");
        assert_eq!(back, p);
        assert!(s.load_policy("nobody").unwrap().is_none());
    }

    #[test]
    fn an_older_policy_never_overwrites_a_newer_one() {
        // Out-of-order delivery after a reconnect must not hand back time a
        // parent has already taken away.
        let s = Store::in_memory().unwrap();
        let mut new = policy_2h();
        new.version = 7;
        assert!(s.save_policy(&new).unwrap());

        let mut old = policy_2h();
        old.version = 3;
        old.daily_quota = WeekSchedule::uniform(Quota::Unlimited);
        assert!(!s.save_policy(&old).unwrap(), "must be refused");

        let stored = s.load_policy("kid").unwrap().unwrap();
        assert_eq!(stored.version, 7);
        assert_eq!(
            *stored.daily_quota.get(tb_core::Day::Monday),
            Quota::Limited(DurationSpec::from_hours(2))
        );
    }

    #[test]
    fn inserting_the_same_segment_twice_counts_it_once() {
        // The property the whole sync design rests on.
        let s = Store::in_memory().unwrap();
        let g = seg("kid", "firefox", &at(2026, 8, 19, 15, 0), 30);
        s.insert_segment(&g).unwrap();
        s.insert_segment(&g).unwrap();
        s.insert_segment(&g).unwrap();
        let used = s
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(used, DurationSpec::from_mins(30));
    }

    #[test]
    fn a_segment_crossing_the_boundary_is_split_not_double_counted() {
        let s = Store::in_memory().unwrap();
        // 03:30 to 04:30 straddles the 04:00 policy-day boundary.
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 19, 3, 30), 60))
            .unwrap();

        let before = s
            .usage_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 19, 4, 0).timestamp(),
            )
            .unwrap();
        let after = s
            .usage_between(
                "kid",
                at(2026, 8, 19, 4, 0).timestamp(),
                at(2026, 8, 19, 23, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(before, DurationSpec::from_mins(30));
        assert_eq!(after, DurationSpec::from_mins(30));
    }

    #[test]
    fn usage_is_per_user() {
        let s = Store::in_memory().unwrap();
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 19, 15, 0), 30))
            .unwrap();
        s.insert_segment(&seg("sibling", "firefox", &at(2026, 8, 19, 15, 0), 90))
            .unwrap();
        let range = (
            at(2026, 8, 19, 0, 0).timestamp(),
            at(2026, 8, 20, 0, 0).timestamp(),
        );
        assert_eq!(
            s.usage_between("kid", range.0, range.1).unwrap(),
            DurationSpec::from_mins(30)
        );
        assert_eq!(
            s.usage_between("sibling", range.0, range.1).unwrap(),
            DurationSpec::from_mins(90)
        );
    }

    #[test]
    fn the_snapshot_uses_policy_days_not_calendar_days() {
        let s = Store::in_memory().unwrap();
        // 23:30 on the 18th and 01:00 on the 19th both belong to policy day
        // the 18th, because the day starts at 04:00.
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 18, 23, 30), 20))
            .unwrap();
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 19, 1, 0), 40))
            .unwrap();

        let (snap, day) = s.snapshot(&policy_2h(), &at(2026, 8, 19, 2, 0)).unwrap();
        assert_eq!(day.date, civil::date(2026, 8, 18));
        assert_eq!(snap.used_today, DurationSpec::from_mins(60));

        // After 04:00 the same segments belong to the previous day only.
        let (snap, day) = s.snapshot(&policy_2h(), &at(2026, 8, 19, 10, 0)).unwrap();
        assert_eq!(day.date, civil::date(2026, 8, 19));
        assert_eq!(snap.used_today, DurationSpec::ZERO);
    }

    #[test]
    fn the_weekly_figure_starts_on_monday() {
        let s = Store::in_memory().unwrap();
        // Sunday the 16th is the previous week; Monday the 17th is this one.
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 16, 12, 0), 60))
            .unwrap();
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 17, 12, 0), 30))
            .unwrap();
        let (snap, _) = s.snapshot(&policy_2h(), &at(2026, 8, 19, 12, 0)).unwrap();
        assert_eq!(snap.used_this_week, DurationSpec::from_mins(30));
    }

    #[test]
    fn bonus_applies_to_one_policy_day_only() {
        let s = Store::in_memory().unwrap();
        let today = policy_day(&at(2026, 8, 19, 15, 0), civil::time(4, 0, 0, 0));
        s.add_bonus("kid", today, DurationSpec::from_mins(30), "mum")
            .unwrap();
        s.add_bonus("kid", today, DurationSpec::from_mins(15), "dad")
            .unwrap();
        assert_eq!(
            s.bonus_for("kid", today).unwrap(),
            DurationSpec::from_mins(45)
        );

        let tomorrow = policy_day(&at(2026, 8, 20, 15, 0), civil::time(4, 0, 0, 0));
        assert_eq!(s.bonus_for("kid", tomorrow).unwrap(), DurationSpec::ZERO);

        // Granting bonus is itself an auditable event.
        assert_eq!(s.event_count("kid", EventKind::BonusGranted).unwrap(), 2);
    }

    #[test]
    fn the_snapshot_feeds_the_engine_end_to_end() {
        let s = Store::in_memory().unwrap();
        let p = policy_2h();
        s.insert_segment(&seg("kid", "firefox", &at(2026, 8, 19, 15, 0), 120))
            .unwrap();

        let now = at(2026, 8, 19, 17, 0);
        let (snap, day) = s.snapshot(&p, &now).unwrap();
        assert!(
            !tb_core::evaluate(&p, &snap, &now).is_allowed(),
            "two hours used against a two hour quota must block"
        );

        // A parent grants half an hour; the very next evaluation lets the child in.
        s.add_bonus("kid", day, DurationSpec::from_mins(30), "mum")
            .unwrap();
        let (snap, _) = s.snapshot(&p, &now).unwrap();
        let verdict = tb_core::evaluate(&p, &snap, &now);
        assert!(verdict.is_allowed());
        assert_eq!(verdict.remaining(), Some(DurationSpec::from_mins(30)));
    }

    #[test]
    fn segments_can_be_read_back() {
        let s = Store::in_memory().unwrap();
        let g = seg("kid", "org.kde.konsole", &at(2026, 8, 19, 15, 0), 30);
        s.insert_segment(&g).unwrap();
        let back = s
            .segments_between(
                "kid",
                at(2026, 8, 19, 0, 0).timestamp(),
                at(2026, 8, 20, 0, 0).timestamp(),
            )
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, g.id);
        assert_eq!(back[0].app.as_str(), "org.kde.konsole");
        assert_eq!(back[0].source, AppIdSource::DesktopFile);
        assert_eq!(back[0].duration(), DurationSpec::from_mins(30));
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::SchemaTooNew { .. })
        ));
    }
}
