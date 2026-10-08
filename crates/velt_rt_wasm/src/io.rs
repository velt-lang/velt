//! Output: `velt_rt_write_*` and `velt_rt_flush` (rt_abi.md "Output").
//!
//! Single-threaded, so one stdout buffer suffices: it reaches the host when it fills, before
//! any stderr write (so interleaved output keeps its order), on `velt_rt_flush`, and at exit.
//! stderr is unbuffered.

use std::sync::Mutex;

use crate::fmt;
use crate::platform;
use crate::str::VeltStr;

/// Buffer size at which stdout is written through.
const CAP: usize = 64 * 1024;

static STDOUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn with_stdout<R>(f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
    // A poisoned lock only means a panic mid-write; the bytes are still valid.
    let mut buf = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut buf)
}

/// Write buffered stdout to the host.
pub fn flush_stdout() {
    #[cfg(test)]
    if capture::active() {
        return;
    }
    let bytes = with_stdout(std::mem::take);
    platform::write(1, &bytes);
}

fn emit(stream: u32, f: impl FnOnce(&mut Vec<u8>)) {
    #[cfg(test)]
    if capture::active() {
        capture::append(stream, f);
        return;
    }
    if stream == 2 {
        let mut local = Vec::with_capacity(64);
        f(&mut local);
        flush_stdout();
        platform::write(2, &local);
        return;
    }
    let full = with_stdout(|b| {
        f(b);
        b.len() >= CAP
    });
    if full {
        flush_stdout();
    }
}

/// Append raw bytes to `stream`.
pub fn write_bytes(stream: u32, bytes: &[u8]) {
    emit(stream, |b| b.extend_from_slice(bytes));
}

/// `console.log` of a string.
/// `stdout.write(bytes)` (std/process): raw bytes to stdout, in order with `console.log`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_stdout_write_bytes(b: *const crate::bytes::VeltBytes) {
    write_bytes(1, (*b).as_bytes());
}

/// Write a string: its bytes as they are when well-formed, else with one U+FFFD per lone
/// surrogate (#377).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_write_str(stream: u32, s: *const VeltStr) {
    emit(stream, |b| (*s).extend_utf8(b));
}

/// `console.log` of an `i64` (decimal).
#[no_mangle]
pub extern "C" fn velt_rt_write_i64(stream: u32, v: i64) {
    emit(stream, |b| fmt::push_i64(b, v));
}

/// `console.log` of a `u64` (decimal).
#[no_mangle]
pub extern "C" fn velt_rt_write_u64(stream: u32, v: u64) {
    emit(stream, |b| fmt::push_u64(b, v));
}

/// `console.log` of an `f64` (JS formatting).
#[no_mangle]
pub extern "C" fn velt_rt_write_f64(stream: u32, v: f64) {
    emit(stream, |b| fmt::push_f64(b, v));
}

/// `console.log` of a `bool`.
#[no_mangle]
pub extern "C" fn velt_rt_write_bool(stream: u32, v: u8) {
    emit(stream, |b| fmt::push_bool(b, v));
}

/// One raw byte (separators and newlines of `console.log`).
#[no_mangle]
pub extern "C" fn velt_rt_write_byte(stream: u32, b: u8) {
    emit(stream, |buf| buf.push(b));
}

/// Flush buffered stdout.
#[no_mangle]
pub extern "C" fn velt_rt_flush() {
    flush_stdout();
}

/// Test-only output capture (per thread), like velt_rt's.
#[cfg(test)]
pub(crate) mod capture {
    use std::cell::RefCell;

    thread_local! {
        static CAP: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    pub fn active() -> bool {
        CAP.with(|c| c.borrow().is_some())
    }

    pub fn append(stream: u32, f: impl FnOnce(&mut Vec<u8>)) {
        CAP.with(|c| {
            let mut c = c.borrow_mut();
            let buf = c.as_mut().expect("capture active");
            if stream == 2 {
                buf.extend_from_slice(b"[err]");
            }
            f(buf);
        })
    }

    /// Run `f` and return what it printed (stderr parts prefixed with `[err]`).
    pub fn run(f: impl FnOnce()) -> String {
        CAP.with(|c| *c.borrow_mut() = Some(Vec::new()));
        f();
        let out = CAP.with(|c| c.borrow_mut().take()).unwrap();
        String::from_utf8(out).unwrap()
    }
}

/// `isatty(fd)` (std/process): 1 if a standard stream is a terminal (WASI hosts may say so; in
/// the browser nothing is), 0 for any other descriptor.
#[no_mangle]
pub extern "C" fn velt_rt_isatty(fd: i32) -> u8 {
    use std::io::IsTerminal;
    let tty = match fd {
        0 => std::io::stdin().is_terminal(),
        1 => std::io::stdout().is_terminal(),
        2 => std::io::stderr().is_terminal(),
        _ => false,
    };
    tty as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_log_sequence() {
        let s = VeltStr::from_static(b"x =");
        let out = capture::run(|| {
            unsafe { velt_rt_write_str(1, &s) };
            velt_rt_write_byte(1, b' ');
            velt_rt_write_i64(1, -42);
            velt_rt_write_byte(1, b' ');
            velt_rt_write_u64(1, 7);
            velt_rt_write_byte(1, b' ');
            velt_rt_write_f64(1, 0.1 + 0.2);
            velt_rt_write_byte(1, b' ');
            velt_rt_write_bool(1, 1);
            velt_rt_write_byte(1, b'\n');
            velt_rt_write_f64(2, f64::NAN);
            velt_rt_flush();
        });
        assert_eq!(out, "x = -42 7 0.30000000000000004 true\n[err]NaN");
    }
}
