//! Unsettled top-level `await`, as in Node: when the program's top-level statements wait at an
//! `await` that nothing can settle any more, Node writes `Warning: Detected unsettled top-level
//! await at <file>:<line>` and exits with code 13.
//!
//! The generated `main` of a program whose top-level statements `await` (modules.md) calls
//! [`velt_rt_tla_at`] with the `await`'s location before it evaluates its operand and suspends;
//! an `await` inside another's operand stores the outer location again once it has its value
//! (velt_sema `top_level_await.rs`). So the location stored last is the `await` that waits now.
//! The store is two atomic writes of a literal's static bytes: no lock, no allocation, as a
//! top-level loop over `await` runs it on every iteration. Shared with velt_rt_wasm (`#[path]`).

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::str::VeltStr;

/// The static bytes of the location (`path:line:col`) of the top-level `await` that suspended
/// last; null before any.
static SITE: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static SITE_LEN: AtomicUsize = AtomicUsize::new(0);

/// A top-level `await` at `site` (`path:line:col`, a string literal) is about to suspend (or,
/// stored again, waits next).
///
/// # Safety
/// `site` must be a valid string. Only a static one (a literal) is kept: its bytes live for the
/// whole process.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tla_at(site: *const VeltStr) {
    let site = &*site;
    if !site.is_static() {
        return;
    }
    let bytes = site.as_bytes();
    SITE_LEN.store(bytes.len(), Ordering::Relaxed);
    SITE.store(bytes.as_ptr() as *mut u8, Ordering::Release);
}

/// The location the top-level `await` that suspended last stored, if any.
fn waiting_at() -> Option<&'static [u8]> {
    let ptr = SITE.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let len = SITE_LEN.load(Ordering::Relaxed);
    // SAFETY: `velt_rt_tla_at` stores only a static string's bytes, which live for the process;
    // only the root task stores, and it is stuck when this runs.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// The program ends with `main` unfinished: if it waits at a top-level `await`, Node's warning
/// (written to stderr in one write) and exit code 13; else 0, as Node exits with an `async
/// main`'s promise pending.
pub fn unfinished_exit_code() -> i32 {
    let Some(site) = waiting_at() else {
        return 0;
    };
    let site = String::from_utf8_lossy(site);
    // `path:line:col`: Node names the line.
    let at = match site.rsplit_once(':') {
        Some((head, col)) if col.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => &site,
    };
    crate::io::flush_stdout();
    let line = format!("Warning: Detected unsettled top-level await at {at}\n");
    crate::io::write_bytes(2, line.as_bytes());
    13
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_await_waiting_now_is_the_one_stored_last() {
        assert_eq!(waiting_at(), None);
        let (outer, inner) = (
            VeltStr::from_static(b"main.vlt:3:1"),
            VeltStr::from_static(b"main.vlt:3:20"),
        );
        let heap = VeltStr::from_vec(b"main.vlt:9:1".to_vec());
        // SAFETY: valid strings.
        unsafe {
            // `await f(await g())`: the outer, the inner, then the outer again.
            velt_rt_tla_at(&outer);
            velt_rt_tla_at(&inner);
            assert_eq!(waiting_at(), Some(&b"main.vlt:3:20"[..]));
            velt_rt_tla_at(&outer);
            assert_eq!(waiting_at(), Some(&b"main.vlt:3:1"[..]));
            // Not a literal: not kept.
            velt_rt_tla_at(&heap);
            assert_eq!(waiting_at(), Some(&b"main.vlt:3:1"[..]));
        }
    }
}
