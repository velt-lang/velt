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
//! Outside `velt dev` (variable not set) listeners bind directly and nothing else changes.

pub mod handover;
pub(crate) mod shutdown;

use std::io;

use crate::net::sys::TcpListener;

/// Names the supervisor's dev channel for listener handover.
pub const SOCKET_ENV: &str = "VELT_DEV_SOCKET";

/// Bind a listener for `addr` (`host:port`) the way the current mode requires.
pub(crate) async fn bind(addr: &str) -> io::Result<TcpListener> {
    let Some(socket) = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty()) else {
        return bind_tcp(addr).await;
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

/// The listen backlog: the OS maximum, so a burst of connects waits in the queue instead of
/// being refused (#1019). Linux and macOS clamp it to `somaxconn`; Windows caps an explicit
/// number at 200 and takes `SOMAXCONN` (`0x7fffffff`) as "the provider's maximum".
#[cfg(windows)]
const BACKLOG: i32 = 0x7fff_ffff;
#[cfg(not(windows))]
const BACKLOG: i32 = 65_535;

/// `TcpListener::bind` with [`BACKLOG`] (tokio's uses 1024): the first of `addr`'s addresses
/// that binds.
async fn bind_tcp(addr: &str) -> io::Result<TcpListener> {
    let mut last = None;
    for a in tokio::net::lookup_host(addr).await? {
        match listen_on(a) {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

fn listen_on(a: std::net::SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::for_address(a), Type::STREAM, Some(Protocol::TCP))?;
    // As tokio and std do: a restarted server binds while old connections are in TIME_WAIT.
    #[cfg(not(windows))]
    s.set_reuse_address(true)?;
    s.set_nonblocking(true)?;
    s.bind(&a.into())?;
    s.listen(BACKLOG)?;
    TcpListener::from_std(s.into())
}
