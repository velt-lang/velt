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
//! Every stream is checked before it is handed out: a look at the socket that finds the end of
//! the stream (the server closed it), unexpected bytes or an error closes it instead. A server
//! that closes a connection right after its response sends the end a moment after the response,
//! which a check right away cannot see yet: a request that may not be sent twice (a POST) is
//! handed only a stream idle for at least [`SETTLED`], by when such an end has usually arrived;
//! the newer streams stay parked (it gets a new connection if there is none). This narrows the
//! race, it does not rule it out: a server that closes later than that, or a loaded one, can still
//! close a stream as a POST goes out on it, which then fails after reaching the server (the
//! client never sends it again, as Node's does not; Node waits two event-loop turns before reusing
//! a socket and has the same residual race). The rule is time, not turns of
//! the event loop as in undici: a Velt request takes tens of microseconds, so a turn (a yield) is
//! far too short for a close a server sends on its own next turn or a timer later, which Node's
//! slower requests happen to outwait. Against a server farther away than [`SETTLED`], every
//! parked stream is older, so this costs nothing; against a fast local one, a tight loop of POSTs
//! keeps more connections (about one per [`SETTLED`] of requests). Streams idle longer than [`MAX_IDLE`] are closed
//! when their key is next asked for, and a key keeps at most [`MAX_PER_KEY`] streams.

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
/// A stream idle this long has usually received a close its server sent shortly after the
/// response: at once, on its next event-loop turn or a timer of a few milliseconds (Windows'
/// timers among them), which 1 ms did not cover.
const SETTLED: Duration = Duration::from_millis(5);

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

/// `TcpStream.unpark(key, settled)`: the most recently parked stream under `key` that is still
/// usable (with `settled` (not 0), the most recent one idle for at least [`SETTLED`]), or 0 when there is
/// none. Streams found closed or idle too long are closed on the way.
///
/// # Safety
/// `key` is a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_unpark(key: *const VeltStr, settled: u32) -> StreamHandle {
    let key = (*key).as_bytes();
    let settled = settled != 0;
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
                    pick(list, settled, Instant::now()).map(|i| list.remove(i))
                }
            }
        };
        // Closed with the lock let go: closing publishes this thread's output (a hand-off).
        for s in stale {
            velt_rt_tcp_close(s);
        }
        let Some((s, _)) = found else {
            return StreamHandle::NULL;
        };
        if still_open(s) {
            return s;
        }
        velt_rt_tcp_close(s);
    }
}

/// The stream of `list` (newest last) to hand out at `now`: the newest, or with `settled` the
/// newest parked at least [`SETTLED`] before `now`.
fn pick(list: &[(StreamHandle, Instant)], settled: bool, now: Instant) -> Option<usize> {
    if settled {
        list.iter()
            .rposition(|(_, since)| now.saturating_duration_since(*since) >= SETTLED)
    } else {
        list.len().checked_sub(1)
    }
}

/// Whether idle stream `s` can carry another request: a read finds nothing yet (not the end of
/// the stream, nor bytes nobody asked for, nor an error).
fn still_open(s: StreamHandle) -> bool {
    STREAMS.get(s).is_some_and(|obj| obj.idle_open())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> VeltStr {
        VeltStr::from_text(s)
    }

    #[test]
    fn a_stream_that_is_not_open_is_never_handed_out() {
        let k = key("http://park-test:1");
        // Handles that are not open: parked, then found closed when asked for, however
        // recently they were parked.
        let (a, b) = (StreamHandle::from_bits(11), StreamHandle::from_bits(12));
        // SAFETY: valid strings.
        unsafe {
            assert_eq!(velt_rt_tcp_unpark(&k, 0).bits(), 0);
            velt_rt_tcp_park(a, &k);
            velt_rt_tcp_park(b, &k);
            assert_eq!(velt_rt_tcp_unpark(&key("http://other:1"), 0).bits(), 0);
            assert_eq!(velt_rt_tcp_unpark(&k, 0).bits(), 0);
        }
    }

    #[test]
    fn a_settled_request_skips_streams_parked_just_now() {
        let t0 = Instant::now();
        let h = StreamHandle::from_bits;
        let list = [(h(1), t0), (h(2), t0 + SETTLED), (h(3), t0 + SETTLED * 3)];
        let now = t0 + SETTLED * 3;
        assert_eq!(pick(&list, false, now), Some(2));
        // 3 was parked just now, 2 one interval ago: the newest settled one.
        assert_eq!(pick(&list, true, now), Some(1));
        assert_eq!(pick(&list[2..], true, now), None);
        assert_eq!(pick(&[], false, now), None);
    }

    #[test]
    fn a_closed_handle_is_not_open() {
        assert!(!still_open(StreamHandle::from_bits(99)));
    }
}
