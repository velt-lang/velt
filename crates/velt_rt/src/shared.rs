//! Primitives behind `shared<T>`: reference counts (`velt_rt_rc_*`), atomic integers
//! (`velt_rt_atomic_*`), the `Mutex<T>` lock (`velt_rt_mutex_*`) and the `RwLock<T>` lock
//! (`velt_rt_rwlock_*`).
//!
//! All operate on memory owned by generated code (a refcount word, an `i64` cell, an 8-byte lock
//! word inside the shared object), so none of them allocates.

use lock_api::RawMutex as _;
use parking_lot::RawMutex;
use std::sync::atomic::{fence, AtomicI64, AtomicU64, Ordering};

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

const _: () = assert!(std::mem::size_of::<RawMutex>() <= MUTEX_SIZE);
const _: () = assert!(std::mem::align_of::<RawMutex>() <= 8);

/// Initialize the 8-byte lock word at `p` (unlocked). No destroy call is needed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_init(p: *mut u64) {
    (p as *mut RawMutex).write(RawMutex::INIT);
}

/// Lock (blocking the thread; `with` bodies never await, so holds are short). The module lock
/// stays held meanwhile: a `with` callback never takes it (task/local/module_state.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock(p: *mut u64) {
    (*(p as *const RawMutex)).lock();
    crate::task::local::mutex_locked();
    crate::read_mode::enter_exclusive();
}

/// Unlock a mutex locked by this thread's `velt_rt_mutex_lock`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock(p: *mut u64) {
    crate::read_mode::leave_exclusive();
    crate::task::local::mutex_unlocked();
    (*(p as *const RawMutex)).unlock();
}

/// The words generated code reserves at the start of an `RwLock<T>` (`lock`, padding, then
/// `state`, `until` and `inhibit`, consecutive, align 8).
pub const RWLOCK_WORDS: usize = 19;

/// `state`: readers take `lock` as writers do (the bias is revoked).
const MUTEX: u64 = 1;

/// Tags in the low bits of a slot naming an `RwLock` (hazard.rs): the read took the lock's
/// mutex; the read is nested in another read of the same lock on this thread (it took nothing).
const HELD: usize = 1;
const NESTED: usize = 2;

/// The longest a revoked bias stays revoked, in nanoseconds.
const MAX_INHIBIT: u64 = 10_000_000;

/// The words of an `RwLock<T>`. Writers take turns on `lock`, a parking_lot mutex. While the
/// lock is *biased* (`state` 0) readers write nothing other threads read (Dice and Kogan,
/// "BRAVO: Biased Locking for Reader-Writer Locks", 2019): a reader names the lock in its
/// thread's slot (hazard.rs) and goes ahead while `state` stays 0; a writer sets `MUTEX`, then
/// waits for the slots naming the lock to clear. That *revokes* the bias: until `until`
/// (nanoseconds on the process clock) readers take `lock` too, exactly as a `Mutex` is taken,
/// so a lock written often costs what a `Mutex` does. A reader holding `lock` after `until`
/// biases it again (a thread looks at the clock after waiting for the mutex and on every 64th
/// such read) and stores the time in `until`. How long a revocation lasts (`inhibit`) adapts:
/// twice the last when the bias it ended lasted less than that, else half (at least nine times
/// what waiting for the readers took, as in BRAVO), at most `MAX_INHIBIT`.
///
/// `state` is 128 bytes past `lock` (a cache line on the largest lines there are): every reader
/// reads it, writers rarely write it, and the mutex's line, which a lock written often moves
/// between threads, stays out of the readers' way.
#[repr(C)]
struct Words {
    lock: RawMutex,
    _line: [u64; 15],
    state: AtomicU64,
    until: AtomicU64,
    inhibit: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Words>() == RWLOCK_WORDS * 8);
const _: () = assert!(std::mem::offset_of!(Words, state) == 128);

/// Nanoseconds since the first call.
fn now_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

/// Initialize the words of an `RwLock<T>` at `p` (unlocked, biased).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_init(p: *mut u64) {
    (p as *mut Words).write(Words {
        lock: RawMutex::INIT,
        _line: [0; 15],
        state: AtomicU64::new(0),
        until: AtomicU64::new(0),
        inhibit: AtomicU64::new(0),
    });
}

/// How a read went in, in the low bits of `velt_rt_rwlock_read`'s result (the rest is this
/// thread's `hazard::Reader`, so the unlock needs no thread-local lookup; a read callback never
/// awaits, so it ends on the thread it began on): named in its slot; holding the mutex; nested
/// in a read of the same lock.
const FAST: u8 = 0;
const TOOK: u8 = 1;
const INSIDE: u8 = 2;
const HOW: u64 = 7;

const _: () = assert!(std::mem::align_of::<crate::hazard::Reader>() > HOW as usize);

/// `rw.read(f)`: read the value beside other readers (waiting while a writer writes; callbacks
/// never await). Readers hold no module lock either (task/local/module_state.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read(p: *mut u64) -> u64 {
    let w = &*(p as *const Words);
    let r = crate::hazard::READER.with(|r| r as *const crate::hazard::Reader);
    // SAFETY: a thread-local without a destructor lives as long as its thread.
    let rr = &*r;
    let how = match rr.read_depth.get() {
        0 => read_outermost(rr, p, w),
        _ => read_slow(rr, p, w),
    };
    crate::task::local::mutex_locked();
    r as u64 | how as u64
}

/// A read in no other (the common case), kept short: biased, slot 0 names the lock; revoked,
/// the mutex, as a `Mutex` takes it.
#[inline(always)]
fn read_outermost(r: &crate::hazard::Reader, p: *mut u64, w: &Words) -> u8 {
    // A lock this thread last found revoked: the mutex at once (always safe), leaving `state`
    // unread, which would delay taking it.
    if r.revoked.get() != p as usize {
        if w.state.load(Ordering::Relaxed) == 0 {
            let s = r.slot(0);
            s.store(p, Ordering::SeqCst);
            if w.state.load(Ordering::SeqCst) == 0 {
                r.read_depth.set(1);
                return FAST;
            }
            // A writer revokes the bias and may wait for this slot (the read has read nothing).
            s.store(std::ptr::null_mut(), Ordering::Release);
        }
        r.revoked.set(p as usize);
    }
    // A read that waited for the mutex looks at the bias at once (waiting cost more than the
    // clock; long readers waiting on each other is what the bias is for), others on every 64th.
    let waited = !grab(&w.lock);
    if waited {
        lock_slow(&w.lock);
    }
    r.read_depth.set(1);
    r.holds.set(p as usize);
    let n = r.took.get().wrapping_add(1);
    r.took.set(n);
    if waited || n.is_multiple_of(64) {
        look(r, w);
    }
    TOOK
}

/// An outermost read holding a mutex, after waiting for it or on every 64th: is the bias back
/// (then no longer go to the mutex at once), or due back (`rebias`)?
#[cold]
#[inline(never)]
fn look(r: &crate::hazard::Reader, w: &Words) {
    if w.state.load(Ordering::Relaxed) == 0 || rebias(w) {
        r.revoked.set(0);
    }
}

/// `lock`'s own fast path: one compare-and-swap of its byte from free to locked. parking_lot's
/// `try_lock` reads the byte first, which under contention moves the line twice.
#[inline]
fn grab(lock: &RawMutex) -> bool {
    // SAFETY: a parking_lot `RawMutex` is one `AtomicU8` whose bit 0 says locked (asserted in
    // the tests); `unlock` releases what this takes as it releases `lock`'s.
    let b = unsafe { std::sync::atomic::AtomicU8::from_ptr(lock as *const RawMutex as *mut u8) };
    b.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// A nested read holding the mutex: its depth and which lock it holds (in its slot, which nested
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

/// `lock` is busy: wait for it.
#[cold]
#[inline(never)]
fn lock_slow(lock: &RawMutex) {
    lock.lock();
}

/// A reader holding the mutex of a revoked `RwLock`: after `until`, bias it again (true).
fn rebias(w: &Words) -> bool {
    let now = now_ns();
    if now < w.until.load(Ordering::Relaxed) {
        return false;
    }
    w.until.store(now, Ordering::Relaxed);
    w.state.store(0, Ordering::Release);
    true
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
    w.lock.lock();
    took(r, p)
}

/// The end of the innermost `velt_rt_rwlock_read`, which returned `how`. A read holding the
/// mutex releases it last, as `Mutex` does: a thread that unlocks and locks again at once keeps
/// it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read_unlock(p: *mut u64, how: u64) {
    crate::task::local::mutex_unlocked();
    // SAFETY: `velt_rt_rwlock_read` on this thread returned it (see `HOW`).
    let r = &*((how & !HOW) as *const crate::hazard::Reader);
    let how = (how & HOW) as u8;
    let v = r.read_depth.get() - 1;
    if v != 0 {
        read_unlock_nested(r, how);
    } else {
        r.read_depth.set(0);
        match how {
            // A writer waits for a slot naming `p` itself: clearing it publishes the read.
            FAST => r.slot(0).store(std::ptr::null_mut(), Ordering::Release),
            _ => r.holds.set(0),
        }
    }
    if how == TOOK {
        (*(p as *const Words)).lock.unlock();
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

/// `rw.write(f)`: take the lock alone, exactly as a `Mutex` is taken. A write inside a `read`
/// callback of the same lock waits for itself, as with any lock (one that went ahead of the
/// mutex stops the program, `revoke`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write(p: *mut u64) {
    let w = &*(p as *const Words);
    w.lock.lock();
    if w.state.load(Ordering::Relaxed) == 0 {
        revoke(p, w);
    }
    crate::task::local::mutex_locked();
    crate::read_mode::enter_exclusive();
}

/// A writer holding `lock` of a biased `RwLock`: readers take `lock` from now on; wait for the
/// ones in slots, and keep the bias revoked for a while.
#[cold]
fn revoke(p: *mut u64, w: &Words) {
    let mine = crate::hazard::READER.with(|r| r.reads(p));
    if mine {
        crate::panic::fatal("deadlock: `write` on an `RwLock` inside its own `read` callback");
    }
    w.state.store(MUTEX, Ordering::SeqCst);
    let t0 = now_ns();
    let mut spins = 0u32;
    while crate::hazard::named(p) {
        spins += 1;
        match spins < 64 {
            true => std::hint::spin_loop(),
            false => std::thread::yield_now(),
        }
    }
    let t1 = now_ns();
    let biased_for = t0.saturating_sub(w.until.load(Ordering::Relaxed));
    let last = w.inhibit.load(Ordering::Relaxed);
    let floor = 9 * (t1 - t0).max(1000);
    let inhibit = match biased_for < last {
        true => (last * 2).min(MAX_INHIBIT),
        false => last / 2,
    }
    .max(floor);
    w.inhibit.store(inhibit, Ordering::Relaxed);
    w.until.store(t1 + inhibit, Ordering::Relaxed);
}

/// Release the lock this thread took with `velt_rt_rwlock_write`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write_unlock(p: *mut u64) {
    crate::read_mode::leave_exclusive();
    crate::task::local::mutex_unlocked();
    (*(p as *const Words)).lock.unlock();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(how: u64) -> u8 {
        (how & HOW) as u8
    }

    #[test]
    fn rwlock_readers_share_and_writers_exclude() {
        let mut l = [1u64; RWLOCK_WORDS];
        let p = l.as_mut_ptr();
        let held = |p: *mut u64| unsafe { !(*(p as *const RawMutex)).try_lock() };
        unsafe {
            velt_rt_rwlock_init(p);
            let a = velt_rt_rwlock_read(p);
            let b = velt_rt_rwlock_read(p);
            assert_eq!((tag(a), tag(b)), (FAST, INSIDE));
            // Biased: named in this thread's slot, the mutex free.
            assert!(crate::hazard::named(p));
            assert!(!held(p));
            (*(p as *const RawMutex)).unlock();
            velt_rt_rwlock_read_unlock(p, b);
            velt_rt_rwlock_read_unlock(p, a);
            assert!(!crate::hazard::named(p));
            // The first write revokes the bias: readers take the mutex.
            velt_rt_rwlock_write(p);
            assert_eq!(l[16], MUTEX);
            velt_rt_rwlock_write_unlock(p);
            let a = velt_rt_rwlock_read(p);
            let b = velt_rt_rwlock_read(p);
            assert_eq!((tag(a), tag(b)), (TOOK, INSIDE));
            assert!(held(p));
            velt_rt_rwlock_read_unlock(p, b);
            velt_rt_rwlock_read_unlock(p, a);
            assert!(!held(p));
            (*(p as *const RawMutex)).unlock();
            // After `until`, a reader biases it again.
            l[17] = 0;
            for _ in 0..64 {
                let a = velt_rt_rwlock_read(p);
                velt_rt_rwlock_read_unlock(p, a);
            }
            assert_eq!(l[16], 0);
            let a = velt_rt_rwlock_read(p);
            assert_eq!(tag(a), FAST);
            velt_rt_rwlock_read_unlock(p, a);
        }
    }

    #[test]
    fn grab_takes_a_parking_lot_mutex() {
        assert_eq!(std::mem::size_of::<RawMutex>(), 1);
        let m = RawMutex::INIT;
        assert!(grab(&m) || grab(&m));
        assert!(!m.try_lock());
        assert!(m.is_locked());
        unsafe { m.unlock() };
        assert!(m.try_lock());
        assert!(!grab(&m));
        unsafe { m.unlock() };
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
                            velt_rt_rwlock_write(p);
                            let v = std::ptr::read_volatile(&(*c).value[0]);
                            std::ptr::write_volatile(&mut (*c).value[0], v + 1);
                            std::ptr::write_volatile(&mut (*c).value[1], v + 1);
                            velt_rt_rwlock_write_unlock(p);
                        } else {
                            let how = velt_rt_rwlock_read(p);
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
}
