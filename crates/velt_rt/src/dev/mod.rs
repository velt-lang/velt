//! Dev mode: behavior under the `velt dev` supervisor (docs/internals/contracts/rt_abi_async.md §13).
//!
//! - `handover`: listening sockets come from the supervisor over the dev channel
//!   (`VELT_DEV_SOCKET`: a Unix socket, or a named pipe on Windows), so a restarted program
//!   serves the same socket and connections arriving during a reload wait in the kernel backlog
//!   instead of being refused.
//!
//! The graceful stop before the next version (SIGTERM on Unix, `stop` on the stop channel on
//! Windows) is std's: std/dev/stop.velt.
//!
//! Outside `velt dev` (variable not set) std binds its listeners itself and nothing else changes.

pub mod handover;

use std::io;

/// Names the supervisor's dev channel for listener handover.
pub const SOCKET_ENV: &str = "VELT_DEV_SOCKET";

/// The supervisor's listener for `addr` (`host:port`), non-blocking. Outside `velt dev` std
/// binds its own sockets and does not ask. Blocks for the round trip to the supervisor (a local
/// socket or pipe), once per listener.
pub(crate) async fn bind(addr: &str) -> io::Result<std::net::TcpListener> {
    let Some(socket) = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty()) else {
        return Err(io::ErrorKind::Unsupported.into());
    };
    let std_listener = handover::request(&socket, addr)?;
    std_listener.set_nonblocking(true)?;
    Ok(std_listener)
}
