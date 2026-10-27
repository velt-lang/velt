//! Primitives behind `shared<T>` (rt_abi_async.md §9): reference counts, atomic integers and
//! the `Mutex<T>` lock word. The WebAssembly runtime runs one thread, so these are plain memory
//! operations; a lock word is 1 while held, and locking a held lock is a deadlock (reported).

/// `shared.clone()`: increment the refcount at `p`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_inc(p: *mut u64) {
    *p = (*p)
        .checked_add(1)
        .unwrap_or_else(|| crate::panic::fatal("reference count overflow"));
}

/// Drop one reference: 1 if it was the last one (the caller drops and frees), else 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_dec(p: *mut u64) -> u8 {
    *p -= 1;
    u8::from(*p == 0)
}

/// Initialize the 8-byte lock word at `p` (unlocked).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_init(p: *mut u64) {
    *p = 0;
}

/// Lock. With one thread a held lock can never be released while we wait.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock(p: *mut u64) {
    if *p != 0 {
        crate::panic::fatal("deadlock: Mutex locked again while held");
    }
    *p = 1;
}

/// [`velt_rt_mutex_lock`] where the code may run in a `read` callback (velt_rt).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock_checked(p: *mut u64) {
    refuse_in_read("with");
    velt_rt_mutex_lock(p)
}

/// Unlock.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock(p: *mut u64) {
    *p = 0;
}

/// Initialize the lock word of an `RwLock<T>` (0: free; n > 0: n readers; `u64::MAX`: a writer).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_init(p: *mut u64) {
    *p = 0;
}

/// Shared lock. With one thread a writer can never release while we wait.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read(p: *mut u64, _reader: u64) -> u64 {
    if *p == u64::MAX {
        crate::panic::fatal("deadlock: RwLock read while written");
    }
    *p += 1;
    READ_DEPTH += 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_read_unlock(p: *mut u64, _how: u64) {
    READ_DEPTH -= 1;
    *p -= 1;
    if READ_DEPTH == 0
        && !((*std::ptr::addr_of!(WAITING)).is_empty() && (*std::ptr::addr_of!(LEAVING)).is_empty())
    {
        free_waiting();
    }
}

/// Exclusive lock.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write(p: *mut u64, _reader: u64) -> u64 {
    if *p != 0 && *p != u64::MAX && in_read() {
        crate::panic::fatal("deadlock: `write` on an `RwLock` inside its own `read` callback");
    }
    refuse_in_read("write");
    if *p != 0 {
        crate::panic::fatal("deadlock: RwLock written while held");
    }
    *p = u64::MAX;
    0
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write_unlock(p: *mut u64, _how: u64) {
    *p = 0;
}

// Read mode (velt_rt `read_mode`): the compiled code counts by its rules inline; here only the
// mode itself (one thread: a depth) and the checks.
static mut READ_DEPTH: u32 = 0;
const LOCAL: u64 = 1 << 62;
/// velt_rt `read_mode::STORED`.
const STORED: u64 = 1 << 61;

/// A drop glue function: releases the reference at the pointer it is given.
type DropGlue = unsafe extern "C" fn(*mut *mut u8);

/// Objects whose last reference went in read mode, freed when the reads end (velt_rt
/// `read_mode::velt_rt_free_after_reads`).
static mut WAITING: Vec<(*mut u8, DropGlue)> = Vec::new();

/// Free the lock-set object `p` (count 1) with its drop glue now, or once the reads end
/// (velt_rt `read_mode::velt_rt_free_after_reads`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_free_after_reads(p: *mut u8, drop: DropGlue) {
    if !in_read() {
        let mut slot = p;
        return drop(&mut slot);
    }
    (*std::ptr::addr_of_mut!(WAITING)).push((p, drop));
}

#[cold]
unsafe fn free_waiting() {
    while let Some((c, p, drop)) = (*std::ptr::addr_of_mut!(LEAVING)).pop() {
        *c &= !(LOCAL | STORED);
        let mut slot = p;
        drop(&mut slot);
    }
    while let Some((p, drop)) = (*std::ptr::addr_of_mut!(WAITING)).pop() {
        let mut slot = p;
        drop(&mut slot);
    }
}

unsafe fn in_read() -> bool {
    READ_DEPTH != 0
}

/// A promise or task starting in read mode stops the program (velt_rt `read_mode`).
pub(crate) fn refuse_tasks_in_read() {
    // SAFETY: one thread.
    if unsafe { in_read() } {
        crate::panic::fatal(
            "a promise or task cannot start inside a `read` callback: what it holds would outlive the callback, which shares the `RwLock`'s value without counting it (start it after `read`, with what `read` returns)",
        );
    }
}

/// A `with` or `write` (`method`) in read mode stops the program (velt_rt `read_mode`).
unsafe fn refuse_in_read(method: &str) {
    if in_read() {
        crate::panic::fatal(&format!(
            "`{method}` cannot run inside a `read` callback, nor what locks (a promise's resolver, clearing a timer): the callback shares the `RwLock`'s value with other readers without counting it, which `{method}` could not tell from what it changes (return what you need from `read` and lock, resolve or clear after it)"
        ));
    }
}
#[no_mangle]
pub extern "C" fn velt_rt_lk_mode() -> u8 {
    // SAFETY: one thread.
    u8::from(unsafe { in_read() })
}

/// velt_rt `read_mode::velt_rt_lk_leave`: in a nested read, the bit goes when the reads end.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_leave(p: *mut u64, obj: *mut u8, drop: DropGlue) {
    if READ_DEPTH as u64 > LEAVE_TO {
        return;
    }
    if READ_DEPTH <= 1 {
        if !leaving(p) {
            *p &= !LOCAL;
        }
        return;
    }
    *p += 1;
    (*std::ptr::addr_of_mut!(LEAVING)).push((p, obj, drop));
}

/// velt_rt `read_mode::velt_rt_lk_keep_made`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_keep_made(p: *mut u64) {
    if (READ_DEPTH <= 1 || READ_DEPTH as u64 > LEAVE_TO) && !leaving(p) {
        *p = 1;
    }
}

/// velt_rt `read_mode::velt_rt_lk_share_made`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_share_made(p: *mut u64, obj: *mut u8, drop: DropGlue) {
    if READ_DEPTH as u64 > LEAVE_TO || leaving(p) {
        return;
    }
    *p = (*p | STORED) + 1;
    (*std::ptr::addr_of_mut!(LEAVING)).push((p, obj, drop));
}

/// velt_rt `read_mode::leaving`.
unsafe fn leaving(p: *mut u64) -> bool {
    (*std::ptr::addr_of!(LEAVING))
        .iter()
        .any(|&(c, _, _)| c == p)
}

/// Objects a nested read stored outside every read (velt_rt `read_mode::LEAVING`).
static mut LEAVING: Vec<(*mut u64, *mut u8, DropGlue)> = Vec::new();

/// velt_rt `read_mode::velt_rt_lk_leave_to` (one thread).
static mut LEAVE_TO: u64 = 1;

/// velt_rt `read_mode::velt_rt_lk_leave_to`.
#[no_mangle]
pub extern "C" fn velt_rt_lk_leave_to(levels: u64) -> u64 {
    // SAFETY: one thread.
    unsafe { std::mem::replace(&mut *std::ptr::addr_of_mut!(LEAVE_TO), levels) }
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_write(p: *mut u64) {
    if in_read() && *p & STORED != 0 {
        crate::panic::fatal("cannot change an object a `read` callback made after storing it outside the callback: it is the caller's from then on, as the same object (store it once it is complete, or change it through the caller's container after `read`)");
    }
    if in_read() && *p & LOCAL == 0 {
        crate::panic::fatal(
            "cannot modify a value an `RwLock` holds inside its `read` callback (other readers read it at the same time): change it in `write`",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_atomics_and_locks() {
        let mut rc = 1u64;
        let mut lock = 7u64;
        unsafe {
            velt_rt_rc_inc(&mut rc);
            assert_eq!(velt_rt_rc_dec(&mut rc), 0);
            assert_eq!(velt_rt_rc_dec(&mut rc), 1);
            velt_rt_mutex_init(&mut lock);
            velt_rt_mutex_lock(&mut lock);
            assert_eq!(lock, 1);
            velt_rt_mutex_unlock(&mut lock);
            assert_eq!(lock, 0);
        }
    }
}
