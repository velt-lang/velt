//! `std/redis`: a Redis client (rt_abi_async.md §14.12), written directly over tokio and the
//! shared rustls configuration rather than the `redis` crate, whose rustls support always pulls
//! in the platform certificate store (macOS Security framework, which programs don't link) and
//! builds its own `ClientConfig`. RESP2 is small; owning it keeps the runtime's dependencies and
//! TLS setup (ring, webpki roots + extra PEM CAs) in one place.
//!
//! - `resp`: wire encoding and incremental reply parsing;
//! - `url`: `redis://` / `rediss://` URLs;
//! - `connect`: TCP/TLS and the `AUTH`/`SELECT` handshake;
//! - `multiplex`: one connection shared by all tasks, requests pipelined in order;
//! - `reply`: reply trees flattened into arrays for Velt;
//! - `client`: exported command, pipeline and transaction functions;
//! - `pubsub`: pull-based subscribers (no stored callbacks, §13.5).

pub mod client;
pub mod connect;
pub mod error;
pub mod multiplex;
pub mod pubsub;
pub mod reply;
pub mod resp;
pub mod url;

use crate::str_array::VeltStrArray;
use std::borrow::Cow;

/// The elements of a Velt `string[]` as UTF-8, borrowed for the duration of the ABI call
/// (encode or copy them before returning); a string with lone surrogates is converted (one
/// U+FFFD each, #377).
///
/// # Safety
/// `a` must point to a valid `VeltStrArray` that outlives the returned slices.
pub(crate) unsafe fn str_args<'a>(a: *const VeltStrArray) -> Vec<Cow<'a, [u8]>> {
    let a = &*a;
    if a.len == 0 {
        return vec![];
    }
    std::slice::from_raw_parts(a.ptr, a.len as usize)
        .iter()
        .map(|s| match s.text_lossy() {
            Cow::Borrowed(t) => Cow::Borrowed(t.as_bytes()),
            Cow::Owned(t) => Cow::Owned(t.into_bytes()),
        })
        .collect()
}
