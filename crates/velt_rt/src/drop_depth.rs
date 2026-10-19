//! Bounded nesting of drops (#543): dropping an object whose fields lead back to objects of its
//! own kind (a list through an array, a `Map` or a closure, a tree of a class hierarchy) would
//! nest one call per level and overflow the stack on a long chain. The compiler counts nested
//! drops in a per-thread state word whose address `velt_rt_drop_state` gives, inline: past
//! [`MAX_DEPTH`] it `queue`s what it was about to drop instead (a value that lives in a slot
//! about to be freed, such as an array element, moved to a heap box first), and the outermost
//! drop, finding [`QUEUED`] set when it ends, calls `drain`, which drops the queued values, each
//! with the full depth again. Values up to the limit drop exactly as nested calls would; deeper
//! ones drop after the outermost drop's other work, in the order they were reached.
//!
//! The state word (`u32`): bits 0..30 count the drops under way, [`QUEUED`] says something was
//! queued since the last drain, [`DRAINING`] that the outermost drop is draining (so a drop
//! ending inside it never drains itself). Generated code, per bracketed drop:
//!
//! ```text
//! n = *state; if n & DEPTH >= MAX_DEPTH { queue(value, drop); return }
//! *state = n + 1; <drop the value>; m = *state - 1; *state = m; if m == QUEUED { drain() }
//! ```

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/// Drop glue that takes over a queued value: called with the queued pointer.
type DropFn = unsafe extern "C" fn(*mut u8);

/// Nested drops a thread runs before it queues the next one (the compiler's
/// `velt_vir/src/lower/glue/drop_depth.rs` uses the same number). A level is a few small frames
/// of drop glue (plus whatever a `[Symbol.dispose]()` hook calls), so this stays far below the
/// smallest stack a program runs on (1 MiB: the main thread on Windows; WebAssembly modules are
/// linked with 8 MiB).
pub const MAX_DEPTH: u32 = 128;

/// State word: the outermost drop is draining the queue.
pub const DRAINING: u32 = 1 << 31;
/// State word: something was queued since the queue was last drained.
pub const QUEUED: u32 = 1 << 30;
/// State word: the drops under way.
pub const DEPTH: u32 = QUEUED - 1;

/// Capacity kept for the next drop that queues values.
const KEEP: usize = 1024;

thread_local! {
    /// This thread's state word (module docs).
    static STATE: Cell<u32> = const { Cell::new(0) };
    /// Values queued by `queue`, dropped by `drain`.
    static QUEUE: RefCell<VecDeque<(usize, DropFn)>> = const { RefCell::new(VecDeque::new()) };
}

/// The address of this thread's state word, which generated code reads and writes in place.
#[no_mangle]
pub extern "C" fn velt_rt_drop_state() -> *mut u32 {
    STATE.with(Cell::as_ptr)
}

/// Drop `value` with `drop` (drop glue that takes it over) when the outermost drop ends; at
/// once while the thread is being torn down (its queue is gone).
///
/// # Safety
/// `drop` must take over `value`: the compiler queues only values whose last reference it was
/// dropping.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_drop_queue(value: *mut u8, drop: DropFn) {
    let queued = QUEUE.try_with(|q| q.borrow_mut().push_back((value as usize, drop)));
    if queued.is_err() {
        return drop(value);
    }
    STATE.with(|s| s.set(s.get() | QUEUED));
}

/// Drop the queued values (which may queue more): called by the outermost drop when it ends
/// with [`QUEUED`] set.
///
/// # Safety
/// Every queued value must still be owned by the queue (see `velt_rt_drop_queue`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_drop_drain() {
    STATE.with(|s| s.set(DRAINING));
    // The drop glue runs with nothing borrowed: it queues values of its own.
    while let Some((value, drop)) = QUEUE
        .try_with(|q| q.borrow_mut().pop_front())
        .ok()
        .flatten()
    {
        drop(value as *mut u8);
    }
    let _ = QUEUE.try_with(|q| {
        let mut q = q.borrow_mut();
        if q.capacity() > KEEP {
            *q = VecDeque::new();
        }
    });
    STATE.with(|s| s.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chain of `Link`s, each dropped by `drop_link`, which recurses into the next one.
    struct Link {
        next: *mut Link,
        id: u32,
    }

    thread_local! {
        static DROPPED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
        static MAX_SEEN: Cell<u32> = const { Cell::new(0) };
    }

    /// The drop glue as the compiler emits it (module docs).
    unsafe extern "C" fn drop_link(obj: *mut u8) {
        let state = velt_rt_drop_state();
        let n = *state;
        if n & DEPTH >= MAX_DEPTH {
            return velt_rt_drop_queue(obj, drop_link);
        }
        *state = n + 1;
        MAX_SEEN.with(|m| m.set(m.get().max((n + 1) & DEPTH)));
        let link = Box::from_raw(obj as *mut Link);
        DROPPED.with(|d| d.borrow_mut().push(link.id));
        if !link.next.is_null() {
            drop_link(link.next as *mut u8);
        }
        let m = *state - 1;
        *state = m;
        if m == QUEUED {
            velt_rt_drop_drain();
        }
    }

    fn chain(n: u32) -> *mut Link {
        let mut head: *mut Link = std::ptr::null_mut();
        for id in (0..n).rev() {
            head = Box::into_raw(Box::new(Link { next: head, id }));
        }
        head
    }

    #[test]
    fn a_long_chain_drops_in_order_within_the_depth() {
        // 1M nested calls would overflow a test thread's stack; with the limit, at most
        // MAX_DEPTH are nested, and a single chain still drops front to back.
        unsafe { drop_link(chain(1_000_000) as *mut u8) };
        let dropped = DROPPED.with(|d| std::mem::take(&mut *d.borrow_mut()));
        assert_eq!(dropped.len(), 1_000_000);
        assert!(dropped.iter().enumerate().all(|(i, &id)| i as u32 == id));
        assert_eq!(MAX_SEEN.with(Cell::get), MAX_DEPTH);
        assert_eq!(STATE.with(Cell::get), 0);
        assert!(QUEUE.with(|q| q.borrow().is_empty()));
    }

    #[test]
    fn a_short_chain_is_never_queued() {
        unsafe { drop_link(chain(10) as *mut u8) };
        assert_eq!(DROPPED.with(|d| d.borrow().len()), 10);
        assert_eq!(STATE.with(Cell::get), 0);
        assert!(QUEUE.with(|q| q.borrow().capacity() == 0));
    }
}
