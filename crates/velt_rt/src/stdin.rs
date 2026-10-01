//! `std/stdin`: reading the process's standard input, line by line or all at once (as text or
//! bytes).
//!
//! Reads go through one process-wide read-ahead buffer ([`INPUT`]), so sync and async reads can
//! be mixed and see one consistent stream. A line already in the buffer is returned without
//! touching the OS: an async `readLine` then completes at once instead of hopping to the
//! blocking pool, and a line costs one memchr plus one copy into its string. Refills read 64 KiB
//! at a time (terminal and pipe reads block on every platform, so async refills run on the
//! blocking pool). Text is decoded as UTF-8, invalid bytes as U+FFFD.

use crate::bytes::VeltBytes;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::{blocking_leaf, new_leaf, run_blocking};
use crate::task::VeltFut;
use parking_lot::Mutex;
use std::io::Read;

/// Bytes read per refill.
const CHUNK: usize = 64 * 1024;

/// Read-ahead state of standard input.
struct Input {
    /// Bytes read from the OS; `buf[start..]` has not been handed out yet.
    buf: Vec<u8>,
    start: usize,
    /// The OS reported end of input.
    eof: bool,
}

static INPUT: Mutex<Input> = Mutex::new(Input {
    buf: Vec::new(),
    start: 0,
    eof: false,
});

impl Input {
    /// The next buffered line without its `\n` / `\r\n`: `Some(Some(line))`, `Some(None)` at
    /// end of input, or `None` when the buffer holds no complete line yet.
    fn take_line(&mut self) -> Option<Option<VeltStr>> {
        let rest = &self.buf[self.start..];
        let (line, used) = match memchr::memchr(b'\n', rest) {
            Some(i) => (rest[..i].strip_suffix(b"\r").unwrap_or(&rest[..i]), i + 1),
            None if self.eof && !rest.is_empty() => (rest, rest.len()),
            None if self.eof => return Some(None),
            None => return None,
        };
        let s = decode(line);
        self.start += used;
        Some(Some(s))
    }

    /// Reads the next chunk from the OS (compacting consumed bytes first).
    fn fill(&mut self) -> std::io::Result<()> {
        self.buf.drain(..self.start);
        self.start = 0;
        let len = self.buf.len();
        self.buf.resize(len + CHUNK, 0);
        let read = loop {
            match std::io::stdin().lock().read(&mut self.buf[len..]) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                r => break r,
            }
        };
        let n = *read.as_ref().unwrap_or(&0);
        self.buf.truncate(len + n);
        read?;
        self.eof = n == 0;
        Ok(())
    }
}

/// A string of `bytes`, invalid UTF-8 replaced by U+FFFD (one copy when the text is valid).
pub(crate) fn decode(bytes: &[u8]) -> VeltStr {
    match std::str::from_utf8(bytes) {
        Ok(_) => VeltStr::from_bytes(bytes),
        Err(_) => VeltStr::from_vec(String::from_utf8_lossy(bytes).into_owned().into_bytes()),
    }
}

/// `Ok(Some(line))` without its `\n` / `\r\n`; `Ok(None)` at end of input.
fn read_line() -> std::io::Result<Option<VeltStr>> {
    let mut input = INPUT.lock();
    loop {
        if let Some(line) = input.take_line() {
            return Ok(line);
        }
        input.fill()?;
    }
}

impl Input {
    /// Everything not handed out yet, through end of input. The buffer is handed over as is
    /// (the rest is read straight into it, with `read_to_end`'s growing reads); on an error it
    /// stays buffered.
    fn take_rest(&mut self) -> std::io::Result<Vec<u8>> {
        let mut rest = std::mem::take(&mut self.buf);
        rest.drain(..self.start);
        self.start = 0;
        if !self.eof {
            if let Err(e) = std::io::stdin().lock().read_to_end(&mut rest) {
                self.buf = rest;
                return Err(e);
            }
            self.eof = true;
        }
        Ok(rest)
    }
}

fn read_all() -> std::io::Result<VeltStr> {
    INPUT.lock().take_rest().map(|rest| decode(&rest))
}

fn read_all_bytes() -> std::io::Result<VeltBytes> {
    INPUT.lock().take_rest().map(VeltBytes::from_vec)
}

/// One `readLine` result: `{ IoResult<VeltStr> line; u8 eof; }` (size 64). At end of input
/// `eof` is 1 and the line is empty (an empty line before the end has `eof` 0).
#[repr(C)]
pub struct LineRead {
    /// The line without its `\n` / `\r\n`.
    pub line: IoResult<VeltStr>,
    /// 1 at end of input.
    pub eof: u8,
}

fn line_read(r: std::io::Result<Option<VeltStr>>) -> LineRead {
    let eof = matches!(r, Ok(None)) as u8;
    LineRead {
        line: IoResult::from_io(r, |line| line.unwrap_or_else(VeltStr::empty)),
        eof,
    }
}

fn next_line() -> LineRead {
    line_read(read_line())
}

/// The next line (blocks the calling thread) → `LineRead`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_stdin_read_line_sync(out: *mut LineRead) {
    out.write(next_line());
}

/// Async `readLine` → result slot `LineRead`.
#[no_mangle]
pub extern "C" fn velt_rt_stdin_read_line() -> *mut VeltFut {
    new_leaf(async {
        // A buffered line needs no blocking read. `try_lock`: if another reader holds the
        // buffer, wait for it on the blocking pool rather than on this worker.
        let buffered = INPUT.try_lock().and_then(|mut input| input.take_line());
        match buffered {
            Some(line) => line_read(Ok(line)),
            None => run_blocking(next_line).await,
        }
    })
}

/// All remaining input (sync) → `IoResult<VeltStr>` (no return value: std reads the code from `out`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_stdin_read_all_sync(out: *mut IoResult<VeltStr>) {
    out.write(IoResult::from_io(read_all(), |s| s));
}

/// All remaining input (async) → `IoResult<VeltStr>`.
#[no_mangle]
pub extern "C" fn velt_rt_stdin_read_all() -> *mut VeltFut {
    blocking_leaf(|| IoResult::from_io(read_all(), |s| s))
}

/// All remaining input as bytes (sync) → `IoResult<VeltBytes>` (the runtime's buffer, not a copy).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_stdin_read_all_bytes_sync(out: *mut IoResult<VeltBytes>) {
    out.write(IoResult::from_io(read_all_bytes(), |b| b));
}

/// All remaining input as bytes (async) → `IoResult<VeltBytes>`.
#[no_mangle]
pub extern "C" fn velt_rt_stdin_read_all_bytes() -> *mut VeltFut {
    blocking_leaf(|| IoResult::from_io(read_all_bytes(), |b| b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(data: &[u8]) -> Vec<Option<String>> {
        let mut input = Input {
            buf: data.to_vec(),
            start: 0,
            eof: true,
        };
        let mut out = Vec::new();
        while let Some(Some(line)) = input.take_line() {
            // SAFETY: a live string built by `take_line` (leaked, as test strings are).
            let bytes = unsafe { line.as_bytes() };
            out.push(Some(String::from_utf8_lossy(bytes).into_owned()));
        }
        out.push(input.take_line().flatten().map(|_| String::new()));
        out
    }

    #[test]
    fn splits_lines_and_strips_crlf() {
        let s = |t: &str| Some(t.to_string());
        assert_eq!(lines(b"a\r\n\nb"), [s("a"), s(""), s("b"), None]);
        assert_eq!(lines(b"x\n"), [s("x"), None]);
        assert_eq!(lines(b""), [None]);
        assert_eq!(lines(b"c\xffd\n"), [s("c\u{fffd}d"), None]);
    }

    #[test]
    fn incomplete_line_waits_for_more_input() {
        let mut input = Input {
            buf: b"partial".to_vec(),
            start: 0,
            eof: false,
        };
        assert!(input.take_line().is_none());
        assert_eq!(input.start, 0);
    }
}
