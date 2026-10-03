//! stdout buffering that scales to many worker threads while keeping output causally ordered.
//!
//! Each thread appends to its own buffer (no lock per `write_*` call, and a `console.log` line built
//! from several calls is never interleaved with another thread's). A thread's buffer is *published*
//! into one shared buffer (one mutex acquisition) when it fills, when stdout is flushed, at the end
//! of every task poll (`publish_thread_output`) — before the task can resume on another worker, so
//! `log a; await; log b` always prints `a` before `b` — and before the task hands work to another
//! (`publish_before_handoff`, called before the hand-off becomes visible: spawning a task, a channel
//! send or close, a receive that frees room in a bounded channel, settling a `new Promise`, aborting
//! a signal, a child leaving a task group), so a line logged before `spawn(f())` prints before
//! anything `f` prints. Hand-offs through shared state (`shared`, a `Mutex`) and timers are not
//! covered: such lines may still appear out of order. The shared buffer reaches the OS
//! when it fills, on explicit flushes, and when a worker goes idle. On an interactive terminal every
//! completed line is written through immediately (line buffering, like C stdio).

use std::cell::{Cell, RefCell};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

/// Buffer size at which data moves on (thread buffer → shared buffer → OS).
pub const CAP: usize = 64 * 1024;
/// A thread buffer holding a partial line is published anyway once it reaches this size: memory
/// stays bounded, at the price that such an enormous line may interleave with other threads'.
const MAX_PARTIAL_LINE: usize = 16 * CAP;

static SHARED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Set while the shared buffer holds unwritten bytes (cheap check for idle flushes).
static DIRTY: AtomicBool = AtomicBool::new(false);
/// 0 = unknown, 1 = block-buffered (pipe/file), 2 = line-buffered (terminal).
static MODE: AtomicU8 = AtomicU8::new(0);

/// Thread buffer; publishes leftovers when the thread exits.
struct Local(RefCell<Vec<u8>>);

impl Drop for Local {
    fn drop(&mut self) {
        let buf = self.0.get_mut();
        if !buf.is_empty() {
            publish(buf);
        }
    }
}

thread_local! {
    static LOCAL: Local = const { Local(RefCell::new(Vec::new())) };
    /// Whether this thread's buffer may hold bytes: a cheap check (no destructor) for the
    /// hand-off points, which mostly find the buffer empty.
    static BUFFERED: Cell<bool> = const { Cell::new(false) };
}

fn line_buffered() -> bool {
    match MODE.load(Ordering::Relaxed) {
        0 => {
            let tty = std::io::stdout().is_terminal();
            MODE.store(if tty { 2 } else { 1 }, Ordering::Relaxed);
            tty
        }
        m => m == 2,
    }
}

fn lock_shared() -> MutexGuard<'static, Vec<u8>> {
    // A poisoned lock only means some thread panicked mid-write; the bytes are still valid.
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

fn os_write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Errors (full disk) are ignored, like console.log in JS, except a closed pipe.
    let mut out = std::io::stdout().lock();
    let r = out.write_all(bytes).and_then(|()| out.flush());
    #[cfg(unix)]
    if matches!(&r, Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe) {
        crate::entry::die_of_broken_pipe();
    }
    #[cfg(not(unix))]
    let _ = r;
}

fn write_shared(shared: &mut Vec<u8>) {
    os_write(shared);
    shared.clear();
    DIRTY.store(false, Ordering::Relaxed);
}

/// Copy a thread buffer's bytes into the shared buffer (or straight to the OS when large).
fn publish(buf: &[u8]) {
    let mut shared = lock_shared();
    if shared.is_empty() && buf.len() >= CAP {
        os_write(buf);
    } else {
        shared.extend_from_slice(buf);
        if shared.len() >= CAP {
            write_shared(&mut shared);
        } else {
            DIRTY.store(true, Ordering::Relaxed);
        }
    }
}

/// Publish and empty a thread buffer; gives back memory a huge single write made it grow to.
fn publish_all(buf: &mut Vec<u8>) {
    publish(buf);
    buf.clear();
    if buf.capacity() > 4 * CAP {
        buf.shrink_to(CAP + 64);
    }
}

/// The thread buffer filled up: publish it through its last newline but keep a trailing partial
/// line, which is the start of a `console.log` built from several writes (`a`, `" "`, `b`, `\n`)
/// and must not be split by another thread's output. `start` = the length before the last append.
fn publish_complete_lines(buf: &mut Vec<u8>, start: usize) {
    // A buffer that was already >= CAP before this append held no newline (it would have been
    // published), so only the new bytes need scanning then.
    let from = if start < CAP { 0 } else { start };
    match buf[from..].iter().rposition(|&b| b == b'\n') {
        Some(i) if from + i + 1 == buf.len() => publish_all(buf),
        Some(i) => {
            let end = from + i + 1;
            publish(&buf[..end]);
            buf.drain(..end);
        }
        None if buf.len() >= MAX_PARTIAL_LINE => publish_all(buf),
        None => {}
    }
}

/// Append to this thread's buffer via `f`, applying the size and line-buffering policies.
pub fn append(f: impl FnOnce(&mut Vec<u8>)) {
    let mut f = Some(f);
    let _ = LOCAL.try_with(|l| {
        let Ok(mut buf) = l.0.try_borrow_mut() else {
            return;
        };
        let Some(f) = f.take() else { return };
        if buf.capacity() == 0 {
            buf.reserve(CAP + 64);
        }
        let start = buf.len();
        f(&mut buf);
        if buf.len() >= CAP {
            publish_complete_lines(&mut buf, start);
        } else if line_buffered() && buf[start..].contains(&b'\n') {
            publish_all(&mut buf);
            write_shared(&mut lock_shared());
        }
        if start == 0 {
            // Only when the buffer starts filling: an over-approximation is harmless.
            BUFFERED.set(true);
        }
    });
    if let Some(f) = f {
        // Thread buffer unavailable (thread shutting down): go through the shared buffer.
        let mut tmp = Vec::new();
        f(&mut tmp);
        publish(&tmp);
    }
}

/// Publish this thread's buffered output into the shared buffer (end of a task poll).
pub fn publish_local() {
    let _ = LOCAL.try_with(|l| {
        if let Ok(mut buf) = l.0.try_borrow_mut() {
            if !buf.is_empty() {
                publish_all(&mut buf);
            }
            BUFFERED.set(false);
        }
    });
}

/// Bytes in this thread's buffer (for tests).
#[cfg(test)]
pub(crate) fn local_len() -> usize {
    LOCAL.with(|l| l.0.borrow().len())
}

/// [`publish_local`], with a cheap check first for the common case of an empty buffer.
pub fn publish_if_buffered() {
    if BUFFERED.get() {
        publish_local();
    }
}

/// Write everything buffered by this thread and the shared buffer to the OS.
pub fn flush() {
    publish_local();
    write_shared(&mut lock_shared());
}

/// Write `bytes` straight to the OS after everything buffered so far (this thread's and the
/// shared buffer), without copying them into a buffer: for large binary writes.
pub fn write_through(bytes: &[u8]) {
    publish_local();
    let mut shared = lock_shared();
    write_shared(&mut shared);
    // Written under the shared lock, so another thread's output can't slip in between.
    os_write(bytes);
}

/// Idle-worker hook: write the shared buffer if it holds anything.
pub fn flush_idle() {
    if DIRTY.load(Ordering::Relaxed) {
        write_shared(&mut lock_shared());
    }
}

/// Best-effort flush for panic/abort paths: never blocks (the panicking thread may hold a lock).
pub fn try_flush() {
    let _ = LOCAL.try_with(|l| {
        if let Ok(mut buf) = l.0.try_borrow_mut() {
            let shared = match SHARED.try_lock() {
                Ok(g) => Some(g),
                Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut shared) = shared {
                write_shared(&mut shared);
                os_write(&buf);
                buf.clear();
            }
        }
    });
}
