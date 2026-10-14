//! Primitives behind `shared<T>`: reference counts (`velt_rt_rc_*`), atomic integers
//! (`velt_rt_atomic_*`), the `Mutex<T>` lock (`velt_rt_mutex_*`) and the `RwLock<T>` lock
//! (`velt_rt_rwlock_*`).
//!
//! All operate on memory owned by generated code (a refcount word, an `i64` cell, an 8-byte lock
//! word inside the shared object), so none of them allocates.

use lock_api::{RawRwLock as _, RawRwLockRecursive as _};
use parking_lot::RawRwLock;
use std::sync::atomic::{fence, AtomicI64, AtomicU64, AtomicU8, Ordering};

/// Refcounts above this abort (a leak of `isize::MAX` clones is a bug, as in `Arc`).
const MAX_REFCOUNT: u64 = isize::MAX as u64;

/// `shared.clone()`: increment the refcount at `p` (Relaxed, like `Arc::clone`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_inc(p: *mut u64) {
    let old = AtomicU64::from_ptr(p).fetch_add(1, Ordering::Relaxed);
    if old > MAX_REFCOUNT {
        crate::panic::fatal("reference count overflow");
    }
}

/// Drop one reference: returns 1 if it was the last one (the caller then drops the value and frees
/// the object; an Acquire fence has made every other owner's writes visible), else 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_dec(p: *mut u64) -> u8 {
    if AtomicU64::from_ptr(p).fetch_sub(1, Ordering::Release) != 1 {
        return 0;
    }
    fence(Ordering::Acquire);
    1
}

/// `shared<int>.add(delta)`: returns the new value (wrapping).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_add_i64(p: *mut i64, delta: i64) -> i64 {
    AtomicI64::from_ptr(p)
        .fetch_add(delta, Ordering::SeqCst)
        .wrapping_add(delta)
}

/// `shared<int>.get()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_load_i64(p: *mut i64) -> i64 {
    AtomicI64::from_ptr(p).load(Ordering::SeqCst)
}

/// `shared<int>.set(v)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_store_i64(p: *mut i64, v: i64) {
    AtomicI64::from_ptr(p).store(v, Ordering::SeqCst)
}

/// Size of the lock word generated code reserves inside a `Mutex<T>` object (align 8).
pub const MUTEX_SIZE: usize = 8;

/// The lock of a `Mutex<T>`: one byte, as `parking_lot`'s (a locked bit, and a bit saying that
/// threads are parked on it), with a different way of waiting. `with` bodies never await and
/// are short, so a thread that finds the lock taken spins briefly without leaving the CPU and
/// then parks. `parking_lot` yields the thread between the two, up to seven times: each a
/// system call, and under contention (many tasks sending to the same WebSocket connections
/// from several threads) that was most of what the waiting threads did.
struct Lock(AtomicU8);

const LOCKED: u8 = 1;
const PARKED: u8 = 2;
/// Rounds of spinning (1, 2, 4… pauses, 16 at most) before parking.
const SPINS: u32 = 10;
/// Unpark token: the lock was handed to the woken thread, which owns it now.
const HANDOFF: parking_lot_core::UnparkToken = parking_lot_core::UnparkToken(1);

const _: () = assert!(std::mem::size_of::<Lock>() <= MUTEX_SIZE);

impl Lock {
    #[inline]
    fn lock(&self) {
        let free = self
            .0
            .compare_exchange_weak(0, LOCKED, Ordering::Acquire, Ordering::Relaxed);
        if free.is_err() {
            self.lock_slow();
        }
    }

    #[cold]
    fn lock_slow(&self) {
        let mut spins = 0;
        let mut state = self.0.load(Ordering::Relaxed);
        loop {
            if state & LOCKED == 0 {
                let taken = state | LOCKED;
                match self.0.compare_exchange_weak(
                    state,
                    taken,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(now) => state = now,
                }
                continue;
            }
            if state & PARKED == 0 {
                if spins < SPINS {
                    for _ in 0..1u32 << spins.min(4) {
                        std::hint::spin_loop();
                    }
                    spins += 1;
                    state = self.0.load(Ordering::Relaxed);
                    continue;
                }
                let parked = state | PARKED;
                if let Err(now) = self.0.compare_exchange_weak(
                    state,
                    parked,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = now;
                    continue;
                }
            }
            let key = self as *const Lock as usize;
            // SAFETY: the key is this lock's address; the closures neither panic nor park.
            let woken = unsafe {
                parking_lot_core::park(
                    key,
                    || self.0.load(Ordering::Relaxed) == LOCKED | PARKED,
                    || {},
                    |_, _| {},
                    parking_lot_core::DEFAULT_PARK_TOKEN,
                    None,
                )
            };
            if woken == parking_lot_core::ParkResult::Unparked(HANDOFF) {
                // Synchronizes with the unlocking thread's critical section.
                std::sync::atomic::fence(Ordering::Acquire);
                return;
            }
            spins = 0;
            state = self.0.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn unlock(&self) {
        let alone = self
            .0
            .compare_exchange(LOCKED, 0, Ordering::Release, Ordering::Relaxed);
        if alone.is_err() {
            self.unlock_slow();
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let key = self as *const Lock as usize;
        // SAFETY: as in `lock_slow`. The callback runs with the queue of this key locked, so
        // the parked bit it leaves matches whether threads are still parked. Now and then
        // (`be_fair`, about every 0.5 ms per lock, as `parking_lot`) the lock goes straight to
        // the woken thread instead of being released, so spinning threads that keep winning it
        // cannot starve a parked one.
        unsafe {
            parking_lot_core::unpark_one(key, |result| {
                let parked = if result.have_more_threads { PARKED } else { 0 };
                if result.unparked_threads != 0 && result.be_fair {
                    self.0.store(LOCKED | parked, Ordering::Release);
                    return HANDOFF;
                }
                self.0.store(parked, Ordering::Release);
                parking_lot_core::DEFAULT_UNPARK_TOKEN
            });
        }
    }
}

/// Initialize the 8-byte lock word at `p` (unlocked). No destroy call is needed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_init(p: *mut u64) {
    p.write(0);
}

/// Lock (blocking the thread; `with` bodies never await, so holds are short). The module lock
/// stays held meanwhile: a `with` callback never takes it (task/local/module_state.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock(p: *mut u64) {
    (*(p as *const Lock)).lock();
    crate::task::local::mutex_locked();
}

/// [`velt_rt_mutex_lock`] from code that may run in a `read` callback (velt_vir: code not known
/// to run outside one), where a `with` stops the program: the callback shares the `RwLock`'s
/// value without counting it, which a `with` callback could not tell from what it changes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock_checked(p: *mut u64) {
    crate::read_mode::refuse_in_read("with");
    velt_rt_mutex_lock(p)
}

/// Unlock a mutex locked by this thread's `velt_rt_mutex_lock`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock(p: *mut u64) {
    crate::task::local::mutex_unlocked();
    (*(p as *const Lock)).unlock();
}

/// The words generated code reserves at the start of an `RwLock<T>` (`lock`, padding, then
/// `state`, `until` and `inhibit`, consecutive, align 8).
pub const RWLOCK_WORDS: usize = 19;

/// `state`: a writer is revoking the bias (waiting for the readers' slots). Readers take `lock`,
/// shared; writers wait for the revocation without holding `lock`.
pub(crate) const REVOKING: u64 = 1;

/// `state`: readers take `lock` too, shared (the bias is revoked). A writer holding `lock` writes
/// only in this state (one bit: the fast path tests it alone).
pub(crate) const MUTEX: u64 = 2;

/// Tags in the low bits of a slot naming an `RwLock` (hazard.rs): the read took the lock's
/// lock shared; the read is nested in another read of the same lock on this thread (it took
/// nothing).
const HELD: usize = 1;
const NESTED: usize = 2;

/// The longest a revoked bias stays revoked, in nanoseconds.
pub(crate) const MAX_INHIBIT: u64 = 10_000_000;

/// The words of an `RwLock<T>`. Writers take turns on `lock`, a parking_lot `RawRwLock`, taken
/// exclusively. While the
/// lock is *biased* (`state` 0) readers write nothing other threads read (Dice and Kogan,
/// "BRAVO: Biased Locking for Reader-Writer Locks", 2019): a reader names the lock in its
/// thread's slot (hazard.rs) and goes ahead while `state` stays 0; a writer sets `REVOKING`,
/// waits for the slots naming the lock to clear, sets `MUTEX`, and only then takes `lock` (a
/// writer holding it while it waited could wait for a reader whose nested read waits for that
/// writer). Another writer holding `lock` writes only once `state` is `MUTEX`. That
/// *revokes* the bias: until `until`
/// (nanoseconds on the process clock) readers take `lock` too, shared (BRAVO's underlying
/// reader-writer lock), so readers still run at once and a lock written often costs about what
/// a `Mutex` does. A writer waiting for `lock` holds new outermost readers back, so writers don't
/// starve, while a read nested in another goes past it, so readers nesting reads of two locks
/// in opposite orders don't wait for each other's writers. A
/// reader holding `lock` after `until`
/// biases it again (a thread looks at the clock after waiting for `lock` and on every 1024th
/// such read) and stores the time in `until`. How long a revocation lasts (`inhibit`) adapts:
/// twice the last when the bias it ended lasted less than that, else half (at least nine times
/// what waiting for the readers took, as in BRAVO), at most `MAX_INHIBIT`.
///
/// `state` is 128 bytes past `lock` (a cache line on the largest lines there are): every reader
/// reads it, writers rarely write it, and `lock`'s line, which a lock written often moves
/// between threads, stays out of the readers' way.
#[repr(C)]
pub(crate) struct Words {
    pub(crate) lock: RawRwLock,
    _line: [u64; 15],
    pub(crate) state: AtomicU64,
    pub(crate) until: AtomicU64,
    pub(crate) inhibit: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Words>() == RWLOCK_WORDS * 8);
const _: () = assert!(std::mem::offset_of!(Words, state) == 128);

/// Nanoseconds since the first call.
pub(crate) fn now_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

/// Initialize the words of an `RwLock<T>` at `p` (unlocked, biased).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_init(p: *mut u64) {
    crate::read_mode::rwlock_made();
    (p as *mut Words).write(Words {
        lock: RawRwLock::INIT,
        _line: [0; 15],
        state: AtomicU64::new(0),
        until: AtomicU64::new(0),
        inhibit: AtomicU64::new(0),
    });
}

/// How a read went in, in the low bits of `velt_rt_rwlock_read`'s result (the rest is this
/// thread's `hazard::Reader`, so the unlock needs no thread-local lookup; a read callback never
/// awaits, so it ends on the thread it began on): named in its slot; holding `lock`; nested
/// in a read of the same lock.
const FAST: u8 = 0;
const TOOK: u8 = 1;
const INSIDE: u8 = 2;
const HOW: u64 = 7;

const _: () = assert!(std::mem::align_of::<crate::hazard::Reader>() > HOW as usize);

/// `rw.read(f)`: read the value beside other readers (waiting while a writer writes; callbacks
/// never await). Readers hold no module lock either (task/local/module_state.rs). `reader` is
/// this thread's state, as an earlier read on this thread returned it (generated code keeps it
/// within one call of a function, which never changes threads), or 0.
///
/// The common reads (outermost, biased or revoked) run here without a call or a stack frame;
/// everything else continues in a function of its own, jumped to.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read(p: *mut u64, reader: u64) -> u64 {
    if reader == 0 {
        return read_looked_up(p);
    }
    mine(reader);
    // SAFETY: a thread-local without a destructor lives as long as its thread.
    let r = &*(reader as *const crate::hazard::Reader);
    if r.read_depth.get() != 0 {
        return read_nested(p, reader);
    }
    let w = &*(p as *const Words);
    // A lock this thread last found revoked: `lock` at once (always safe), leaving `state`
    // unread, which would delay taking it.
    if r.revoked.get() != p as usize {
        if w.state.load(Ordering::Relaxed) == 0 {
            let Some(s) = r.slot0() else {
                return read_claiming(p, reader);
            };
            s.store(p, Ordering::SeqCst);
            if w.state.load(Ordering::SeqCst) == 0 {
                r.read_depth.set(1);
                return entered(reader | FAST as u64);
            }
            // A writer revokes the bias and may wait for this slot (the read has read nothing).
            s.store(std::ptr::null_mut(), Ordering::Release);
        }
        r.revoked.set(p as usize);
    }
    if !w.lock.try_lock_shared() {
        return read_waiting(p, reader);
    }
    r.read_depth.set(1);
    r.holds.set(p as usize);
    let n = r.took.get().wrapping_add(1);
    r.took.set(n);
    if n.is_multiple_of(LOOK_EVERY) {
        return read_looking(p, reader);
    }
    entered(reader | TOOK as u64)
}

/// A read went in (`how`): count the lock for the module lock's check, when the program has
/// module state.
#[inline(always)]
fn entered(how: u64) -> u64 {
    if crate::task::local::module_used() {
        return entered_counted(how);
    }
    how
}

#[cold]
#[inline(never)]
fn entered_counted(how: u64) -> u64 {
    crate::task::local::mutex_locked();
    how
}

/// `velt_rt_rwlock_read` without this thread's state at hand.
#[cold]
#[inline(never)]
unsafe fn read_looked_up(p: *mut u64) -> u64 {
    velt_rt_rwlock_read(p, this_reader() as u64)
}

/// Debug builds: `reader` is this thread's (generated code starts over after an `await`).
#[inline(always)]
pub(crate) fn mine(reader: u64) {
    #[cfg(debug_assertions)]
    if reader != this_reader() as u64 {
        crate::panic::fatal("ICE: an RwLock call got another thread's reader state");
    }
    let _ = reader;
}

/// This thread's reader state, looked up.
#[cold]
#[inline(never)]
pub(crate) fn this_reader() -> *const crate::hazard::Reader {
    crate::hazard::READER.with(|r| r as *const crate::hazard::Reader)
}

/// The first read of a thread: its slots, then the read.
#[cold]
#[inline(never)]
unsafe fn read_claiming(p: *mut u64, reader: u64) -> u64 {
    (*(reader as *const crate::hazard::Reader)).slot(0);
    velt_rt_rwlock_read(p, reader)
}

/// A read nested in another.
#[inline(never)]
unsafe fn read_nested(p: *mut u64, reader: u64) -> u64 {
    let r = &*(reader as *const crate::hazard::Reader);
    let how = read_slow(r, p, &*(p as *const Words));
    entered(reader | how as u64)
}

/// An outermost read whose `lock` is held by a writer. One that waited looks at the bias at once (waiting
/// cost more than the clock; long readers waiting on each other is what the bias is for).
#[cold]
#[inline(never)]
unsafe fn read_waiting(p: *mut u64, reader: u64) -> u64 {
    let r = &*(reader as *const crate::hazard::Reader);
    let w = &*(p as *const Words);
    lock_slow(&w.lock, Take::Shared);
    r.read_depth.set(1);
    r.holds.set(p as usize);
    look(r, w);
    entered(reader | TOOK as u64)
}

/// Every `LOOK_EVERY`th outermost read holding `lock` (it reads the clock, which costs as
/// much as dozens of reads).
#[cold]
#[inline(never)]
unsafe fn read_looking(p: *mut u64, reader: u64) -> u64 {
    look(
        &*(reader as *const crate::hazard::Reader),
        &*(p as *const Words),
    );
    entered(reader | TOOK as u64)
}

/// An outermost read holding `lock`, after waiting for it or on every `LOOK_EVERY`th: is the bias back
/// (then no longer go to `lock` at once), or due back (`rebias`)?
#[cold]
#[inline(never)]
fn look(r: &crate::hazard::Reader, w: &Words) {
    if w.state.load(Ordering::Relaxed) == 0 || rebias(w) {
        r.revoked.set(0);
    }
}

/// How often an outermost read holding `lock` looks at the bias when it did not wait.
const LOOK_EVERY: u32 = 1024;

/// A nested read holding `lock`: its depth and which lock it holds (in its slot, which nested
/// reads look at).
#[inline]
fn took(r: &crate::hazard::Reader, p: *mut u64) -> u8 {
    match r.depth() {
        0 => {
            r.read_depth.set(r.read_depth.get() + 1);
            r.holds.set(p as usize);
        }
        _ => match r.push() {
            (_, Some(s)) => s.store((p as usize | HELD) as *mut u64, Ordering::Relaxed),
            (_, None) => crate::panic::fatal(
                "`read` callbacks of more than 16 different `RwLock`s nested in each other",
            ),
        },
    }
    TOOK
}

/// Rounds of backing off before blocking: waiting between tries doubles from 64 spins (about
/// 0.5 µs) to 4096 (about 35 µs), about 0.7 ms in all.
const BACKOFF_ROUNDS: u32 = 24;

/// How `lock_slow` takes the lock.
#[derive(Clone, Copy)]
pub(crate) enum Take {
    Shared,
    /// Shared, past waiting writers (a read nested in another, `read_slow`).
    Nested,
    Exclusive,
}

/// `lock` is busy: wait for it (as `take` says), backing off. A waiter that tries the
/// lock takes its cache line from the holder; one that rarely tries lets the holder lock it
/// again where it is, so short callbacks taking turns run several in a row (Anderson's
/// exponential backoff), more than `Mutex` (whose waiters look often) gets through. Each round
/// yields, so a waiter never keeps the holder from a core; after the rounds it blocks as
/// parking_lot does (a writer blocking there holds new readers back).
#[cold]
#[inline(never)]
pub(crate) fn lock_slow(lock: &RawRwLock, take: Take) {
    for i in 0..BACKOFF_ROUNDS {
        for _ in 0..1u32 << (i + 6).min(12) {
            std::hint::spin_loop();
        }
        std::thread::yield_now();
        let took = match take {
            Take::Shared => lock.try_lock_shared(),
            Take::Nested => lock.try_lock_shared_recursive(),
            Take::Exclusive => lock.try_lock_exclusive(),
        };
        if took {
            return;
        }
    }
    match take {
        Take::Shared => lock.lock_shared(),
        Take::Nested => lock.lock_shared_recursive(),
        Take::Exclusive => lock.lock_exclusive(),
    }
}

/// A reader holding `lock` of a revoked `RwLock`: after `until`, bias it again (true). Only
/// from `MUTEX`: never while a writer revokes (rwlock_write.rs `revoke`).
pub(crate) fn rebias(w: &Words) -> bool {
    let now = now_ns();
    let until = w.until.load(Ordering::Relaxed);
    // `u64::MAX`: a writer revoked and does not hold `lock` yet. A writer that starts revoking
    // after this claims `until` finds the lock biased again once it holds `lock`, and revokes
    // once more.
    if now < until
        || w.until
            .compare_exchange(until, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return false;
    }
    w.state
        .compare_exchange(MUTEX, 0, Ordering::Release, Ordering::Relaxed)
        .is_ok()
}

/// A read nested in another.
#[inline(never)]
fn read_slow(r: &crate::hazard::Reader, p: *mut u64, w: &Words) -> u8 {
    // Inside a read of the same lock: already safe from writers.
    if r.reads(p) {
        if let (_, Some(s)) = r.push() {
            s.store((p as usize | NESTED) as *mut u64, Ordering::Relaxed);
        }
        return INSIDE;
    }
    if w.state.load(Ordering::Relaxed) == 0 {
        let (_, slot) = r.push();
        let Some(s) = slot else {
            crate::panic::fatal(
                "`read` callbacks of more than 16 different `RwLock`s nested in each other",
            );
        };
        s.store(p, Ordering::SeqCst);
        if w.state.load(Ordering::SeqCst) == 0 {
            return FAST;
        }
        // A writer revokes the bias and may wait for this slot (the read has read nothing).
        s.store(std::ptr::null_mut(), Ordering::Release);
        r.leave();
    }
    // A read nested in another takes the lock past a waiting writer, as a recursive read
    // does: a writer waiting for it would wait for this thread's outer read, which another
    // writer may be holding new readers back on.
    if !w.lock.try_lock_shared_recursive() {
        lock_slow(&w.lock, Take::Nested);
    }
    took(r, p)
}

/// The end of the innermost `velt_rt_rwlock_read`, which returned `how`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read_unlock(p: *mut u64, how: u64) {
    if crate::task::local::module_used() {
        return read_unlock_counted(p, how);
    }
    read_unlock(p, how)
}

#[cold]
#[inline(never)]
unsafe fn read_unlock_counted(p: *mut u64, how: u64) {
    crate::task::local::mutex_unlocked();
    read_unlock(p, how)
}

#[inline(always)]
unsafe fn read_unlock(p: *mut u64, how: u64) {
    // SAFETY: `velt_rt_rwlock_read` on this thread returned it (see `HOW`).
    let r = &*((how & !HOW) as *const crate::hazard::Reader);
    let tag = (how & HOW) as u8;
    if r.read_depth.get() != 1 {
        return read_unlock_nested_in(p, r, tag);
    }
    read_unlock_outermost(p, r, tag)
}

/// The end of a thread's outermost read.
#[inline(always)]
unsafe fn read_unlock_outermost(p: *mut u64, r: &crate::hazard::Reader, tag: u8) {
    r.read_depth.set(0);
    match tag {
        // A writer waits for a slot naming `p` itself: clearing it publishes the read (whose
        // slot claimed the block).
        FAST => {
            if let Some(s) = r.slot0() {
                s.store(std::ptr::null_mut(), Ordering::Release)
            }
        }
        _ => {
            r.holds.set(0);
            (*(p as *const Words)).lock.unlock_shared();
        }
    }
}

/// The end of a read nested in another, and `lock` if it took it; or of the outermost read
/// when objects wait to be freed (`hazard::FREES_WAIT`), which it then frees.
#[inline(never)]
unsafe fn read_unlock_nested_in(p: *mut u64, r: &crate::hazard::Reader, tag: u8) {
    if r.read_depth.get() == crate::hazard::FREES_WAIT | 1 {
        read_unlock_outermost(p, r, tag);
        return crate::read_mode::free_waiting();
    }
    read_unlock_nested(r, tag);
    if tag == TOOK {
        (*(p as *const Words)).lock.unlock_shared();
    }
}

/// The end of a read nested in another. Slots past the depth are only ever read again after
/// being written.
#[inline(never)]
fn read_unlock_nested(r: &crate::hazard::Reader, how: u8) {
    match how {
        FAST => {
            if let Some(s) = r.pop() {
                s.store(std::ptr::null_mut(), Ordering::Release);
            }
        }
        _ => {
            r.leave();
            if r.depth() == 0 {
                r.holds.set(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rwlock_write::{velt_rt_rwlock_write, velt_rt_rwlock_write_unlock};

    fn tag(how: u64) -> u8 {
        (how & HOW) as u8
    }

    #[test]
    fn rwlock_readers_share_and_writers_exclude() {
        let mut l = [1u64; RWLOCK_WORDS];
        let p = l.as_mut_ptr();
        let held = |p: *mut u64| unsafe { !(*(p as *const RawRwLock)).try_lock_exclusive() };
        unsafe {
            velt_rt_rwlock_init(p);
            let a = velt_rt_rwlock_read(p, 0);
            let b = velt_rt_rwlock_read(p, 0);
            assert_eq!((tag(a), tag(b)), (FAST, INSIDE));
            // Biased: named in this thread's slot, `lock` free.
            assert!(crate::hazard::named(p));
            assert!(!held(p));
            (*(p as *const RawRwLock)).unlock_exclusive();
            velt_rt_rwlock_read_unlock(p, b);
            velt_rt_rwlock_read_unlock(p, a);
            assert!(!crate::hazard::named(p));
            // The first write revokes the bias: readers take the lock, shared.
            let wr = velt_rt_rwlock_write(p, 0);
            assert_eq!(l[16], MUTEX);
            velt_rt_rwlock_write_unlock(p, wr);
            let a = velt_rt_rwlock_read(p, 0);
            let b = velt_rt_rwlock_read(p, 0);
            assert_eq!((tag(a), tag(b)), (TOOK, INSIDE));
            assert!(held(p));
            velt_rt_rwlock_read_unlock(p, b);
            velt_rt_rwlock_read_unlock(p, a);
            assert!(!held(p));
            (*(p as *const RawRwLock)).unlock_exclusive();
            // After `until`, a reader biases it again.
            l[17] = 0;
            for _ in 0..LOOK_EVERY {
                let a = velt_rt_rwlock_read(p, 0);
                velt_rt_rwlock_read_unlock(p, a);
            }
            assert_eq!(l[16], 0);
            let a = velt_rt_rwlock_read(p, 0);
            assert_eq!(tag(a), FAST);
            velt_rt_rwlock_read_unlock(p, a);
        }
    }

    #[test]
    fn revoked_readers_share_the_lock() {
        // After a write, readers on two threads hold `lock` at once (a nested read of two locks
        // in opposite orders can't wait on another reader), and a writer waits for both.
        let mut l = [1u64; RWLOCK_WORDS];
        let p = l.as_mut_ptr() as usize;
        unsafe {
            velt_rt_rwlock_init(p as *mut u64);
            let wr = velt_rt_rwlock_write(p as *mut u64, 0);
            velt_rt_rwlock_write_unlock(p as *mut u64, wr);
            let a = velt_rt_rwlock_read(p as *mut u64, 0);
            assert_eq!(tag(a), TOOK);
            let other = std::thread::spawn(move || {
                let b = velt_rt_rwlock_read(p as *mut u64, 0);
                let t = tag(b);
                velt_rt_rwlock_read_unlock(p as *mut u64, b);
                t
            });
            assert_eq!(other.join().unwrap(), TOOK);
            assert!(!(*(p as *const RawRwLock)).try_lock_exclusive());
            velt_rt_rwlock_read_unlock(p as *mut u64, a);
            assert!((*(p as *const RawRwLock)).try_lock_exclusive());
            (*(p as *const RawRwLock)).unlock_exclusive();
        }
    }

    #[test]
    fn lock_slow_waits_as_reader_and_writer() {
        static L: RawRwLock = RawRwLock::INIT;
        L.lock_exclusive();
        let reader = std::thread::spawn(|| {
            lock_slow(&L, Take::Shared);
            unsafe { L.unlock_shared() };
        });
        let writer = std::thread::spawn(|| {
            lock_slow(&L, Take::Exclusive);
            unsafe { L.unlock_exclusive() };
        });
        std::thread::sleep(std::time::Duration::from_millis(2));
        unsafe { L.unlock_exclusive() };
        reader.join().unwrap();
        writer.join().unwrap();
        assert!(L.try_lock_exclusive());
        unsafe { L.unlock_exclusive() };
    }

    #[test]
    fn rwlock_writer_waits_for_readers() {
        struct Cell {
            words: [u64; RWLOCK_WORDS],
            value: [u64; 2],
        }
        let c = Box::into_raw(Box::new(Cell {
            words: [0; RWLOCK_WORDS],
            value: [0, 0],
        })) as usize;
        unsafe { velt_rt_rwlock_init((*(c as *mut Cell)).words.as_mut_ptr()) };
        let threads: Vec<_> = (0..4)
            .map(|i| {
                std::thread::spawn(move || unsafe {
                    let c = c as *mut Cell;
                    let p = (*c).words.as_mut_ptr();
                    for n in 0..20000 {
                        if i == 0 && n % 10 == 0 {
                            let wr = velt_rt_rwlock_write(p, 0);
                            let v = std::ptr::read_volatile(&(*c).value[0]);
                            std::ptr::write_volatile(&mut (*c).value[0], v + 1);
                            std::ptr::write_volatile(&mut (*c).value[1], v + 1);
                            velt_rt_rwlock_write_unlock(p, wr);
                        } else {
                            let how = velt_rt_rwlock_read(p, 0);
                            let a = std::ptr::read_volatile(&(*c).value[0]);
                            let b = std::ptr::read_volatile(&(*c).value[1]);
                            assert_eq!(a, b, "a reader saw a write in progress");
                            velt_rt_rwlock_read_unlock(p, how);
                        }
                    }
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        let c = unsafe { Box::from_raw(c as *mut Cell) };
        assert_eq!(c.value, [2000, 2000]);
    }

    #[test]
    fn refcount_and_atomics() {
        let mut rc = 1u64;
        unsafe {
            velt_rt_rc_inc(&mut rc);
            assert_eq!(velt_rt_rc_dec(&mut rc), 0);
            assert_eq!(velt_rt_rc_dec(&mut rc), 1);
        }
        let mut v = 5i64;
        unsafe {
            assert_eq!(velt_rt_atomic_add_i64(&mut v, 3), 8);
            velt_rt_atomic_store_i64(&mut v, -1);
            assert_eq!(velt_rt_atomic_load_i64(&mut v), -1);
        }
    }

    #[test]
    fn mutex_serializes_threads() {
        struct Cell {
            lock: u64,
            items: Vec<u32>,
        }
        let shared = Box::into_raw(Box::new(Cell {
            lock: 0,
            items: Vec::new(),
        })) as usize;
        unsafe { velt_rt_mutex_init(&mut (*(shared as *mut Cell)).lock) };
        let threads: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    let c = shared as *mut Cell;
                    for i in 0..1000 {
                        unsafe {
                            velt_rt_mutex_lock(&mut (*c).lock);
                            (*c).items.push(t * 1000 + i);
                            velt_rt_mutex_unlock(&mut (*c).lock);
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let cell = unsafe { Box::from_raw(shared as *mut Cell) };
        assert_eq!(cell.items.len(), 8000);
    }

    #[test]
    fn a_parked_thread_gets_the_lock_from_threads_that_keep_taking_it() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::time::{Duration, Instant};
        // Liveness under the worst pattern for a waiter: four threads take the lock again right
        // after releasing it, so a thread that parks loses most races for it. (The hand-off
        // bounds the wait; here, on an M4, it took the 50 acquisitions from ~200 ms to ~10 ms.
        // A bound that tight would be flaky on a loaded machine, so this checks only progress.)
        let lock = Box::leak(Box::new(AtomicU64::new(0))).as_ptr() as usize;
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let hogs: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        unsafe {
                            velt_rt_mutex_lock(lock as *mut u64);
                            for _ in 0..2000 {
                                std::hint::spin_loop();
                            }
                            velt_rt_mutex_unlock(lock as *mut u64);
                        }
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        for _ in 0..50 {
            unsafe {
                velt_rt_mutex_lock(lock as *mut u64);
                velt_rt_mutex_unlock(lock as *mut u64);
            }
        }
        let took = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        for h in hogs {
            h.join().unwrap();
        }
        assert!(
            took < Duration::from_secs(5),
            "50 acquisitions took {took:?}"
        );
    }
}
