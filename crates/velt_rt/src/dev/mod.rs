//! Dev mode: behavior under the `velt dev` supervisor (docs/internals/contracts/rt_abi_async.md §13).
//!
//! - `handover`: listening sockets come from the supervisor over the dev channel
//!   (`VELT_DEV_SOCKET`: a Unix socket, or a named pipe on Windows), so a restarted program
//!   serves the same socket and connections arriving during a reload wait in the kernel backlog
//!   instead of being refused.
//! - `shutdown`: a stop request (SIGTERM on Unix, `stop` on the dev channel on Windows) stops
//!   the HTTP accept loops, lets in-flight requests finish (bounded) and exits, so a reload does
//!   not cut requests off.
//! - HTTP servers re-read their handler descriptor per request (`http::handler`), the runtime
//!   hook for hot swap.
//!
//! Outside `velt dev` (variable not set) listeners bind directly and nothing else changes.

pub mod handover;
pub(crate) mod shutdown;

use std::io;

use tokio::net::TcpListener;

/// Names the supervisor's dev channel for listener handover.
pub const SOCKET_ENV: &str = "VELT_DEV_SOCKET";

/// Whether this process runs under `velt dev` (checked once).
pub(crate) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(SOCKET_ENV).is_some_and(|s| !s.is_empty()))
}

/// Bind a listener for `addr` (`host:port`) the way the current mode requires.
pub(crate) async fn bind(addr: &str) -> io::Result<TcpListener> {
    let Some(socket) = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty()) else {
        return TcpListener::bind(addr).await;
    };
    shutdown::install();
    let addr = addr.to_string();
    let std_listener = tokio::task::spawn_blocking(move || {
        // Before the first listener: once it is served, the supervisor may ask us to stop.
        #[cfg(windows)]
        shutdown::open_stop_channel(&socket);
        handover::request(&socket, &addr)
    })
    .await
    .map_err(io::Error::other)??;
    std_listener.set_nonblocking(true)?;
    TcpListener::from_std(std_listener)
}
