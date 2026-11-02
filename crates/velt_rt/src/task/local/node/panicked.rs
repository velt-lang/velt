//! Promise nodes that catch Velt panics (`catchPanic` in `velt/task`, panic.rs, #2008).
//!
//! A node catches a panic at its poll when it is marked [`CATCHES`] (the promise `catchPanic`
//! guards, and every promise started while a catching poll runs) or when a poll up the stack
//! catches: a panic then never unwinds through the set's bookkeeping around a poll (the running
//! frame, the adoption count), only through compiled frames and runtime functions that hold
//! nothing. The node becomes [`PANICKED`]: it counts as finished, its result slot holds nothing
//! and is never read, transferred or dropped, and its state is leaked (the compiled frames that
//! were running hold no landing pads, so whatever they had taken is leaked with it, and a lock
//! they held stays locked). The node itself is never freed: a runtime leaf inside the leaked
//! state (a timer, a socket wait) may still wake it, which then finds a finished node.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use super::{head, retain, CATCHES, PANICKED, REJECTABLE};
use crate::task::VeltFut;

/// The messages of the nodes that panicked, by address (they are never freed).
static MESSAGES: Mutex<Option<HashMap<usize, Box<[u8]>>>> = Mutex::new(None);

/// Poll node `f` with `poll` (true: finished). A Velt panic out of it marks `f` panicked, which
/// counts as finished; nothing is caught when neither `f` nor a poll up the stack catches.
#[inline(always)]
pub(in crate::task::local) unsafe fn caught(f: *mut VeltFut, poll: impl FnOnce() -> bool) -> bool {
    // A program that never arms `catchPanic` pays one load of a global flag here.
    if !crate::panic::may_catch()
        || (head(f).flags.load(Ordering::Relaxed) & CATCHES == 0 && !crate::panic::catching())
    {
        return poll();
    }
    let _catching = crate::panic::Catching::enter();
    match crate::panic::catch(poll) {
        Ok(done) => done,
        Err(msg) => {
            mark(f, msg);
            true
        }
    }
}

/// Node `f` catches panics from now on (before it starts).
pub(in crate::task::local) unsafe fn catch_panics(f: *mut VeltFut) {
    head(f).flags.fetch_or(CATCHES, Ordering::Relaxed);
}

/// A node started now inherits a catching poll up the stack.
pub(in crate::task::local) unsafe fn catches_here(f: *mut VeltFut) {
    if crate::panic::may_catch() && crate::panic::catching() {
        catch_panics(f);
    }
}

/// Did node `f` panic?
pub(in crate::task::local) unsafe fn is_panicked(f: *mut VeltFut) -> bool {
    head(f).flags.load(Ordering::Acquire) & PANICKED != 0
}

#[cold]
unsafe fn mark(f: *mut VeltFut, msg: Box<[u8]>) {
    let h = head(f);
    // Nothing reads the result slot: no rejection to report, no result to drop or transfer.
    h.result_drop.store(std::ptr::null_mut(), Ordering::Release);
    h.transfer.store(std::ptr::null_mut(), Ordering::Release);
    h.flags.fetch_and(!REJECTABLE, Ordering::Relaxed);
    // Never freed (the module comment says why).
    retain(f);
    crate::panic::count_caught();
    lock()
        .get_or_insert_with(HashMap::new)
        .insert(f as usize, msg);
    h.flags.fetch_or(PANICKED, Ordering::Release);
}

fn lock() -> std::sync::MutexGuard<'static, Option<HashMap<usize, Box<[u8]>>>> {
    MESSAGES.lock().unwrap_or_else(|e| e.into_inner())
}

/// The message of panicked node `f` (empty if it did not panic).
pub(in crate::task::local) unsafe fn panic_message(f: *mut VeltFut) -> Box<[u8]> {
    lock()
        .as_ref()
        .and_then(|m| m.get(&(f as usize)).cloned())
        .unwrap_or_default()
}

/// The owner awaits panicked node `f`: the await panics with its message.
#[cold]
pub(in crate::task::local) unsafe fn raise(f: *mut VeltFut) -> ! {
    crate::panic::raise(&panic_message(f))
}
