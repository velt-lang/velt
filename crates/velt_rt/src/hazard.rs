//! Per-thread reader slots (hazard pointers): how a reader tells writers on other threads what it
//! is reading without writing any memory other threads write. Each thread has one block of
//! slots on a cache line of its own; slot `d` names what the read callback at nesting depth `d`
//! reads (an `RwLock`'s lock word, shared.rs), and the last one is free for short uses. A
//! writer scans every thread's slots for what it is about to change.
//!
//! A reader stores into its slot and then checks the writer's flag, and a writer sets its flag
//! and then scans the slots, both SeqCst: either the writer sees the slot or the reader sees
//! the flag.

use std::cell::Cell;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Slots for read callbacks nested in each other (deeper ones only nest in a read of the same
/// lock, shared.rs), then one more.
pub(crate) const READ_SLOTS: usize = 16;
const SLOTS: usize = READ_SLOTS + 1;

/// One thread's slots. Never freed: a thread that exits gives its block to the next one.
#[repr(align(128))]
struct Block {
    slot: [AtomicPtr<u64>; SLOTS],
    used: AtomicBool,
    next: *mut Block,
}

/// Every thread's block, a list blocks are only ever pushed onto.
static HEAD: AtomicPtr<Block> = AtomicPtr::new(null_mut());

/// This thread's block, given back when it exits.
struct Owner(Cell<*mut Block>);

impl Drop for Owner {
    fn drop(&mut self) {
        let b = self.0.get();
        if !b.is_null() {
            // SAFETY: blocks are never freed.
            unsafe { (*b).used.store(false, Ordering::Release) };
        }
    }
}

/// What a reader needs on every read, its slots' and read mode's, in one thread-local without a
/// destructor (the fastest kind to reach).
pub(crate) struct Reader {
    /// This thread's block (null until it first reads).
    block: Cell<*mut Block>,
    /// The read callbacks this thread is in (`push`): it is in read mode when nonzero
    /// (read_mode.rs).
    pub(crate) read_depth: Cell<u32>,
    /// The `RwLock` whose mutex this thread's outermost read callback holds (shared.rs), or 0.
    pub(crate) holds: Cell<usize>,
    /// The `RwLock` this thread last found revoked (shared.rs): its outermost reads take the
    /// mutex without looking at the bias first.
    pub(crate) revoked: Cell<usize>,
    /// Outermost reads this thread made holding a mutex (every `LOOK_EVERY`th looks at the bias, shared.rs).
    pub(crate) took: Cell<u32>,
}

thread_local! {
    pub(crate) static READER: Reader = const {
        Reader {
            block: Cell::new(null_mut()),
            read_depth: Cell::new(0),
            holds: Cell::new(0),
            revoked: Cell::new(0),
            took: Cell::new(0),
        }
    };
    /// Gives the block back when the thread exits.
    static OWNER: Owner = const { Owner(Cell::new(null_mut())) };
}

impl Reader {
    /// This thread's block, claimed on first use.
    #[inline]
    fn block(&self) -> *mut Block {
        let b = self.block.get();
        if !b.is_null() {
            return b;
        }
        self.claim_block()
    }

    #[cold]
    fn claim_block(&self) -> *mut Block {
        let b = claim();
        self.block.set(b);
        // Without a destructor run (the thread is exiting) the block stays claimed: a leak
        // of one block, never a reuse while used.
        let _ = OWNER.try_with(|o| o.0.set(b));
        b
    }

    /// Slot 0, once this thread has a block (`slot` claims one).
    #[inline]
    pub(crate) fn slot0(&self) -> Option<&'static AtomicPtr<u64>> {
        let b = self.block.get();
        // SAFETY: blocks are never freed.
        (!b.is_null()).then(|| unsafe { &(*b).slot[0] })
    }

    /// Slot `i` of this thread.
    #[inline]
    pub(crate) fn slot(&self, i: usize) -> &'static AtomicPtr<u64> {
        // SAFETY: blocks are never freed.
        unsafe { &(*self.block()).slot[i] }
    }

    /// Entering a read callback (and read mode): its depth, and its slot if it has one.
    #[inline]
    pub(crate) fn push(&self) -> (usize, Option<&'static AtomicPtr<u64>>) {
        let v = self.read_depth.get();
        self.read_depth.set(v + 1);
        let d = v as usize;
        (d, (d < READ_SLOTS).then(|| self.slot(d)))
    }

    /// Does a read this thread is in name `p` (with any tag in its low 3 bits): the lock its
    /// outermost read holds, or a slot below the depth? (Slots at or past the depth may hold
    /// stale values, and so does the first while the outermost read holds a lock.)
    pub(crate) fn reads(&self, p: *mut u64) -> bool {
        let d = self.depth();
        if d == 0 {
            return false;
        }
        let held = self.holds.get();
        if held == p as usize {
            return true;
        }
        let from = usize::from(held != 0);
        // SAFETY: blocks are never freed.
        let slots = unsafe { &(*self.block()).slot };
        slots[from..d.min(READ_SLOTS)]
            .iter()
            .any(|s| s.load(Ordering::Relaxed) as usize & !7 == p as usize)
    }

    /// Leaving the innermost read callback without touching its slot.
    #[inline]
    pub(crate) fn leave(&self) {
        self.read_depth.set(self.read_depth.get() - 1);
    }

    /// Read callbacks this thread is in (that `push` counted).
    #[inline]
    pub(crate) fn depth(&self) -> usize {
        self.read_depth.get() as usize
    }

    /// Leaving the innermost read callback (and read mode, when it is the last): its slot, if
    /// its depth has one.
    #[inline]
    pub(crate) fn pop(&self) -> Option<&'static AtomicPtr<u64>> {
        let v = self.read_depth.get() - 1;
        self.read_depth.set(v);
        let d = v as usize;
        (d < READ_SLOTS).then(|| self.slot(d))
    }
}

/// A free block, or a new one pushed onto the list.
fn claim() -> *mut Block {
    let mut h = HEAD.load(Ordering::Acquire);
    while !h.is_null() {
        // SAFETY: blocks are never freed.
        let b = unsafe { &*h };
        if !b.used.load(Ordering::Relaxed)
            && b.used
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        {
            return h;
        }
        h = b.next;
    }
    let b = Box::into_raw(Box::new(Block {
        slot: Default::default(),
        used: AtomicBool::new(true),
        next: null_mut(),
    }));
    let mut head = HEAD.load(Ordering::Relaxed);
    loop {
        // SAFETY: not yet published.
        unsafe { (*b).next = head };
        match HEAD.compare_exchange_weak(head, b, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return b,
            Err(h) => head = h,
        }
    }
}

/// Every non-null slot of every thread, SeqCst (after the caller's own SeqCst write).
pub(crate) fn each(mut f: impl FnMut(*mut u64)) {
    let mut h = HEAD.load(Ordering::Acquire);
    while !h.is_null() {
        // SAFETY: blocks are never freed.
        let b = unsafe { &*h };
        for s in &b.slot {
            let p = s.load(Ordering::SeqCst);
            if !p.is_null() {
                f(p);
            }
        }
        h = b.next;
    }
}

/// Does a slot of any thread name `p`?
pub(crate) fn named(p: *mut u64) -> bool {
    let mut found = false;
    each(|q| found |= q == p);
    found
}
