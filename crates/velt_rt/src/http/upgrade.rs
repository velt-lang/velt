//! Pending HTTP upgrades (WebSocket handshakes) between the request that asks for one and the
//! code that accepts it.
//!
//! hyper hands out an upgrade as an `OnUpgrade` taken from the request. The Velt handler only
//! sees the request through accessors, so the runtime parks the `OnUpgrade` here under a key the request
//! carries (`velt_rt_http_req_upgrade`); `velt_rt_http_take_upgrade` claims it and hands the
//! connection over as a std/net stream. Once the handler has produced its response, an unclaimed
//! entry is dropped, so nothing outlives its request.

use crate::net::stream::{Ready, StreamObj};
use crate::net::sys::TcpStream;
use crate::net::tcp::{StreamHandle, STREAMS};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn pending() -> MutexGuard<'static, HashMap<u64, OnUpgrade>> {
    static PENDING: OnceLock<Mutex<HashMap<u64, OnUpgrade>>> = OnceLock::new();
    PENDING
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Parks `on` and returns its key (never 0).
pub fn park(on: OnUpgrade) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let key = NEXT.fetch_add(1, Ordering::Relaxed);
    pending().insert(key, on);
    key
}

/// Claims the upgrade parked under `key` (once).
pub fn claim(key: u64) -> Option<OnUpgrade> {
    pending().remove(&key)
}

/// Drops the upgrade under `key` if nobody claimed it (0 = none).
pub fn release(key: u64) {
    if key != 0 {
        pending().remove(&key);
    }
}

/// The connection of the upgrade parked under `key`, as a stream: usable once the handler has
/// returned its `101` response (the first operation waits for that; what is written before goes
/// out right after the response). Over plain TCP it is the raw socket again, and the bytes the
/// HTTP server read past the request are delivered by the first read; over TLS it is the
/// upgraded connection. `None` if the upgrade was claimed already.
pub fn take(key: u64, peer: SocketAddr) -> Option<StreamHandle> {
    let on = claim(key)?;
    Some(STREAMS.insert(StreamObj::lazy(Box::pin(async move {
        let upgraded = on.await.map_err(|e| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("the upgrade did not happen (was the 101 response returned?): {e}"),
            )
        })?;
        type Plain = TokioIo<TcpStream>;
        Ok(
            match hyper_util::server::conn::auto::upgrade::downcast::<Plain>(upgraded) {
                Ok(parts) => Ready::tcp(parts.io.into_inner(), parts.read_buf.to_vec()),
                Err(upgraded) => Ready::other(Box::new(TokioIo::new(upgraded)), Some(peer)),
            },
        )
    }))))
}
