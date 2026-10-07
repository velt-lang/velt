//! Bounded nesting of drops (#543): dropping an object whose fields lead back to objects of its
//! own kind (a list through an array, a `Map` or a closure, a tree of a class hierarchy) would
//! nest one call per level and overflow the stack on a long chain. The compiler brackets the
//! drop glue of such a type with `enter` / `leave`; past [`MAX_DEPTH`] nested drops on a
//! thread, `enter` says no and the glue `queue`s the object instead (a value that lives in a
//! slot about to be freed, such as an array element, is moved to a heap box first), and the
//! outermost `leave` drops the queued objects, each with the full depth again. Objects up to
//! the limit drop exactly as nested calls would; deeper ones drop after the outermost drop's
//! other work, in the order they were reached. Every call reads and writes one thread-local
//! counter, and `leave` looks at the queue only when it ends the outermost drop.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/// Drop glue of an object: called with the object pointer.
type DropFn = unsafe extern "C" fn(*mut u8);

/// Nested drops a thread runs before it queues the next one. A level is a few small frames of
/// drop glue (plus whatever a `[Symbol.dispose]()` hook calls), so this stays far below the
/// smallest stack a program runs on (1 MiB: the main thread on Windows; WebAssembly modules
/// are linked with 8 MiB).
pub const MAX_DEPTH: u32 = 128;

/// Capacity kept for the next drop that queues objects.
const KEEP: usize = 1024;

/// [`STATE`]: the outermost `leave` is draining the queue.
const DRAINING: u32 = 1 << 31;
/// [`STATE`]: something was queued since the queue was last drained.
const QUEUED: u32 = 1 << 30;
/// [`STATE`]: nested drops under way.
const DEPTH: u32 = QUEUED - 1;

thread_local! {
    /// Nested drops under way on this thread, with the [`DRAINING`] and [`QUEUED`] bits.
    static STATE: Cell<u32> = const { Cell::new(0) };
    /// Objects queued by `queue`, dropped by the outermost `leave`.
    static QUEUE: RefCell<VecDeque<(usize, DropFn)>> = const { RefCell::new(VecDeque::new()) };
}

/// Start a drop: 1 to go ahead (then `leave` when done), or 0 when the thread is
/// [`MAX_DEPTH`] drops deep: the caller then `queue`s what it was dropping and returns.
#[no_mangle]
pub extern "C" fn velt_rt_drop_enter() -> u8 {
    let state = STATE.with(Cell::get);
    if state & DEPTH >= MAX_DEPTH {
        return 0;
    }
    STATE.with(|s| s.set(state + 1));
    1
}

/// Drop `obj` with `drop` (drop glue that takes it over) when the outermost drop is done; at
/// once while the thread is being torn down (its queue is gone).
///
/// # Safety
/// `drop` must take over `obj`: the compiler queues only objects whose last reference it was
/// dropping.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_drop_queue(obj: *mut u8, drop: DropFn) {
    let queued = QUEUE.try_with(|q| q.borrow_mut().push_back((obj as usize, drop)));
    if queued.is_err() {
        return drop(obj);
    }
    STATE.with(|s| s.set(s.get() | QUEUED));
}

/// Done with a drop `enter` let through: the outermost `leave` drops the queued objects (which
/// may queue more), unless it runs inside that draining already or nothing was queued.
///
/// # Safety
/// Every queued object must still be owned by the queue (see `velt_rt_drop_queue`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_drop_leave() {
    let state = STATE.with(Cell::get) - 1;
    if state != QUEUED {
        return STATE.with(|s| s.set(state));
    }
    STATE.with(|s| s.set(DRAINING));
    // The drop glue runs with nothing borrowed: it queues objects of its own.
    while let Some((obj, drop)) = QUEUE
        .try_with(|q| q.borrow_mut().pop_front())
        .ok()
        .flatten()
    {
        drop(obj as *mut u8);
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

    unsafe extern "C" fn drop_link(obj: *mut u8) {
        if velt_rt_drop_enter() == 0 {
            return velt_rt_drop_queue(obj, drop_link);
        }
        let depth = STATE.with(Cell::get) & DEPTH;
        MAX_SEEN.with(|m| m.set(m.get().max(depth)));
        let link = Box::from_raw(obj as *mut Link);
        DROPPED.with(|d| d.borrow_mut().push(link.id));
        if !link.next.is_null() {
            drop_link(link.next as *mut u8);
        }
        velt_rt_drop_leave();
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
        assert!(QUEUE.with(|q| q.borrow().capacity() == 0));
    }
}
