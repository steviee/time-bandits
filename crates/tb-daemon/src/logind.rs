// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Session observation and control through `systemd-logind`.
//!
//! This is the daemon's authoritative view of who is logged in and its only way
//! to act on a session. It needs no cooperation from anything running as the
//! child, which is exactly why enforcement is built on it rather than on the
//! session agent.
//!
//! The blocking D-Bus API is used deliberately: the tick loop makes one round
//! trip every few seconds on its own thread, and the blocking client is far
//! easier to reason about than threading a runtime through the enforcement
//! state machine.

use std::collections::HashMap;

use zbus::zvariant::OwnedObjectPath;

/// What the daemon needs to know about one login session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub uid: u32,
    pub user: String,
    /// On the active virtual terminal. A session switched away from still
    /// exists but is not in front of anybody.
    pub active: bool,
    /// The screen lock is engaged.
    pub locked: bool,
    /// `user`, `greeter`, `lock-screen`, `background`, …
    pub class: String,
    /// `wayland`, `x11`, `tty`, …
    pub kind: String,
    pub remote: bool,
}

impl SessionInfo {
    /// Does this session mean the person is actually sitting in front of a
    /// desktop right now?
    ///
    /// The display manager's own greeter session runs as a system user and must
    /// never be counted or locked; a background session (`systemd --user`
    /// lingering after logout) is not somebody using the computer either.
    #[must_use]
    pub fn is_creditable_desktop(&self) -> bool {
        self.class == "user" && self.active && !self.locked && self.kind != "tty"
    }

    /// Should this session be locked when time runs out?
    ///
    /// Broader than [`Self::is_creditable_desktop`]: a session that is merely
    /// inactive (the child switched to another VT) still has to be locked, or
    /// switching back would be a way around the limit.
    #[must_use]
    pub fn is_lockable(&self) -> bool {
        self.class == "user" && !self.remote
    }
}

/// The operations the enforcement loop performs on sessions.
///
/// A trait so the state machine can be tested against scripted sessions instead
/// of a live login manager.
pub trait SessionControl {
    fn sessions(&self) -> anyhow::Result<Vec<SessionInfo>>;
    fn lock(&self, session_id: &str) -> anyhow::Result<()>;
    fn terminate(&self, session_id: &str) -> anyhow::Result<()>;
}

/// One row of `ListSessions`: session id, uid, user name, seat, object path.
type ListedSession = (String, u32, String, String, OwnedObjectPath);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1",
    gen_async = false
)]
trait Login1Manager {
    fn list_sessions(&self) -> zbus::Result<Vec<ListedSession>>;
    fn lock_session(&self, session_id: &str) -> zbus::Result<()>;
    fn terminate_session(&self, session_id: &str) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1",
    gen_async = false
)]
trait Login1Session {
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property, name = "LockedHint")]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn session_type(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
}

/// The live implementation, talking to logind on the system bus.
#[derive(Debug)]
pub struct Logind {
    connection: zbus::blocking::Connection,
}

impl Logind {
    pub fn connect() -> anyhow::Result<Self> {
        Ok(Self {
            connection: zbus::blocking::Connection::system()?,
        })
    }

    fn manager(&self) -> zbus::Result<Login1ManagerProxy<'_>> {
        Login1ManagerProxy::new(&self.connection)
    }
}

impl SessionControl for Logind {
    fn sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        let manager = self.manager()?;
        let mut out = Vec::new();
        for (id, uid, user, _seat, path) in manager.list_sessions()? {
            let session = Login1SessionProxy::builder(&self.connection)
                .path(path)?
                .build()?;
            // A session can disappear between listing and querying it — somebody
            // logging out at the wrong moment must not abort the whole tick.
            let (Ok(active), Ok(class)) = (session.active(), session.class()) else {
                tracing::debug!(%id, "session vanished while reading it");
                continue;
            };
            out.push(SessionInfo {
                id,
                uid,
                user,
                active,
                // A missing LockedHint means the desktop does not report lock
                // state. Treating that as "not locked" keeps the clock running,
                // which is the safe direction for a screen-time limit.
                locked: session.locked_hint().unwrap_or(false),
                class,
                kind: session.session_type().unwrap_or_default(),
                remote: session.remote().unwrap_or(false),
            });
        }
        Ok(out)
    }

    fn lock(&self, session_id: &str) -> anyhow::Result<()> {
        self.manager()?.lock_session(session_id)?;
        Ok(())
    }

    fn terminate(&self, session_id: &str) -> anyhow::Result<()> {
        self.manager()?.terminate_session(session_id)?;
        Ok(())
    }
}

/// Groups sessions by the user they belong to.
#[must_use]
pub fn by_user(sessions: Vec<SessionInfo>) -> HashMap<String, Vec<SessionInfo>> {
    let mut map: HashMap<String, Vec<SessionInfo>> = HashMap::new();
    for s in sessions {
        map.entry(s.user.clone()).or_default().push(s);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(user: &str, class: &str, active: bool, locked: bool) -> SessionInfo {
        SessionInfo {
            id: format!("{user}-1"),
            uid: 1000,
            user: user.to_owned(),
            active,
            locked,
            class: class.to_owned(),
            kind: "wayland".to_owned(),
            remote: false,
        }
    }

    #[test]
    fn an_active_unlocked_desktop_counts() {
        assert!(session("kid", "user", true, false).is_creditable_desktop());
    }

    #[test]
    fn a_locked_session_does_not_count() {
        assert!(!session("kid", "user", true, true).is_creditable_desktop());
    }

    #[test]
    fn a_session_on_another_vt_does_not_count() {
        assert!(!session("kid", "user", false, false).is_creditable_desktop());
    }

    #[test]
    fn the_greeter_is_never_counted_or_locked() {
        // sddm's own greeter is an active graphical session. Counting it would
        // charge a child for the login screen; locking it would make the machine
        // unusable for everybody.
        let greeter = session("sddm", "greeter", true, false);
        assert!(!greeter.is_creditable_desktop());
        assert!(!greeter.is_lockable());
    }

    #[test]
    fn a_background_session_does_not_count() {
        assert!(!session("kid", "background", true, false).is_creditable_desktop());
    }

    #[test]
    fn an_inactive_session_is_still_locked() {
        // Otherwise switching to another virtual terminal would park a session
        // out of reach of enforcement and switching back would restore it.
        let s = session("kid", "user", false, false);
        assert!(!s.is_creditable_desktop());
        assert!(s.is_lockable());
    }

    #[test]
    fn remote_sessions_are_left_alone() {
        let mut s = session("kid", "user", true, false);
        s.remote = true;
        assert!(!s.is_lockable(), "we do not manage ssh sessions this way");
    }

    /// Reads the real logind on this machine.
    ///
    /// Ignored by default because it needs a system bus and a live session, but
    /// it is the only thing that checks the D-Bus property mapping — a typo in a
    /// property name compiles perfectly and fails silently at runtime.
    ///
    /// Run with `cargo test -p tb-daemon -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a live system bus"]
    fn live_logind_reports_usable_sessions() {
        let logind = Logind::connect().expect("connect to the system bus");
        let sessions = logind.sessions().expect("list sessions");
        assert!(!sessions.is_empty(), "a machine with a login has sessions");

        for s in &sessions {
            println!(
                "{:<4} uid={:<6} user={:<10} class={:<8} type={:<8} active={} locked={} \
                 creditable={} lockable={}",
                s.id,
                s.uid,
                s.user,
                s.class,
                s.kind,
                s.active,
                s.locked,
                s.is_creditable_desktop(),
                s.is_lockable()
            );
            // Empty strings here mean a property name did not match and the
            // fallback silently produced a default.
            assert!(!s.id.is_empty(), "session id must be populated");
            assert!(!s.class.is_empty(), "class must be populated");
        }
        assert!(
            sessions.iter().any(|s| s.class == "user"),
            "at least one real user session expected"
        );
    }

    #[test]
    fn sessions_group_by_user() {
        let grouped = by_user(vec![
            session("kid", "user", true, false),
            session("kid", "user", false, false),
            session("sibling", "user", true, false),
        ]);
        assert_eq!(grouped["kid"].len(), 2);
        assert_eq!(grouped["sibling"].len(), 1);
    }
}
