//! `std/http` on hyper 1.x: a server whose per-request handler is a compiled async function
//! (`server.rs`, descriptor in `handler.rs`), request accessors (`request.rs`), a response builder
//! (`response.rs`, constant header values interned by `interned.rs`) whose body is complete or
//! streamed (`body.rs`, `stream.rs`) and a minimal `fetch` client (`client.rs`).
//!
//! Connections are served with hyper-util's auto builder: HTTP/1.1 with keep-alive and upgrades
//! (`upgrade.rs`), and HTTP/2 (prior knowledge, h2c, or ALPN when serving TLS) on the same port.

pub mod body;
pub mod client;
pub mod handler;
mod interned;
pub mod request;
pub mod response;
pub mod server;
pub mod stream;
pub mod upgrade;

#[cfg(test)]
mod stream_tests;

use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use bytes::Bytes;

/// Take a `u8[]` argument's buffer without copying when it is owned (`cap > 0`); the caller's
/// value is left empty. Static/borrowed bytes are copied.
///
/// # Safety
/// `b` must point to a valid `VeltBytes`.
pub(crate) unsafe fn take_bytes(b: *mut VeltBytes) -> Bytes {
    Bytes::from((*b).take_vec())
}

/// Take a string argument (the caller's value is left empty): a well-formed heap string's
/// buffer becomes the body without copying (the `Bytes` holds the reference); short and static
/// text is copied, and text with lone surrogates is converted (one U+FFFD each, #377).
///
/// # Safety
/// `s` must point to a valid `VeltStr`.
pub(crate) unsafe fn take_text(s: *mut VeltStr) -> Bytes {
    let mut st = std::ptr::replace(s, VeltStr::empty());
    if !st.is_well_formed() {
        let body = Bytes::from(st.to_string_lossy().into_bytes());
        st.release();
        return body;
    }
    if st.is_heap() {
        return Bytes::from_owner(StrOwner(st));
    }
    Bytes::copy_from_slice(st.as_bytes())
}

/// A heap string owned by a `Bytes` (released when the last `Bytes` view goes).
struct StrOwner(VeltStr);

impl AsRef<[u8]> for StrOwner {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: a valid string moved out of generated code.
        unsafe { self.0.as_bytes() }
    }
}

impl Drop for StrOwner {
    fn drop(&mut self) {
        // SAFETY: this owner holds one reference.
        unsafe { self.0.release() }
    }
}

/// Owned `VeltStr` copy of text.
pub(crate) fn owned_str(s: &str) -> VeltStr {
    VeltStr::from_bytes(s.as_bytes())
}
