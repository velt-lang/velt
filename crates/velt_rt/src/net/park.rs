//! Idle streams parked for a later task: the keep-alive pool of the global `fetch`, whose client
//! is Velt code (std/fetch/client/conn.vlt). A stream parked under a key (an origin) is handed to
//! the next task that asks for that key, most recently parked first, which the server is least
//! likely to have closed meanwhile.
//!
//! The pool is runtime state rather than Velt state because it belongs to the whole process:
//! Velt module state would make every task that fetches take the module lock for its code
//! between `await`s, and a `shared` value in a static `Atomic` is never released at exit, which
//! the leak counters of the debug runtime see. Its entries are stream handles, so it owns no
//! Velt memory.
//!
//! A stream idle longer than [`CHECK_IDLE`] is checked before it is handed out: a read that finds
//! the end of the stream (the server closed it), unexpected bytes or an error closes it instead.
//! Streams idle longer than [`MAX_IDLE`] are closed when their key is next asked for, and a key
//! keeps at most [`MAX_PER_KEY`] streams.

use super::tcp::{velt_rt_tcp_close, StreamHandle, STREAMS};
use crate::str::VeltStr;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The most idle streams kept per key; more are closed.
const MAX_PER_KEY: usize = 512;
/// A stream idle this long is closed rather than reused.
const MAX_IDLE: Duration = Duration::from_secs(60);
/// A stream idle this long is checked before it is reused.
const CHECK_IDLE: Duration = Duration::from_secs(1);

/// FNV-1a over the key's bytes: keys are a handful of origins the program chose, so a seeded
/// hash's cost (SipHash, several hundred instructions per request) buys nothing.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type Parked = HashMap<Vec<u8>, Vec<(StreamHandle, Instant)>, BuildHasherDefault<Fnv>>;

static PARKED: Mutex<Option<Parked>> = Mutex::new(None);

fn parked() -> MutexGuard<'static, Option<Parked>> {
    PARKED.lock().unwrap_or_else(|e| e.into_inner())
}

/// `TcpStream.park(key)`: keeps idle stream `s` under `key` for a later `velt_rt_tcp_unpark`
/// (closed instead when the key holds [`MAX_PER_KEY`] streams already). The caller's handle is
/// given up: the stream is the pool's until it is taken back.
///
/// # Safety
/// `key` is a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_park(s: StreamHandle, key: *const VeltStr) {
    let key = (*key).as_bytes();
    let full = {
        let mut guard = parked();
        let map = guard.get_or_insert_with(Parked::default);
        let list = match map.get_mut(key) {
            Some(list) => list,
            None => map.entry(key.to_vec()).or_default(),
        };
        let full = list.len() >= MAX_PER_KEY;
        if !full {
            list.push((s, Instant::now()));
        }
        full
    };
    if full {
        velt_rt_tcp_close(s);
    }
}

/// `TcpStream.unpark(key)`: the most recently parked stream under `key` that is still usable,
/// or 0 when there is none. Streams found closed or idle too long are closed on the way.
///
/// # Safety
/// `key` is a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_unpark(key: *const VeltStr) -> StreamHandle {
    let key = (*key).as_bytes();
    loop {
        let mut stale = vec![];
        let found = {
            let mut guard = parked();
            let list = guard.as_mut().and_then(|map| map.get_mut(key));
            match list {
                None => None,
                Some(list) => {
                    let expired = list
                        .iter()
                        .take_while(|(_, since)| since.elapsed() > MAX_IDLE)
                        .count();
                    stale.extend(list.drain(..expired).map(|(s, _)| s));
                    list.pop()
                }
            }
        };
        // Closed with the lock let go: closing publishes this thread's output (a hand-off).
        for s in stale {
            velt_rt_tcp_close(s);
        }
        let Some((s, since)) = found else {
            return StreamHandle::NULL;
        };
        if since.elapsed() < CHECK_IDLE || still_open(s) {
            return s;
        }
        velt_rt_tcp_close(s);
    }
}

/// Whether idle stream `s` can carry another request: a read finds nothing yet (not the end of
/// the stream, nor bytes nobody asked for, nor an error).
fn still_open(s: StreamHandle) -> bool {
    let Some(obj) = STREAMS.get(s) else {
        return false;
    };
    let mut probe = [0u8; 1];
    matches!(obj.try_read_into(&mut probe), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> VeltStr {
        VeltStr::from_text(s)
    }

    #[test]
    fn unparks_what_was_parked_last_first() {
        let k = key("http://park-test:1");
        // Handles that are not open: parking keeps them as they are, and a fresh one is handed
        // back without a check.
        let (a, b) = (StreamHandle::from_bits(11), StreamHandle::from_bits(12));
        // SAFETY: valid strings.
        unsafe {
            assert_eq!(velt_rt_tcp_unpark(&k).bits(), 0);
            velt_rt_tcp_park(a, &k);
            velt_rt_tcp_park(b, &k);
            assert_eq!(velt_rt_tcp_unpark(&key("http://other:1")).bits(), 0);
            assert_eq!(velt_rt_tcp_unpark(&k).bits(), 12);
            assert_eq!(velt_rt_tcp_unpark(&k).bits(), 11);
            assert_eq!(velt_rt_tcp_unpark(&k).bits(), 0);
        }
    }

    #[test]
    fn a_closed_handle_is_not_open() {
        assert!(!still_open(StreamHandle::from_bits(99)));
    }
}
