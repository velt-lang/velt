//! Primitives behind `shared<T>`: reference counts (`velt_rt_rc_*`), atomic integers
//! (`velt_rt_atomic_*`) and the `Mutex<T>` lock (`velt_rt_mutex_*`).
//!
//! All operate on memory owned by generated code (a refcount word, an `i64` cell, an 8-byte lock
//! word inside the shared object), so none of them allocates.

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

/// Lock (blocking the thread; `with` bodies never await, so holds are short).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock(p: *mut u64) {
    (*(p as *const Lock)).lock();
}

/// Unlock a mutex locked by this thread's `velt_rt_mutex_lock`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock(p: *mut u64) {
    (*(p as *const Lock)).unlock();
}

#[cfg(test)]
mod tests {
    use super::*;

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
