//! Graceful stop in dev mode: the supervisor asks the program to stop before starting the next
//! version (SIGTERM on Unix; on Windows, which has no SIGTERM, `stop` on the stop channel opened
//! over the dev channel). HTTP servers stop accepting (the supervisor keeps the listening socket,
//! so new connections queue for the next version), finish their in-flight requests for up to
//! [`DRAIN`], and then the process flushes stdout and exits with status 0.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Once, OnceLock};
use std::time::Duration;

use tokio::sync::watch;

/// Longest time in-flight requests get to finish after a stop request.
pub(crate) const DRAIN: Duration = Duration::from_secs(1);

static INSTALL: Once = Once::new();
/// `true` once a stop request arrived; `None` outside dev mode.
static STOP: OnceLock<watch::Sender<bool>> = OnceLock::new();
/// Servers that have not finished draining yet.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Start the stop watcher (once per process). Must run inside the runtime.
pub(crate) fn install() {
    INSTALL.call_once(|| {
        let (tx, _) = watch::channel(false);
        let _ = STOP.set(tx);
        tokio::spawn(watch_stop());
    });
}

/// Wait for a stop request, then drain every server and exit.
async fn watch_stop() {
    stop_requested().await;
    if let Some(stop) = STOP.get() {
        stop.send_replace(true);
    }
    // Every server drains within DRAIN; the margin covers scheduling.
    let all_drained = async {
        while ACTIVE.load(Ordering::Acquire) > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let _ = tokio::time::timeout(DRAIN + Duration::from_millis(200), all_drained).await;
    crate::io::flush_stdout();
    std::process::exit(0);
}

#[cfg(unix)]
async fn stop_requested() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            term.recv().await;
        }
        // Without a handler SIGTERM keeps its default action (the program just ends).
        Err(_) => std::future::pending().await,
    }
}

/// Fired by the stop-channel thread (Windows).
#[cfg(windows)]
static STOP_MESSAGE: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[cfg(windows)]
async fn stop_requested() {
    STOP_MESSAGE.notified().await;
}

/// Open the stop channel to the supervisor at `socket` (once per process; blocking). A thread
/// waits on it: `stop`, or the supervisor going away, starts the graceful stop. If it cannot be
/// opened the supervisor kills this program instead of asking it to stop.
#[cfg(windows)]
pub(crate) fn open_stop_channel(socket: &std::ffi::OsStr) {
    static OPEN: Once = Once::new();
    OPEN.call_once(|| {
        let Ok(stream) = super::handover::watch_stop(socket) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("velt-dev-stop".into())
            .spawn(move || {
                super::handover::wait_stop(&stream);
                STOP_MESSAGE.notify_one();
            });
    });
}

/// A server's registration: [`Stop::requested`] resolves on a stop request; dropping it reports
/// the server as drained.
pub(crate) struct Stop {
    rx: watch::Receiver<bool>,
}

/// Register a server with the dev-mode shutdown, or `None` outside dev mode.
pub(crate) fn register() -> Option<Stop> {
    let rx = STOP.get()?.subscribe();
    ACTIVE.fetch_add(1, Ordering::AcqRel);
    Some(Stop { rx })
}

impl Stop {
    /// Resolves once a stop request arrived.
    pub(crate) async fn requested(&mut self) {
        let _ = self.rx.wait_for(|stop| *stop).await;
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
