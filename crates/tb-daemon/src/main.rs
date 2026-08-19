// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `timebanditsd` — the root service that measures screen time and enforces it.
//!
//! Everything that *decides* lives here. The session agent, the KWin script and
//! the plasmoid all run as the child and are treated as sources of hints, never
//! of authority.
//!
//! Two loops run side by side:
//!
//! * an async task serving the PAM socket, so logins and unlocks get an answer
//!   in milliseconds;
//! * a plain thread running the enforcement tick, which uses blocking D-Bus. One
//!   round trip every few seconds does not need a runtime, and the state machine
//!   is far easier to follow without one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use jiff::Zoned;
use tracing_subscriber::EnvFilter;

use tb_daemon::config::{self, Config};
use tb_daemon::logind::Logind;
use tb_daemon::pamserver;
use tb_daemon::store::Store;
use tb_daemon::tick::{LogNotifier, Ticker};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TIMEBANDITS_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // systemd-journald adds its own timestamps.
        .without_time()
        .init();

    let config_path = parse_config_path();
    let cfg =
        Config::load(&config_path).with_context(|| format!("reading {}", config_path.display()))?;

    if Config::enforcement_disabled() {
        tracing::warn!(
            flag = config::DISABLE_FLAG,
            "enforcement is disabled by the override file; observing only"
        );
    }

    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("creating {}", cfg.state_dir.display()))?;
    let db = cfg.database_path();
    let store = Arc::new(Mutex::new(
        Store::open(&db).with_context(|| format!("opening {}", db.display()))?,
    ));
    tracing::info!(database = %db.display(), "state loaded");

    let running = Arc::new(AtomicBool::new(true));
    let ticker = std::thread::Builder::new().name("tb-tick".into()).spawn({
        let (store, cfg, running) = (store.clone(), cfg.clone(), running.clone());
        move || run_ticks(&store, &cfg, &running)
    })?;

    // A single-threaded runtime is plenty for the socket: the workload is a
    // handful of round trips, and a small footprint matters on a machine a
    // child is actually using.
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(serve_pam(&cfg, store));

    running.store(false, Ordering::Relaxed);
    let _ = ticker.join();
    let _ = std::fs::remove_file(&cfg.pam_socket);
    result
}

async fn serve_pam(cfg: &Config, store: Arc<Mutex<Store>>) -> anyhow::Result<()> {
    let listener = pamserver::bind(&cfg.pam_socket)
        .with_context(|| format!("binding {}", cfg.pam_socket.display()))?;
    let pam = tokio::spawn(pamserver::serve(listener, pamserver::Responder::new(store)));

    tracing::info!("timebanditsd ready");
    shutdown_signal().await;
    tracing::info!("shutting down");
    pam.abort();
    Ok(())
}

/// The enforcement loop.
///
/// A missing logind is treated as a transient fault rather than a fatal one. It
/// should never happen on a systemd machine, but refusing to start would take
/// the PAM socket down with it — and an unreachable socket makes the module fail
/// closed, locking out the whole household over a D-Bus hiccup.
fn run_ticks(store: &Arc<Mutex<Store>>, cfg: &Config, running: &AtomicBool) {
    let interval = Duration::from_secs(cfg.tick_interval.as_secs().max(1));
    let mut ticker: Option<Ticker<Logind, LogNotifier>> = None;

    while running.load(Ordering::Relaxed) {
        if ticker.is_none() {
            match Logind::connect() {
                Ok(logind) => {
                    tracing::info!("connected to logind");
                    ticker = Some(Ticker::new(store.clone(), logind, LogNotifier, cfg));
                }
                Err(e) => {
                    tracing::error!(error = %e, "cannot reach logind; enforcement is degraded");
                }
            }
        }

        if let Some(t) = ticker.as_mut()
            && let Err(e) = t.tick(&Zoned::now())
        {
            tracing::error!(error = %e, "tick failed; reconnecting to logind");
            ticker = None;
        }

        // Sleep in slices so shutdown does not wait out a whole interval.
        let deadline = std::time::Instant::now() + interval;
        while running.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200).min(interval));
        }
    }

    // Persist the stretch that was in progress, so shutting the machine down
    // does not silently give back the last few minutes.
    if let Some(t) = ticker.as_mut() {
        t.flush(&Zoned::now());
        tracing::info!("final segments saved");
    }
}

/// Waits for whichever of SIGTERM or SIGINT arrives first.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen for SIGTERM");
            return;
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen for SIGINT");
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

/// `--config PATH`, defaulting to the packaged location.
fn parse_config_path() -> PathBuf {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                if let Some(p) = args.next() {
                    return PathBuf::from(p);
                }
            }
            other => {
                if let Some(p) = other.strip_prefix("--config=") {
                    return PathBuf::from(p);
                }
            }
        }
    }
    PathBuf::from(config::DEFAULT_CONFIG)
}
