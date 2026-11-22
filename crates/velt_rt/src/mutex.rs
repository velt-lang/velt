//! The lock of `Mutex<T>` (`velt_rt_mutex_*`), on an 8-byte word inside the shared object that
//! generated code owns, so it never allocates. On a native target an uncontended lock and
//! unlock are std's (std/lang/hooks/mutex.velt: a compare-and-swap of the word's first 32 bits),
//! which calls the slow paths here (`velt_rt_mutex_lock_slow`, `velt_rt_mutex_unlock_slow`) when
//! the lock is held or threads are parked on it.

use std::sync::atomic::{AtomicU32, Ordering};

/// Size of the lock word generated code reserves inside a `Mutex<T>` object (align 8).
pub const MUTEX_SIZE: usize = 8;

/// The lock of a `Mutex<T>`: 32 bits (std's compare-and-swap is 32 or 64 bits wide), with
/// `parking_lot`'s two bits (a locked bit, and a bit saying that threads are parked on it) and a
/// different way of waiting. `with` bodies never await and are short, so a thread that finds the
/// lock taken spins briefly without leaving the CPU and then parks. `parking_lot` yields the
/// thread between the two, up to seven times: each a system call, and under contention (many
/// tasks sending to the same WebSocket connections from several threads) that was most of what
/// the waiting threads did.
struct Lock(AtomicU32);

const LOCKED: u32 = 1;
const PARKED: u32 = 2;
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

    /// The unlock std's `mutexUnlock` does inline (the runtime's tests use this one).
    #[cfg(test)]
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

/// The lock's slow path (std/lang/hooks/mutex.velt `mutexLock`, whose compare-and-swap found it
/// held): spin briefly, then park until it is free or handed over (blocking the thread; `with`
/// bodies never await, so holds are short).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock_slow(p: *mut u64) {
    (*(p as *const Lock)).lock_slow();
}

/// The unlock's slow path (std/lang/hooks/mutex.velt `mutexUnlock`, whose compare-and-swap found
/// threads parked on the lock): wake one, or hand the lock to it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock_slow(p: *mut u64) {
    (*(p as *const Lock)).unlock_slow();
}

/// The lock from code that may run in a `read` callback (velt_vir: code not known to run outside
/// one), where a `with` stops the program: the callback shares the `RwLock`'s value without
/// counting it, which a `with` callback could not tell from what it changes. Its unlock is std's
/// (`mutexUnlock`), as any `with`'s.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock_checked(p: *mut u64) {
    crate::read_mode::refuse_in_read("with");
    lock(p);
}

/// Lock the `Mutex` whose lock word is at `p`, as std's `mutexLock` does (the runtime's tests).
pub(crate) unsafe fn lock(p: *mut u64) {
    (*(p as *const Lock)).lock();
}

/// Unlock it, as std's `mutexUnlock` does.
#[cfg(test)]
pub(crate) unsafe fn unlock(p: *mut u64) {
    (*(p as *const Lock)).unlock();
}
