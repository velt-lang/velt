//! The copies made during one transfer of a value to another thread (`velt_rt_xfer_*`): a
//! source-to-copy map, so an object reached twice in the copied graph is copied once (`p.x ===
//! p.y` still holds on the other side, as with structured clone) and a cycle is copied as a
//! cycle instead of recursing until the stack overflows (#351, #352).
//!
//! The compiler brackets the transfer of a value whose type can reach a counted object with
//! `begin` / `end` (the `TransferRoot` glue). The clone glue of a counted object asks `find`
//! before copying an object referenced more than once and `record`s its copy; the transfer glue
//! asks `find` before copying a still-shared object. The sender's reference to an object that
//! was replaced by its copy is released at `end` (`defer`), not at once, so every source in the
//! map stays alive (its address cannot be reused) and keeps the count that made it a candidate
//! for sharing until the transfer is done. A copy maps to itself as well, so a graph that was
//! just copied and is transferred next (a function value's copy for a task) is not copied again.
//! User code that runs during a transfer (a class's own `clone()`) runs outside it (`suspend` /
//! `resume`): its own `.clone()` calls copy as they would anywhere else. Outside a transfer every
//! call returns after reading one thread-local counter.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Drop glue of a value: called with the address of a slot holding it.
type DropFn = unsafe extern "C" fn(*mut u8);

/// What `find` returns outside a transfer (never an object address: objects are aligned).
const NO_TRANSFER: usize = 1;

/// Capacity kept for the next transfer.
const KEEP: usize = 1024;

/// Hashes an object address (aligned, so the low bits carry nothing).
#[derive(Default)]
struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8 | u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = ((n as u64) >> 3).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

#[derive(Default)]
struct Transfer {
    /// Source object address -> its copy.
    copies: HashMap<usize, usize, BuildHasherDefault<AddrHasher>>,
    /// Sources whose reference the transfer gave up, released at `end`.
    deferred: Vec<(usize, DropFn)>,
}

thread_local! {
    /// Nested `begin`s: 0 outside a transfer (the fast check).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static TRANSFER: RefCell<Transfer> = RefCell::new(Transfer::default());
    /// Transfers suspended while user code runs, innermost last.
    static SUSPENDED: RefCell<Vec<Transfer>> = const { RefCell::new(Vec::new()) };
}

/// Set the transfer under way aside while user code runs (a class's own `clone()`): returns its
/// depth for `resume`. In between, nothing is looked up or recorded in it, and a transfer the
/// user code starts has a map of its own.
#[no_mangle]
pub extern "C" fn velt_rt_xfer_suspend() -> u32 {
    let depth = DEPTH.with(|d| d.replace(0));
    if depth > 0 {
        let current = TRANSFER.with(|t| std::mem::take(&mut *t.borrow_mut()));
        SUSPENDED.with(|s| s.borrow_mut().push(current));
    }
    depth
}

/// Continue the transfer set aside by the `suspend` that returned `depth`.
#[no_mangle]
pub extern "C" fn velt_rt_xfer_resume(depth: u32) {
    if depth == 0 {
        return;
    }
    if let Some(saved) = SUSPENDED.with(|s| s.borrow_mut().pop()) {
        TRANSFER.with(|t| *t.borrow_mut() = saved);
    }
    DEPTH.with(|d| d.set(depth));
}

/// Start transferring a value (nested calls nest).
#[no_mangle]
pub extern "C" fn velt_rt_xfer_begin() {
    DEPTH.with(|d| d.set(d.get() + 1));
}

/// Done transferring a value: the outermost `end` releases the deferred references and
/// forgets the copies.
///
/// # Safety
/// Each deferred object must still be valid (the compiler defers only references it owned).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_xfer_end() {
    let depth = DEPTH.with(|d| {
        d.set(d.get().saturating_sub(1));
        d.get()
    });
    if depth > 0 {
        return;
    }
    let mut deferred = TRANSFER.with(|t| {
        let mut t = t.borrow_mut();
        if t.copies.capacity() > KEEP {
            t.copies = HashMap::default();
        } else {
            t.copies.clear();
        }
        std::mem::take(&mut t.deferred)
    });
    if deferred.is_empty() {
        return;
    }
    // The drop glue runs with nothing borrowed.
    for &(object, drop) in &deferred {
        let mut slot = object as *mut u8;
        drop(&mut slot as *mut *mut u8 as *mut u8);
    }
    deferred.clear();
    if deferred.capacity() <= KEEP {
        TRANSFER.with(|t| t.borrow_mut().deferred = deferred);
    }
}

/// The copy made of `object` in this transfer; null if none yet (the caller copies it and
/// `record`s the copy), or 1 outside a transfer (nothing to record).
#[no_mangle]
pub extern "C" fn velt_rt_xfer_find(object: *const u8) -> *mut u8 {
    if DEPTH.with(Cell::get) == 0 {
        return NO_TRANSFER as *mut u8;
    }
    TRANSFER.with(|t| {
        t.borrow()
            .copies
            .get(&(object as usize))
            .map_or(std::ptr::null_mut(), |&c| c as *mut u8)
    })
}

/// `copy` is the copy of `object` in this transfer (and stays itself if it is reached again).
#[no_mangle]
pub extern "C" fn velt_rt_xfer_record(object: *const u8, copy: *const u8) {
    if DEPTH.with(Cell::get) == 0 {
        return;
    }
    TRANSFER.with(|t| {
        let mut t = t.borrow_mut();
        t.copies.insert(object as usize, copy as usize);
        t.copies.insert(copy as usize, copy as usize);
    });
}

/// Release the transfer's reference to `object` (with its drop glue `drop`) when the transfer
/// ends: 1, or 0 outside a transfer (the caller releases it now).
#[no_mangle]
pub extern "C" fn velt_rt_xfer_defer(object: *mut u8, drop: DropFn) -> u8 {
    if DEPTH.with(Cell::get) == 0 {
        return 0;
    }
    TRANSFER.with(|t| t.borrow_mut().deferred.push((object as usize, drop)));
    1
}

/// HTTP handlers whose requests copy what they captured (velt_vir `async_fn/handler.rs`): a
/// request copies from the environment all requests share, and the copy updates the counts of
/// captured variables' cells there (another reference while the closure's environment is
/// copied, given up once the copy has cells of its own). Counts are not atomic, so those copies
/// take turns: `copy_lock` / `copy_unlock` bracket one request's copies (nested pairs on one
/// thread take the lock once).
static COPY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// The `copy_lock`s this thread holds (the guard lives in `COPY_GUARD`).
    static COPY_DEPTH: Cell<u32> = const { Cell::new(0) };
    static COPY_GUARD: RefCell<Option<std::sync::MutexGuard<'static, ()>>> =
        const { RefCell::new(None) };
    /// Set by the many-threads check of a closure environment with captured variables' cells
    /// (`saw_cells`), read and cleared by `take_cells`.
    static SAW_CELLS: Cell<bool> = const { Cell::new(false) };
}

/// Start copying a request's captures from a shared handler environment (see `COPY_LOCK`).
#[no_mangle]
pub extern "C" fn velt_rt_copy_lock() {
    let depth = COPY_DEPTH.with(|d| d.replace(d.get() + 1));
    if depth == 0 {
        // A panic while copying stops the program, so a poisoned lock guards nothing torn.
        let guard = COPY_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        COPY_GUARD.with(|g| *g.borrow_mut() = Some(guard));
    }
}

/// Done copying (the outermost call releases the lock).
#[no_mangle]
pub extern "C" fn velt_rt_copy_unlock() {
    let depth = COPY_DEPTH.with(|d| {
        d.set(d.get().saturating_sub(1));
        d.get()
    });
    if depth == 0 {
        COPY_GUARD.with(|g| drop(g.borrow_mut().take()));
    }
}

/// The many-threads check (velt_vir `glue/many.rs`) reached a function value whose environment
/// holds captured variables' cells, which its calls update.
#[no_mangle]
pub extern "C" fn velt_rt_saw_cells() {
    SAW_CELLS.with(|s| s.set(true));
}

/// Did the many-threads check reach such cells since the last call (1 or 0)? Clears the mark.
#[no_mangle]
pub extern "C" fn velt_rt_take_cells() -> u8 {
    u8::from(SAW_CELLS.with(|s| s.replace(false)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_drop(slot: *mut u8) {
        DROPPED.fetch_add(*(slot as *const usize), Ordering::SeqCst);
    }

    #[test]
    fn copies_are_found_only_inside_one_transfer() {
        let (a, a2, b) = (0x1000 as *mut u8, 0x2000 as *mut u8, 0x3000 as *mut u8);
        assert_eq!(velt_rt_xfer_find(a) as usize, NO_TRANSFER);
        velt_rt_xfer_record(a, a2);
        assert_eq!(velt_rt_xfer_defer(a, count_drop), 0);
        velt_rt_xfer_begin();
        assert!(velt_rt_xfer_find(a).is_null());
        velt_rt_xfer_record(a, a2);
        velt_rt_xfer_begin();
        assert_eq!(velt_rt_xfer_find(a), a2);
        assert_eq!(velt_rt_xfer_find(a2), a2, "a copy is not copied again");
        assert!(velt_rt_xfer_find(b).is_null());
        // User code runs outside the transfer, then it continues where it was.
        let depth = velt_rt_xfer_suspend();
        assert_eq!(velt_rt_xfer_find(a) as usize, NO_TRANSFER);
        velt_rt_xfer_begin();
        assert!(velt_rt_xfer_find(a).is_null(), "a transfer of its own");
        unsafe { velt_rt_xfer_end() };
        velt_rt_xfer_resume(depth);
        assert_eq!(velt_rt_xfer_find(a), a2);
        assert_eq!(velt_rt_xfer_defer(b, count_drop), 1);
        unsafe { velt_rt_xfer_end() };
        assert_eq!(
            DROPPED.load(Ordering::SeqCst),
            0,
            "released by the outermost end"
        );
        unsafe { velt_rt_xfer_end() };
        assert_eq!(DROPPED.load(Ordering::SeqCst), 0x3000);
        assert_eq!(velt_rt_xfer_find(a) as usize, NO_TRANSFER);
    }

    #[test]
    fn copy_lock_nests_and_cells_mark_clears() {
        velt_rt_copy_lock();
        velt_rt_copy_lock();
        velt_rt_copy_unlock();
        assert!(
            COPY_LOCK.try_lock().is_err(),
            "held until the outermost unlock"
        );
        velt_rt_copy_unlock();
        assert!(COPY_LOCK.try_lock().is_ok());
        assert_eq!(velt_rt_take_cells(), 0);
        velt_rt_saw_cells();
        assert_eq!(velt_rt_take_cells(), 1);
        assert_eq!(velt_rt_take_cells(), 0);
    }
}
