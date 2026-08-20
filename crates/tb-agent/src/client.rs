// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Talking to the daemon over its agent socket.
//!
//! Unlike the PAM client, nothing here is in a login path, so a slow answer is
//! merely a stale widget. It still gets a timeout: an agent stuck on a socket
//! read stops reporting, and the daemon would then correctly conclude that
//! tracking has gone blind.

use std::path::Path;
use std::time::Duration;

use tb_proto::agent::{MAX_MESSAGE_BYTES, Report, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// How long to wait for the daemon before giving up on this round.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Sends one report and returns the daemon's view of this user.
pub async fn exchange(socket: &Path, report: &Report) -> anyhow::Result<State> {
    let work = async {
        let stream = UnixStream::connect(socket).await?;
        let (read_half, mut write_half) = stream.into_split();

        let mut line = serde_json::to_string(report)?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await?;

        let mut reader = BufReader::new(read_half).take(MAX_MESSAGE_BYTES as u64);
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        if response.trim().is_empty() {
            anyhow::bail!("daemon closed the connection without answering");
        }
        Ok::<State, anyhow::Error>(serde_json::from_str(response.trim())?)
    };

    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| anyhow::anyhow!("daemon did not answer within {TIMEOUT:?}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// A daemon that answers with `reply`, or hangs if there is none.
    fn serve(path: &Path, reply: Option<State>) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind");
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let (r, mut w) = stream.into_split();
                // Read the request first, as the daemon does. Answering before
                // reading drops the socket under the client's write.
                let mut line = String::new();
                let _ = BufReader::new(r).read_line(&mut line).await;
                if let Some(reply) = reply {
                    let mut out = serde_json::to_string(&reply).unwrap();
                    out.push('\n');
                    let _ = w.write_all(out.as_bytes()).await;
                    let _ = w.flush().await;
                } else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        })
    }

    #[tokio::test]
    async fn a_normal_exchange_returns_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let expected = State {
            enforcement: true,
            remaining_secs: Some(1800),
            ..State::unmanaged("kid")
        };
        let handle = serve(&path, Some(expected.clone()));

        let got = exchange(&path, &Report::new()).await.expect("a state");
        assert_eq!(got, expected);
        handle.abort();
    }

    #[tokio::test]
    async fn a_missing_daemon_is_an_error_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let err = exchange(&dir.path().join("nothing.sock"), &Report::new())
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("No such file"), "{err}");
    }

    #[tokio::test]
    async fn a_silent_daemon_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let handle = serve(&path, None);

        let started = std::time::Instant::now();
        let err = exchange(&path, &Report::new())
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("did not answer"), "{err}");
        assert!(
            started.elapsed() < TIMEOUT * 2,
            "took {:?}",
            started.elapsed()
        );
        handle.abort();
    }
}
