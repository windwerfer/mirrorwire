//! One long-lived tokio runtime for every xdg-desktop-portal call (capture, remote input,
//! camera).
//!
//! ashpd keeps a single process-wide D-Bus session connection, and zbus ties a connection to
//! the runtime it was built on: its socket reader runs as a task there. Before, the capture
//! built a short-lived runtime on its own `portal` thread, so the first capture owned that
//! connection. When the capture was reopened (mode, cursor or frame-rate change) the thread and
//! its runtime ended, the reader task went with them, and every later portal call waited for
//! a reply forever: the new capture never started, the presenter process stayed alive with no
//! video, and only a restart helped (owner reports t316u/t318u). Measured on the owner's KDE
//! session from the dev container: after the first runtime was dropped, a plain
//! `AvailableSourceTypes` read hung (5 s timeout, no dialog involved). This runtime is never
//! dropped, so the connection lives as long as the process.

use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

/// The portal runtime (one worker thread, built on first use). Run portal futures with
/// `block_on` (from a plain thread, never from inside another runtime) or `spawn`.
pub fn portal_runtime() -> std::io::Result<&'static Runtime> {
    static RT: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("portal-rt")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| std::io::Error::other(format!("portal runtime: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The regression in one picture: a task the portal side started from a thread that has
    /// since finished (the old capture) must keep running, and a later caller on another
    /// thread (the reopened capture) must get the same runtime.
    #[test]
    fn the_portal_runtime_outlives_the_thread_that_first_used_it() {
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        let first = std::thread::spawn(|| {
            let rt = portal_runtime().unwrap();
            let handle = rt.handle().clone();
            // Like zbus's socket reader: spawned once, expected to live on.
            rt.block_on(async move {
                handle.spawn(async move {
                    for i in 0.. {
                        if tx.send(i).is_err() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                });
            });
            rt.handle().clone()
        })
        .join()
        .unwrap();
        // The first thread is gone; the background task still ticks.
        let seen_after = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let later = std::thread::spawn(|| portal_runtime().unwrap().handle().clone())
            .join()
            .unwrap();
        assert_eq!(first.id(), later.id());
        let next = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            next > seen_after,
            "the task stopped with the thread that started it"
        );
    }
}
