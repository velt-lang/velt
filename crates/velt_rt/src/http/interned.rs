//! Interned response header names and values, so a header every response carries (`server:
//! velt`, `cache-control: no-store`) costs no allocation or parsing per response.
//!
//! `HeaderValue::from_bytes` copies the value into a fresh buffer. Most values a server sets are
//! the same on every response, so each worker thread remembers the first [`SLOTS`] short values
//! it sees, leaked as `'static` bytes (a `HeaderValue` over static bytes clones for free and is
//! never freed). Leaking is bounded by [`LEAK_BUDGET`] bytes per process; once a thread's slots
//! or the budget are used up, other values take the copying path as before. Slots are never
//! evicted: the headers a server sets on every response are among the first it sees.
//!
//! Names are remembered the same way (the first [`SLOTS`] per thread, as written): parsing a
//! name (`HeaderName::from_bytes`) matches it against every standard header, and a custom one
//! is copied; a remembered name is a comparison and a clone. They are owned, not leaked, so
//! they need no budget.

use bytes::Bytes;
use hyper::header::{HeaderName, HeaderValue, InvalidHeaderName, InvalidHeaderValue};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Values remembered per worker thread.
const SLOTS: usize = 8;
/// Longest value worth interning (longer ones are rarely constant).
const MAX_LEN: usize = 64;
/// Total bytes all threads may leak.
const LEAK_BUDGET: usize = 16 * 1024;

static LEAKED: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static VALUES: RefCell<Vec<HeaderValue>> = const { RefCell::new(Vec::new()) };
    /// Names as written, with what they parse to.
    static NAMES: RefCell<Vec<(Box<[u8]>, HeaderName)>> = const { RefCell::new(Vec::new()) };
}

/// A header name for `bytes`: a clone of the name this thread parsed from the same bytes before,
/// else a newly parsed one (remembered if there is room).
pub fn header_name(bytes: &[u8]) -> Result<HeaderName, InvalidHeaderName> {
    if bytes.len() > MAX_LEN {
        return HeaderName::from_bytes(bytes);
    }
    NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some((_, name)) = names.iter().find(|(seen, _)| **seen == *bytes) {
            return Ok(name.clone());
        }
        let name = HeaderName::from_bytes(bytes)?;
        if names.len() < SLOTS {
            names.push((bytes.into(), name.clone()));
        }
        Ok(name)
    })
}

/// A header value for `bytes`: a free clone of an interned value when this thread has seen it
/// before, else a new value (interned if there is room).
pub fn header_value(bytes: &[u8]) -> Result<HeaderValue, InvalidHeaderValue> {
    if bytes.len() > MAX_LEN {
        return HeaderValue::from_bytes(bytes);
    }
    VALUES.with(|values| {
        let mut values = values.borrow_mut();
        if let Some(v) = values.iter().find(|v| v.as_bytes() == bytes) {
            return Ok(v.clone());
        }
        if values.len() >= SLOTS || !is_valid(bytes) || !reserve(bytes.len()) {
            return HeaderValue::from_bytes(bytes);
        }
        let leaked: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
        let v = HeaderValue::from_maybe_shared(Bytes::from_static(leaked))?;
        values.push(v.clone());
        Ok(v)
    })
}

/// What `HeaderValue::from_bytes` accepts (visible ASCII, spaces, tabs and obs-text), checked
/// before anything is leaked.
fn is_valid(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|&b| b == b'\t' || (b >= 0x20 && b != 0x7f))
}

/// Takes `len` bytes from the leak budget; false if it is used up.
fn reserve(len: usize) -> bool {
    LEAKED
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
            (used + len <= LEAK_BUDGET).then_some(used + len)
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_values_share_static_bytes() {
        let a = header_value(b"velt-test").expect("valid");
        let b = header_value(b"velt-test").expect("valid");
        assert_eq!(a, "velt-test");
        assert_eq!(a.as_bytes().as_ptr(), b.as_bytes().as_ptr());
    }

    #[test]
    fn names_are_remembered_as_written() {
        for _ in 0..2 {
            assert_eq!(header_name(b"X-Velt-Test").expect("valid"), "x-velt-test");
            assert_eq!(
                header_name(b"server").expect("valid"),
                hyper::header::SERVER
            );
        }
        assert!(header_name(b"bad name").is_err());
        assert!(
            header_name(b"bad name").is_err(),
            "an invalid name is not remembered"
        );
        let long = vec![b'x'; MAX_LEN + 1];
        assert_eq!(
            header_name(&long).expect("valid").as_str().len(),
            MAX_LEN + 1
        );
    }

    #[test]
    fn invalid_and_long_values() {
        assert!(header_value(b"bad\nvalue").is_err());
        let long = vec![b'x'; MAX_LEN + 1];
        assert_eq!(header_value(&long).expect("valid").as_bytes(), &long[..]);
    }
}
