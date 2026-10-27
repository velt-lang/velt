//! `std/redis`: a Redis client (rt_abi_async.md §14.12). A crate on the native-package SDK only
//! (`velt_native`: the runtime's sockets, timers and tasks), linked into the runtime: its
//! `#[export]` functions are std's `velt_std_redis__*` declarations. Written directly over
//! tokio's I/O traits and the shared rustls configuration (`velt_tls`) rather than the `redis`
//! crate, whose rustls support always pulls in the platform certificate store (macOS Security
//! framework, which programs don't link) and builds its own `ClientConfig`. RESP2 is small.
//!
//! - `resp`: wire encoding and incremental reply parsing;
//! - `url`: `redis://` / `rediss://` URLs;
//! - `connect`: TCP/TLS and the `AUTH`/`SELECT` handshake;
//! - `multiplex`: one connection shared by all tasks, requests pipelined in order;
//! - `reply`: reply trees flattened into arrays for Velt;
//! - `client`: commands, pipelines and transactions on a client;
//! - `abi`: the exports;
//! - `pubsub`: pull-based subscribers (no stored callbacks, §13.5).

pub mod abi;
pub mod client;
pub mod connect;
pub mod error;
pub mod multiplex;
pub mod pubsub;
pub mod reply;
pub mod resp;
pub mod url;

use futures_util::future::{abortable, AbortHandle};
use std::future::Future;

/// Runs `f` as a task on the runtime's scheduler until it ends or the handle aborts it.
pub(crate) fn spawn_abortable<F: Future<Output = ()> + Send + 'static>(f: F) -> AbortHandle {
    let (f, handle) = abortable(f);
    velt_native::rt::spawn(async move {
        let _ = f.await;
    });
    handle
}
