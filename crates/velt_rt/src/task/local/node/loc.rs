//! Where a started promise's error was thrown, kept with the promise (panic.rs `ThrowLoc`).
//!
//! The location of a `throw` is recorded per thread, and a started node finishes on the thread
//! of whichever poll of its task ran it: its owner may claim the result in a later poll, on
//! another worker, or in another task (`spawn(use(p))`), and an unhandled rejection is reported
//! at a checkpoint of the task that drove it. So each poll of a node starts with no location,
//! and a node that finishes with one keeps it here; whoever then claims or reports the result
//! makes it the location of the error it is propagating.
//!
//! Rejections are rare, and a node's head has no room left (node/mod.rs), so the locations live
//! in a side table by node address, and a flag bit ([`super::LOC`]) says that a node has an
//! entry: a node that throws nothing pays two thread-local accesses per poll.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;

use super::{head, state, LOC, REJECTABLE};
use crate::panic::ThrowLoc;
use crate::task::VeltFut;

const SHARDS: usize = 16;

/// Locations of the nodes with [`LOC`], by address.
static LOCS: [Mutex<Option<HashMap<usize, ThrowLoc>>>; SHARDS] =
    [const { Mutex::new(None) }; SHARDS];

fn shard(f: *mut VeltFut) -> &'static Mutex<Option<HashMap<usize, ThrowLoc>>> {
    // Nodes are 16-aligned: the bits above the alignment spread them.
    &LOCS[(f as usize >> 4) % SHARDS]
}

/// Run `poll`, a poll of started node `f`'s state on the current thread, starting with no
/// location; the caller's location is back in place afterwards. When `poll` says the state
/// finished with a location (an error the node threw, not one it caught and resolved anyway),
/// the node keeps it.
#[inline]
pub(in crate::task::local) unsafe fn polled(f: *mut VeltFut, poll: impl FnOnce() -> bool) -> bool {
    let own = ThrowLoc::take();
    let done = poll();
    let mine = own.replace();
    if done && mine.is_known() {
        keep(f, mine);
    }
    done
}

/// Node `f` finished with location `loc` on this thread: keep it if its result can carry an
/// error (published before `DONE`, which the owner reads first).
#[cold]
pub(in crate::task::local) unsafe fn keep(f: *mut VeltFut, loc: ThrowLoc) {
    let h = head(f);
    let fl = h.flags.load(Ordering::Relaxed);
    // A promise that can reject and resolved threw nothing that leaves it.
    if fl & REJECTABLE != 0 && *state(f) == 0 {
        return;
    }
    shard(f)
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(f as usize, loc);
    h.flags.fetch_or(LOC, Ordering::Release);
}

/// The location node `f` keeps (unknown if none).
pub(in crate::task::local) unsafe fn of(f: *mut VeltFut) -> ThrowLoc {
    if head(f).flags.load(Ordering::Acquire) & LOC == 0 {
        return ThrowLoc::NONE;
    }
    (shard(f).lock().as_ref())
        .and_then(|m| m.get(&(f as usize)).copied())
        .unwrap_or(ThrowLoc::NONE)
}

/// The owner claims finished node `f`'s result: the location of its error is this thread's.
#[inline]
pub(in crate::task::local) unsafe fn claim(f: *mut VeltFut) {
    if head(f).flags.load(Ordering::Acquire) & LOC != 0 {
        of(f).restore();
    }
}

/// Run `d`, which disposes of node `f`'s unclaimed result (and may report it as an unhandled
/// rejection), with the node's location; the caller's is back in place afterwards.
#[inline]
pub(in crate::task::local) unsafe fn with_loc(f: *mut VeltFut, d: impl FnOnce()) {
    if head(f).flags.load(Ordering::Acquire) & LOC == 0 {
        return d();
    }
    let own = of(f).replace();
    d();
    own.restore();
}

/// Node `f` is freed: forget its location.
#[inline]
pub(in crate::task::local) unsafe fn forget(f: *mut VeltFut, flags: u32) {
    if flags & LOC != 0 {
        if let Some(m) = shard(f).lock().as_mut() {
            m.remove(&(f as usize));
        }
    }
}
