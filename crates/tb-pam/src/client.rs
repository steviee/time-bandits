// SPDX-FileCopyrightText: 2026 Time Bandits contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Talking to `timebanditsd` from inside the login path.
//!
//! The hard requirement is that this returns within a bounded time, always.
//! A display manager that hangs because a background service is wedged is worse
//! than one that lets a child log in for another minute.
//!
//! `UnixStream::connect` can block if the daemon's accept backlog is full, and
//! a blocking `connect` on the calling thread cannot be interrupted. So the
//! whole exchange runs on a throwaway thread and the caller waits on a channel
//! with a deadline. If the deadline passes, the thread is abandoned; it either
//! finishes on its own moments later or dies with the process.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tb_proto::pam::{Answer, MAX_MESSAGE_BYTES, Query};

/// Why the daemon could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No socket, no listener, or the connection failed.
    Unreachable(String),
    /// The exchange did not finish within the deadline.
    TimedOut,
    /// The daemon answered with something we cannot parse.
    BadAnswer(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "daemon unreachable: {e}"),
            Self::TimedOut => f.write_str("daemon did not answer in time"),
            Self::BadAnswer(e) => write!(f, "malformed answer: {e}"),
        }
    }
}

/// Sends one query and waits at most `timeout` for the answer.
pub fn ask(socket: &Path, query: &Query, timeout: Duration) -> Result<Answer, ClientError> {
    let (tx, rx) = mpsc::channel();
    let socket: PathBuf = socket.to_path_buf();
    let line = match serde_json::to_string(query) {
        Ok(l) => l,
        Err(e) => return Err(ClientError::BadAnswer(e.to_string())),
    };

    // The thread is detached on purpose; see the module comment.
    thread::Builder::new()
        .name("tb-pam-query".into())
        .spawn(move || {
            let _ = tx.send(exchange(&socket, &line, timeout));
        })
        .map_err(|e| ClientError::Unreachable(e.to_string()))?;

    // A little headroom over the socket timeouts so the inner error, which is
    // more specific, usually wins the race against the outer deadline.
    match rx.recv_timeout(timeout + Duration::from_millis(50)) {
        Ok(result) => result,
        Err(_) => Err(ClientError::TimedOut),
    }
}

fn exchange(socket: &Path, line: &str, timeout: Duration) -> Result<Answer, ClientError> {
    let stream =
        UnixStream::connect(socket).map_err(|e| ClientError::Unreachable(e.to_string()))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| ClientError::Unreachable(e.to_string()))?;

    let mut writer = &stream;
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|e| ClientError::Unreachable(e.to_string()))?;

    let mut reader = BufReader::new(&stream).take(MAX_MESSAGE_BYTES as u64);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| ClientError::Unreachable(e.to_string()))?;
    if response.trim().is_empty() {
        return Err(ClientError::BadAnswer("empty response".into()));
    }
    serde_json::from_str(response.trim()).map_err(|e| ClientError::BadAnswer(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;
    use std::os::unix::net::UnixListener;
    use tb_proto::pam::{Decision, Phase};

    fn temp_socket(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("tb-pam-test-{name}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Runs a one-shot server that replies with `reply`, optionally after a delay.
    fn serve(path: &Path, reply: Option<String>, delay: Duration) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind");
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                thread::sleep(delay);
                if let Some(reply) = reply {
                    let mut w = BufWriter::new(&stream);
                    let _ = writeln!(w, "{reply}");
                    let _ = w.flush();
                }
            }
        })
    }

    #[test]
    fn a_normal_exchange_returns_the_answer() {
        let path = temp_socket("ok");
        let handle = serve(
            &path,
            Some(serde_json::to_string(&Answer::deny("out of time")).unwrap()),
            Duration::ZERO,
        );
        let answer = ask(
            &path,
            &Query::new("kid", "kde", Phase::Auth),
            Duration::from_millis(500),
        )
        .expect("answer");
        assert_eq!(answer.decision, Decision::Deny);
        assert_eq!(answer.message.as_deref(), Some("out of time"));
        let _ = handle.join();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_socket_is_reported_as_unreachable() {
        let path = temp_socket("missing");
        let err = ask(
            &path,
            &Query::new("kid", "sddm", Phase::Account),
            Duration::from_millis(200),
        )
        .expect_err("must fail");
        assert!(matches!(err, ClientError::Unreachable(_)), "got {err:?}");
    }

    #[test]
    fn a_silent_daemon_times_out_instead_of_hanging() {
        let path = temp_socket("slow");
        // Accepts the connection and then never answers.
        let handle = serve(&path, None, Duration::from_secs(30));
        let started = std::time::Instant::now();
        let err = ask(
            &path,
            &Query::new("kid", "sddm", Phase::Account),
            Duration::from_millis(150),
        )
        .expect_err("must fail");
        // The whole point: the login path is released quickly.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(err, ClientError::TimedOut | ClientError::Unreachable(_)),
            "got {err:?}"
        );
        drop(handle);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn garbage_is_not_mistaken_for_an_answer() {
        let path = temp_socket("garbage");
        let handle = serve(&path, Some("this is not json".to_owned()), Duration::ZERO);
        let err = ask(
            &path,
            &Query::new("kid", "kde", Phase::Auth),
            Duration::from_millis(500),
        )
        .expect_err("must fail");
        assert!(matches!(err, ClientError::BadAnswer(_)), "got {err:?}");
        let _ = handle.join();
        let _ = std::fs::remove_file(&path);
    }
}
