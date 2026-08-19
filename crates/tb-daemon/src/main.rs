// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `timebanditsd` — the root service that measures screen time and enforces it.
//!
//! Everything that *decides* lives here. The session agent, the KWin script and
//! the plasmoid all run as the child and are treated as sources of hints, never
//! of authority.
//!
//! At this stage the daemon serves the PAM socket from the stored policy and
//! usage. The tick loop, logind enforcement and hub synchronisation follow.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use tb_daemon::config::{self, Config};
use tb_daemon::pamserver;
use tb_daemon::store::Store;

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
            "enforcement is disabled by the override file; running in observe-only mode"
        );
    }

    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("creating {}", cfg.state_dir.display()))?;
    let db = cfg.database_path();
    let store = Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
    tracing::info!(database = %db.display(), "state loaded");

    // A single-threaded runtime is plenty: the workload is a handful of socket
    // round-trips and one timer. It also keeps the memory footprint small enough
    // to be unremarkable on a machine a child is using.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(cfg, store))
}

async fn run(cfg: Config, store: Store) -> anyhow::Result<()> {
    let store = Arc::new(Mutex::new(store));
    let listener = pamserver::bind(&cfg.pam_socket)
        .with_context(|| format!("binding {}", cfg.pam_socket.display()))?;

    let responder = pamserver::Responder::new(store);
    let pam = tokio::spawn(pamserver::serve(listener, responder));

    tracing::info!("timebanditsd ready");
    shutdown_signal().await;
    tracing::info!("shutting down");

    pam.abort();
    // The socket is in /run and would otherwise linger until reboot, making the
    // next start log a spurious "stale socket" warning.
    let _ = std::fs::remove_file(&cfg.pam_socket);
    Ok(())
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
