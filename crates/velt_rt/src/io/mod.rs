//! Output: `velt_rt_write_*` and `velt_rt_flush`.
//!
//! stdout is buffered (see `stdout.rs` for the per-thread/shared buffering scheme) and flushed when
//! the buffers fill, at exit, before any stderr write, on `velt_rt_flush`, when an async worker goes
//! idle, and at every newline when stdout is an interactive terminal. stderr is unbuffered: each
//! call formats into a small local buffer and writes it in one go.

mod stdout;

use crate::bytes::VeltBytes;
use crate::fmt;
use crate::str::VeltStr;
use std::io::Write;

/// Flush all buffered stdout (this thread's and the shared buffer).
pub fn flush_stdout() {
    #[cfg(test)]
    if capture::active() {
        return;
    }
    stdout::flush();
}

/// Best-effort flush for panic/abort paths: never blocks.
pub fn try_flush_stdout() {
    #[cfg(test)]
    if capture::active() {
        return;
    }
    stdout::try_flush();
}

/// Called after every poll of a compiled future by the runtime: makes this thread's output visible
/// in order before the task can continue on another thread.
pub fn publish_thread_output() {
    stdout::publish_local();
}

/// Called before this task hands work to another one, ahead of the change the other task sees:
/// spawning a task, a channel send or close, a receive that frees room in a bounded channel,
/// settling a `new Promise`, aborting a signal, a child leaving a task group. What this thread
/// printed so far becomes visible before anything the other task prints, which may run on another
/// worker at once.
pub fn publish_before_handoff() {
    stdout::publish_local();
}

/// Test probe for the hand-off points: buffer a line on this thread, and check later whether it
/// was published.
#[cfg(test)]
pub(crate) mod handoff_probe {
    /// Buffer a space in this thread's buffer (no newline: a terminal would publish a line at
    /// once).
    pub(crate) fn buffer_output() {
        super::stdout::append(|b| b.push(b' '));
        assert!(!published(), "the line stays buffered");
    }

    /// Whether this thread's buffer was published (it is empty).
    pub(crate) fn published() -> bool {
        super::stdout::local_len() == 0
    }
}

/// Runtime idle hook (a worker is about to park): write pending output to the OS.
pub fn flush_idle() {
    stdout::flush_idle();
}

fn os_write_stderr(bytes: &[u8]) {
    let _ = std::io::stderr().lock().write_all(bytes);
}

/// Run `f` to append bytes for `stream` (2 = stderr, anything else = stdout).
#[inline]
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
        os_write_stderr(&local);
        return;
    }
    stdout::append(f);
}

/// Write raw bytes to `stream` with the buffering policy above. Large payloads go through the
/// thread buffer too (one extra copy), so a line with a huge string in it is still never split.
pub fn write_bytes(stream: u32, bytes: &[u8]) {
    emit(stream, |b| b.extend_from_slice(bytes));
}

/// `process.stdout.write(bytes)`: raw bytes to stdout, in order with `console.log`. A buffer of
/// at least [`stdout::CAP`] bytes is written straight from the caller's array (no copy).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_stdout_write_bytes(b: *const VeltBytes) {
    let bytes = (*b).as_bytes();
    #[cfg(test)]
    if capture::active() {
        write_bytes(1, bytes);
        return;
    }
    if bytes.len() >= stdout::CAP {
        stdout::write_through(bytes);
    } else {
        write_bytes(1, bytes);
    }
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_write_str(stream: u32, s: *const VeltStr) {
    write_bytes(stream, (*s).as_bytes());
}

#[no_mangle]
pub extern "C" fn velt_rt_write_i64(stream: u32, v: i64) {
    emit(stream, |b| fmt::push_i64(b, v));
}

#[no_mangle]
pub extern "C" fn velt_rt_write_u64(stream: u32, v: u64) {
    emit(stream, |b| fmt::push_u64(b, v));
}

#[no_mangle]
pub extern "C" fn velt_rt_write_f64(stream: u32, v: f64) {
    emit(stream, |b| fmt::push_f64(b, v));
}

#[no_mangle]
pub extern "C" fn velt_rt_write_bool(stream: u32, v: u8) {
    emit(stream, |b| fmt::push_bool(b, v));
}

#[no_mangle]
pub extern "C" fn velt_rt_write_byte(stream: u32, b: u8) {
    emit(stream, |buf| buf.push(b));
}

#[no_mangle]
pub extern "C" fn velt_rt_flush() {
    flush_stdout();
}

/// Test-only output capture: while active on the current thread, both streams are appended to a
/// thread-local buffer instead of reaching the OS (tests run in parallel threads).
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

    /// Run `f` with capture enabled and return everything it wrote.
    pub fn run(f: impl FnOnce()) -> String {
        CAP.with(|c| *c.borrow_mut() = Some(Vec::new()));
        f();
        let out = CAP.with(|c| c.borrow_mut().take()).unwrap();
        String::from_utf8(out).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::str::VeltStr;

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
            velt_rt_write_f64(1, 1e21);
            velt_rt_write_byte(1, b' ');
            velt_rt_write_bool(1, 1);
            velt_rt_write_byte(1, b'\n');
            velt_rt_write_f64(2, f64::NAN);
            velt_rt_flush();
        });
        assert_eq!(out, "x = -42 7 0.30000000000000004 1e+21 true\n[err]NaN");
    }

    #[test]
    fn large_write_captured() {
        let big = vec![b'a'; stdout::CAP * 2];
        let s = VeltStr::from_static(Box::leak(big.into_boxed_slice()));
        let out = capture::run(|| unsafe { velt_rt_write_str(1, &s) });
        assert_eq!(out.len(), stdout::CAP * 2);
    }
}
