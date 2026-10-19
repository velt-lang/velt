//! Primitives behind `shared<T>`: reference counts (`velt_rt_rc_*`), atomic integers
//! (`velt_rt_atomic_*`), the `Mutex<T>` lock (`velt_rt_mutex_*`) and the `RwLock<T>` lock
//! (`velt_rt_rwlock_*`).
//!
//! All operate on memory owned by generated code (a refcount word, an `i64` cell, an 8-byte lock
//! word inside the shared object), so none of them allocates.

use lock_api::{RawMutex as _, RawRwLock as _};
use parking_lot::{RawMutex, RawRwLock};
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

const _: () = assert!(std::mem::size_of::<RawRwLock>() <= MUTEX_SIZE);
const _: () = assert!(std::mem::align_of::<RawRwLock>() <= 8);

/// Initialize the 8-byte lock word of an `RwLock<T>` at `p` (unlocked). parking_lot's lock
/// prefers writers (new readers wait while a writer waits) with eventual fairness, so neither
/// side starves. No destroy call is needed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_init(p: *mut u64) {
    (p as *mut RawRwLock).write(RawRwLock::INIT);
}

/// `rw.read(f)`: take the lock shared with other readers (blocking the thread; callbacks
/// never await). Readers hold no module lock either (task/local/module_state.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read(p: *mut u64) {
    (*(p as *const RawRwLock)).lock_shared();
    crate::task::local::mutex_locked();
    crate::read_mode::enter_read();
}

/// Release a shared lock this thread took with `velt_rt_rwlock_read`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read_unlock(p: *mut u64) {
    crate::read_mode::leave_read();
    crate::task::local::mutex_unlocked();
    (*(p as *const RawRwLock)).unlock_shared();
}

/// `rw.write(f)`: take the lock exclusively.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write(p: *mut u64) {
    (*(p as *const RawRwLock)).lock_exclusive();
    crate::task::local::mutex_locked();
    crate::read_mode::enter_exclusive();
}

/// Release the exclusive lock this thread took with `velt_rt_rwlock_write`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write_unlock(p: *mut u64) {
    crate::read_mode::leave_exclusive();
    crate::task::local::mutex_unlocked();
    (*(p as *const RawRwLock)).unlock_exclusive();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rwlock_readers_share_and_writers_exclude() {
        let mut w = 1u64;
        unsafe {
            velt_rt_rwlock_init(&mut w);
            velt_rt_rwlock_read(&mut w);
            velt_rt_rwlock_read(&mut w);
            assert!(!(*(&w as *const u64 as *const RawRwLock)).try_lock_exclusive());
            velt_rt_rwlock_read_unlock(&mut w);
            velt_rt_rwlock_read_unlock(&mut w);
            velt_rt_rwlock_write(&mut w);
            assert!(!(*(&w as *const u64 as *const RawRwLock)).try_lock_shared());
            velt_rt_rwlock_write_unlock(&mut w);
        }
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
