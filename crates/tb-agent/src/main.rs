// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `timebandits-agent` — the part that runs inside the child's session.
//!
//! It reports what only the session can see (which window has focus, how long
//! the user has been idle) and shows what the daemon decides. It holds no
//! authority whatsoever: killing it costs attribution and warnings, never
//! enforcement. The daemon notices the silence and says so.
//!
//! Three things run side by side:
//!
//! * a Wayland thread watching `ext-idle-notify-v1`,
//! * a session-bus interface the KWin script writes to and the widget reads,
//! * a loop that exchanges a report for the daemon's answer every few seconds.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tb_agent::client;
use tb_agent::dbus::{AgentInterface, BUS_NAME, OBJECT_PATH};
use tb_agent::idle::{self, IdleClock};
use tb_agent::state::{AgentState, Announcement};
use tb_proto::agent::Report;
use tracing_subscriber::EnvFilter;

/// How often the agent reports. Matches the daemon's default tick, so a report
/// is never more than one tick old when the daemon looks at it.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TIMEBANDITS_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .without_time()
        .init();

    let socket = std::env::var("TIMEBANDITS_AGENT_SOCKET").map_or_else(
        |_| PathBuf::from(tb_proto::agent::SOCKET_PATH),
        PathBuf::from,
    );

    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned());
    let state = Arc::new(Mutex::new(AgentState::new(user)));

    // Idle watching is blocking and belongs on its own thread. A compositor
    // without the protocol is a degradation, not a failure: without it every
    // session minute counts, which is the safe direction.
    let clock = IdleClock::new();
    std::thread::Builder::new()
        .name("tb-idle".into())
        .spawn({
            let clock = clock.clone();
            move || {
                if let Err(e) = idle::run(clock) {
                    tracing::warn!(error = %e, "idle detection unavailable; all session time will count");
                }
            }
        })?;

    let connection = zbus::connection::Builder::session()
        .context("connecting to the session bus")?
        .name(BUS_NAME)
        .context("claiming the bus name")?
        .serve_at(OBJECT_PATH, AgentInterface::new(state.clone()))
        .context("publishing the interface")?
        .build()
        .await
        .context("starting the session bus connection")?;
    tracing::info!(
        name = BUS_NAME,
        path = OBJECT_PATH,
        "listening on the session bus"
    );

    let iface = connection
        .object_server()
        .interface::<_, AgentInterface>(OBJECT_PATH)
        .await?;

    let mut ticker = tokio::time::interval(REPORT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _instant = ticker.tick() => {
                report_once(&socket, &state, &clock).await;
                // One announcement per round; the widget re-reads what it shows.
                iface.get().await.announce(iface.signal_emitter()).await;
            }
            () = shutdown_signal() => {
                tracing::info!("shutting down");
                return Ok(());
            }
        }
    }
}

/// One exchange with the daemon, and whatever the answer calls for.
async fn report_once(socket: &Path, state: &Arc<Mutex<AgentState>>, clock: &IdleClock) {
    let now = Instant::now();
    let report = {
        let Ok(state) = state.lock() else {
            return;
        };
        Report {
            focus: state.focus_for_report(now),
            idle_secs: clock.idle_secs(now),
            // The daemon cross-checks against logind, which it trusts more.
            locked: false,
            focus_tracking: state.focus_tracking(now),
            ..Report::new()
        }
    };

    match client::exchange(socket, &report).await {
        Ok(answer) => {
            let announcement = state
                .lock()
                .map_or(Announcement::Nothing, |mut s| s.ingest(answer));
            deliver(&announcement);
        }
        Err(e) => {
            // Expected while the daemon restarts. The widget shows "not
            // connected" rather than a stale countdown, because a stale
            // countdown is the one thing worse than no countdown.
            tracing::debug!(error = %e, "no answer from the daemon");
        }
    }
}

/// Puts an announcement on screen.
///
/// Desktop notifications are the one channel that does not depend on the widget
/// being in the panel, which is why the warnings go here rather than only into
/// the popup.
fn deliver(announcement: &Announcement) {
    match announcement {
        Announcement::Nothing => {}
        Announcement::Warning { remaining_secs } => {
            tracing::info!(remaining_secs, "warning the child");
        }
        Announcement::Blocked { message } => {
            tracing::info!(message, "telling the child they are blocked");
        }
        Announcement::Restored => {
            tracing::info!("telling the child their time is back");
        }
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
        return;
    };
    let Ok(mut int) = signal(SignalKind::interrupt()) else {
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
