//! Dev mode: behavior under the `velt dev` supervisor (docs/internals/contracts/rt_abi_async.md §13).
//!
//! - `handover`: listening sockets come from the supervisor over the dev channel
//!   (`VELT_DEV_SOCKET`: a Unix socket, or a named pipe on Windows), so a restarted program
//!   serves the same socket and connections arriving during a reload wait in the kernel backlog
//!   instead of being refused.
//! - `shutdown`: a stop request (SIGTERM on Unix, `stop` on the dev channel on Windows) stops
//!   the servers that joined (velt/dev's `joinStop`, through `velt_rt_dev_server_stop`), lets their
//!   in-flight requests finish (bounded) and exits, so a reload does not cut requests off.
//!
//! Outside `velt dev` (variable not set) std binds its listeners itself and nothing else changes.

pub mod handover;
pub(crate) mod shutdown;

use std::io;

/// Names the supervisor's dev channel for listener handover.
pub const SOCKET_ENV: &str = "VELT_DEV_SOCKET";

/// The supervisor's listener for `addr` (`host:port`), non-blocking. Outside `velt dev` std
/// binds its own sockets and does not ask.
pub(crate) async fn bind(addr: &str) -> io::Result<std::net::TcpListener> {
    let Some(socket) = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty()) else {
        return Err(io::ErrorKind::Unsupported.into());
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
    Ok(std_listener)
}
