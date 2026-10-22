//! Unsettled top-level `await`, as in Node: when the program's top-level statements wait at an
//! `await` that nothing can settle any more, Node writes `Warning: Detected unsettled top-level
//! await at <file>:<line>` and exits with code 13.
//!
//! The generated `main` of a program whose top-level statements `await` (modules.md) calls
//! [`velt_rt_tla_enter`] with the `await`'s location and depth (how many top-level `await`s'
//! operands hold it) before it waits, and [`velt_rt_tla_leave`] after its value arrives. The
//! entry left last is the `await` that waits now: an inner `await` leaves before the `await`
//! holding it waits, and the entry an `await` that threw left behind is replaced by the next one
//! entered at its depth or above. Shared with velt_rt_wasm (`#[path]`).

use std::sync::Mutex;

use crate::str::VeltStr;

/// The locations (`path:line:col`) of the top-level `await`s waiting now, by depth.
static WAITING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A top-level `await` at `site` (`path:line:col`), inside the operands of `depth` others, is
/// about to wait.
///
/// # Safety
/// `site` must be a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tla_enter(site: *const VeltStr, depth: i64) {
    let site = String::from_utf8_lossy((*site).as_bytes()).into_owned();
    if let Ok(mut w) = WAITING.lock() {
        w.truncate(depth.max(0) as usize);
        w.push(site);
    }
}

/// The top-level `await` entered at `depth` has its value.
#[no_mangle]
pub extern "C" fn velt_rt_tla_leave(depth: i64) {
    if let Ok(mut w) = WAITING.lock() {
        w.truncate(depth.max(0) as usize);
    }
}

/// The program ends with `main` unfinished: if it waits at a top-level `await`, Node's warning
/// (written to stderr in one write) and exit code 13; else 0, as Node exits with an `async
/// main`'s promise pending.
pub fn unfinished_exit_code() -> i32 {
    let site = WAITING.lock().ok().and_then(|w| w.last().cloned());
    let Some(site) = site else {
        return 0;
    };
    // `path:line:col`: Node names the line.
    let at = match site.rsplit_once(':') {
        Some((head, col)) if col.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => site.as_str(),
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
    fn the_await_waiting_now_is_the_entry_left_last() {
        assert_eq!(unfinished_exit_code(), 0);
        let last = || WAITING.lock().unwrap().last().cloned();
        let (outer, inner, next) = (
            VeltStr::from_static(b"main.vlt:3:1"),
            VeltStr::from_static(b"main.vlt:4:3"),
            VeltStr::from_static(b"main.vlt:6:1"),
        );
        // SAFETY: valid strings.
        unsafe {
            velt_rt_tla_enter(&outer, 0);
            velt_rt_tla_enter(&inner, 1);
            assert_eq!(last().as_deref(), Some("main.vlt:4:3"));
            velt_rt_tla_leave(1);
            assert_eq!(last().as_deref(), Some("main.vlt:3:1"));
            // The inner one threw: its entry stays until the next `await` replaces it.
            velt_rt_tla_enter(&inner, 1);
            velt_rt_tla_enter(&next, 0);
            assert_eq!(last().as_deref(), Some("main.vlt:6:1"));
        }
        velt_rt_tla_leave(0);
        assert_eq!(last(), None);
    }
}
