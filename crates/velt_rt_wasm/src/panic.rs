//! Panics and exit (rt_abi.md "Control"): `velt_rt_panic`, `velt_rt_exit`, the throw-location
//! slot, and the hook that reports internal Rust panics as `panic: <msg>` with exit code 101.
//! Same messages and codes as velt_rt, routed through [`crate::platform`].

use std::cell::Cell;

use crate::io;
use crate::platform;
use crate::str::VeltStr;

/// Exit code used for panics (same as Rust and velt_rt).
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
    io::flush_stdout();
    platform::write(2, &panic_message(msg));
    platform::exit(PANIC_EXIT_CODE)
}

/// Internal fatal runtime error (bad ABI arguments, unsupported operation): reported like a
/// Velt panic.
#[cold]
pub fn fatal(msg: &str) -> ! {
    die(msg.as_bytes())
}

/// Report Rust panics inside the runtime as `panic: <msg>` and exit 101.
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
        die(msg.as_bytes())
    }));
}

/// `panic(msg)` and compiler-emitted checks: flush stdout, print `panic: <msg>`, exit 101.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_panic(msg: *const VeltStr) -> ! {
    let bytes: &[u8] = if msg.is_null() {
        b""
    } else {
        (*msg).as_bytes()
    };
    die(bytes)
}

/// `process.exit(code)`: flush stdout and stop.
#[no_mangle]
pub extern "C" fn velt_rt_exit(code: i32) -> ! {
    io::flush_stdout();
    platform::exit(code)
}

thread_local! {
    /// Location suffix (` at <path>:<line>:<col>`, a static string) of the last `throw`.
    static THROW_LOC: Cell<*const VeltStr> = const { Cell::new(std::ptr::null()) };
}

/// Record where the error being thrown now comes from (a static string, or null).
#[no_mangle]
pub extern "C" fn velt_rt_set_throw_loc(loc: *const VeltStr) {
    THROW_LOC.with(|c| c.set(loc));
}

/// The location recorded by the last [`velt_rt_set_throw_loc`] (null if none).
#[no_mangle]
pub extern "C" fn velt_rt_throw_loc() -> *const VeltStr {
    THROW_LOC.with(|c| c.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throw_location_slot() {
        let s = VeltStr::from_static(b" at a.vlt:1:2");
        assert!(velt_rt_throw_loc().is_null());
        velt_rt_set_throw_loc(&s);
        assert_eq!(velt_rt_throw_loc(), &s as *const _);
        velt_rt_set_throw_loc(std::ptr::null());
        assert!(velt_rt_throw_loc().is_null());
    }

    #[test]
    fn message_format() {
        assert_eq!(panic_message(b"boom"), b"panic: boom\n");
    }
}
