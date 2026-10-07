//! Panics and process exit: `velt_rt_panic`, `velt_rt_exit`, plus the hook that turns internal
//! Rust panics into the same `panic: <msg>` / exit 101 behavior. No Rust panic ever unwinds across
//! the C ABI: every exported function is `extern "C"` (non-unwinding, aborts as a last resort), and
//! the hook installed by [`install_hook`] exits the process before unwinding starts.
//!
//! Also the per-thread "where was the last error thrown" slot (`velt_rt_set_throw_loc` /
//! `velt_rt_throw_loc`) that compiled code fills at each `throw` and reads when it reports an
//! uncaught error (`Uncaught E: msg at file.vlt:3:5`). A task's error can be read on another
//! thread than the one that threw it (`async main` runs on a worker, a spawned task's handle is
//! awaited anywhere), so finished tasks carry the slot to whoever takes their result
//! ([`ThrowLoc`]). Each poll of a task starts with an empty slot, so a task that threw nothing in
//! its last poll hands over no location and leaves the taker's own in place. Known limits (the
//! slot is "the last throw on this thread", not part of the error value): a task that throws,
//! then awaits before its error leaves it (in a `finally`), reports no location; and a task that
//! threw and caught an error in its last poll hands that location to a taker that is itself
//! propagating an error (#356).

use std::cell::Cell;

use crate::io;
use crate::str::VeltStr;

/// Exit code used for panics (same as Rust).
pub const PANIC_EXIT_CODE: i32 = 101;

/// Bytes printed to stderr for a panic with message `msg`.
pub fn panic_message(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 8);
    out.extend_from_slice(b"panic: ");
    out.extend_from_slice(msg);
    out.push(b'\n');
    out
}

fn die(msg: &[u8]) -> ! {
    use std::io::Write;
    io::try_flush_stdout();
    let _ = std::io::stderr().lock().write_all(&panic_message(msg));
    std::process::exit(PANIC_EXIT_CODE)
}

/// Internal fatal runtime error (bad ABI arguments etc.): reported like a Velt panic.
#[cold]
pub fn fatal(msg: &str) -> ! {
    die(msg.as_bytes())
}

/// Make Rust panics inside the runtime (bugs, or M3 tasks) report `panic: <msg>` and exit 101
/// instead of unwinding into generated code.
pub fn install_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        let msg = match info.location() {
            Some(l) => format!("{msg} (runtime: {}:{})", l.file(), l.line()),
            None => msg,
        };
        die(msg.as_bytes())
    }));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_panic(msg: *const VeltStr) -> ! {
    if msg.is_null() {
        die(b"")
    }
    die((*msg).text_lossy().as_bytes())
}

thread_local! {
    /// Location suffix (` at <path>:<line>:<col>`, a static string) of the last `throw`.
    static THROW_LOC: Cell<*const VeltStr> = const { Cell::new(std::ptr::null()) };
}

/// Record where the error being thrown now comes from. `loc` is a static string (compiled
/// code passes read-only data) or null for "unknown".
#[no_mangle]
pub extern "C" fn velt_rt_set_throw_loc(loc: *const VeltStr) {
    THROW_LOC.with(|c| c.set(loc));
}

/// The location recorded by the last [`velt_rt_set_throw_loc`] on this thread (null if none).
#[no_mangle]
pub extern "C" fn velt_rt_throw_loc() -> *const VeltStr {
    THROW_LOC.with(|c| c.get())
}

/// The throw location of one thread, carried to another with a task's result.
#[derive(Clone, Copy)]
pub(crate) struct ThrowLoc(*const VeltStr);

// SAFETY: compiled code records only static strings (read-only data) or null.
unsafe impl Send for ThrowLoc {}

impl ThrowLoc {
    /// This thread's location (of the last `throw` here).
    pub(crate) fn current() -> ThrowLoc {
        ThrowLoc(velt_rt_throw_loc())
    }

    /// Make it this thread's location.
    pub(crate) fn restore(self) {
        velt_rt_set_throw_loc(self.0);
    }

    /// Make it this thread's location if it is known (a task that threw nothing keeps the
    /// taker's location).
    pub(crate) fn restore_if_known(self) {
        if !self.0.is_null() {
            velt_rt_set_throw_loc(self.0);
        }
    }

    /// Forget this thread's location (a task poll starts with none).
    pub(crate) fn clear() {
        velt_rt_set_throw_loc(std::ptr::null());
    }
}

#[no_mangle]
pub extern "C" fn velt_rt_exit(code: i32) -> ! {
    io::flush_stdout();
    crate::str::stats::report();
    io::stats::report();
    #[cfg(all(debug_assertions, not(velt_rt_host)))]
    crate::debug_alloc::check_quarantine();
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    #[test]
    fn throw_location_is_per_thread() {
        let s = crate::str::VeltStr::empty();
        assert!(super::velt_rt_throw_loc().is_null());
        super::velt_rt_set_throw_loc(&s);
        assert_eq!(super::velt_rt_throw_loc(), &s as *const _);
        std::thread::spawn(|| assert!(super::velt_rt_throw_loc().is_null()))
            .join()
            .unwrap();
        super::velt_rt_set_throw_loc(std::ptr::null());
        assert!(super::velt_rt_throw_loc().is_null());
    }

    #[test]
    fn message_format() {
        assert_eq!(
            super::panic_message(b"division by zero"),
            b"panic: division by zero\n"
        );
    }
}
