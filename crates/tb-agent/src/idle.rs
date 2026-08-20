// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Idle detection through `ext-idle-notify-v1`.
//!
//! The obvious route — `org.freedesktop.ScreenSaver.GetSessionIdleTime` — is a
//! dead end on Wayland. KDE still exports the method and answers every call
//! with `org.freedesktop.DBus.Error.NotSupported`, which is worse than not
//! having it: code written against it compiles, runs, and silently never sees a
//! user go idle.
//!
//! `ext-idle-notify-v1` is the protocol that actually works, and it is a
//! Wayland protocol rather than a KDE one — KWin, Mutter and Sway all
//! implement it, so this file is not part of the desktop-specific surface.
//!
//! The protocol is event-driven: ask for a notification after N seconds of
//! inactivity, then receive `idled` and `resumed`. There is no "how long has it
//! been" query, so a short timeout is used and the elapsed time added to it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

/// How long the compositor waits before calling the user idle.
///
/// Short on purpose: the policy's real threshold is minutes and is applied by
/// the daemon. This only needs to be fine-grained enough that the reported
/// figure is useful, and coarse enough not to wake the process constantly.
const NOTIFY_AFTER: Duration = Duration::from_secs(10);

/// How long the user has been inactive, shared with the reporting loop.
#[derive(Debug, Clone, Default)]
pub struct IdleClock {
    inner: Arc<Mutex<Option<Instant>>>,
}

impl IdleClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn set_idle_since(&self, at: Option<Instant>) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = at;
        }
    }

    /// Seconds of inactivity, or zero while the user is active.
    #[must_use]
    pub fn idle_secs(&self, now: Instant) -> u64 {
        let Ok(guard) = self.inner.lock() else {
            return 0;
        };
        guard.map_or(0, |since| {
            NOTIFY_AFTER.as_secs() + now.duration_since(since).as_secs()
        })
    }
}

/// Wayland state for the idle listener.
struct Listener {
    clock: IdleClock,
    notifier: Option<ExtIdleNotifierV1>,
    seat: Option<wl_seat::WlSeat>,
    /// Set when the compositor does not offer the protocol, so the caller can
    /// say so once instead of reporting a permanently active user.
    supported: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Listener {
    fn event(
        this: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name, interface, ..
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "ext_idle_notifier_v1" => {
                this.notifier = Some(registry.bind::<ExtIdleNotifierV1, _, _>(name, 1, qh, ()));
                this.supported = true;
            }
            "wl_seat" => {
                this.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(name, 1, qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for Listener {
    fn event(
        this: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                tracing::debug!("compositor reports the user is idle");
                this.clock.set_idle_since(Some(Instant::now()));
            }
            ext_idle_notification_v1::Event::Resumed => {
                tracing::debug!("compositor reports the user is active again");
                this.clock.set_idle_since(None);
            }
            _ => {}
        }
    }
}

/// Runs the idle listener until the connection drops.
///
/// Blocking, so it belongs on its own thread. Returns an error if the
/// compositor does not offer the protocol — worth reporting rather than
/// silently pretending nobody is ever idle.
pub fn run(clock: IdleClock) -> anyhow::Result<()> {
    let connection =
        Connection::connect_to_env().map_err(|e| anyhow::anyhow!("no Wayland connection: {e}"))?;
    let display = connection.display();
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    display.get_registry(&qh, ());

    let mut listener = Listener {
        clock,
        notifier: None,
        seat: None,
        supported: false,
    };
    // One round trip is enough for the compositor to advertise its globals.
    queue.roundtrip(&mut listener)?;

    let (Some(notifier), Some(seat)) = (listener.notifier.clone(), listener.seat.clone()) else {
        anyhow::bail!(
            "this compositor does not offer ext-idle-notify-v1; \
             idle time cannot be measured and all session time will count"
        );
    };
    let timeout_ms = u32::try_from(NOTIFY_AFTER.as_millis()).unwrap_or(10_000);
    let _notification = notifier.get_idle_notification(timeout_ms, &seat, &qh, ());

    tracing::info!("watching for idle through ext-idle-notify-v1");
    loop {
        queue.blocking_dispatch(&mut listener)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_active_user_is_reported_as_not_idle() {
        let clock = IdleClock::new();
        assert_eq!(clock.idle_secs(Instant::now()), 0);
    }

    #[test]
    fn idle_time_includes_the_notification_threshold() {
        // The compositor only speaks once the timeout has already passed, so
        // reporting the elapsed time alone would understate it by that much
        // every time.
        let clock = IdleClock::new();
        let t0 = Instant::now();
        clock.set_idle_since(Some(t0));
        assert_eq!(clock.idle_secs(t0), NOTIFY_AFTER.as_secs());
        assert_eq!(
            clock.idle_secs(t0 + Duration::from_secs(50)),
            NOTIFY_AFTER.as_secs() + 50
        );
    }

    #[test]
    fn resuming_puts_the_clock_back_to_zero() {
        let clock = IdleClock::new();
        clock.set_idle_since(Some(Instant::now()));
        assert!(clock.idle_secs(Instant::now()) > 0);
        clock.set_idle_since(None);
        assert_eq!(clock.idle_secs(Instant::now()), 0);
    }

    #[test]
    fn the_clock_is_shared_between_threads() {
        // The listener runs on its own thread and the reporting loop reads it.
        let clock = IdleClock::new();
        let other = clock.clone();
        let t0 = Instant::now();
        std::thread::spawn(move || other.set_idle_since(Some(t0)))
            .join()
            .unwrap();
        assert_eq!(clock.idle_secs(t0), NOTIFY_AFTER.as_secs());
    }
}
