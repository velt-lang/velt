//! stdout buffering that scales to many worker threads while keeping output causally ordered.
//!
//! Each thread appends to its own buffer (no lock per `write_*` call, and a `console.log` line built
//! from several calls is never interleaved with another thread's). A thread's buffer is *published*
//! into one shared buffer (one mutex acquisition) when it fills, when stdout is flushed, at the end
//! of every task poll (`publish_thread_output`) — before the task can resume on another worker, so
//! `log a; await; log b` always prints `a` before `b` — and before the task hands work to another
//! (`publish_before_handoff`, called before the hand-off becomes visible: spawning a task, a channel
//! send or close, a receive that frees room in a bounded channel, settling a `new Promise`, aborting
//! a signal, a child leaving a task group, releasing a keep-alive reference or a server's
//! handler, sending on a socket: a TCP connect, write or close, a UDP datagram, a `fetch`
//! request, a server's response or body chunk), so a line logged
//! before `spawn(f())` prints before anything `f` prints, and a timer callback's last line before
//! the program exits. Hand-offs through shared state (`shared`, a `Mutex`) and timers are not
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
        if buf.is_empty() {
            return;
        }
        if EXITING.load(Ordering::Relaxed) {
            // The process is ending (a panic or fatal error flushed already): the lock's holder
            // may be gone, so this must not wait for it forever.
            match lock_shared_for_exit() {
                Some(mut shared) => {
                    shared.extend_from_slice(buf);
                    write_shared(&mut shared);
                }
                None => os_write(buf),
            }
            buf.clear();
            return;
        }
        publish(buf);
    }
}

thread_local! {
    static LOCAL: Local = const { Local(RefCell::new(Vec::new())) };
    /// Whether this thread's buffer may hold bytes: a cheap check (no destructor) for the
    /// hand-off points, which mostly find the buffer empty.
    static BUFFERED: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread holds the shared buffer's lock (a [`SharedGuard`] is alive): a flush
    /// on the way out from under it must not wait for itself.
    static HOLDS_SHARED: Cell<bool> = const { Cell::new(false) };
}

/// Set by a flush on the way out ([`try_flush`]): the process ends now.
static EXITING: AtomicBool = AtomicBool::new(false);

/// The shared buffer's lock, marking this thread as its holder while it lives.
struct SharedGuard(MutexGuard<'static, Vec<u8>>);

impl SharedGuard {
    fn new(g: MutexGuard<'static, Vec<u8>>) -> SharedGuard {
        let _ = HOLDS_SHARED.try_with(|h| h.set(true));
        SharedGuard(g)
    }
}

impl Drop for SharedGuard {
    fn drop(&mut self) {
        let _ = HOLDS_SHARED.try_with(|h| h.set(false));
    }
}

impl std::ops::Deref for SharedGuard {
    type Target = Vec<u8>;
    fn deref(&self) -> &Vec<u8> {
        &self.0
    }
}

impl std::ops::DerefMut for SharedGuard {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
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

fn lock_shared() -> SharedGuard {
    // A poisoned lock only means some thread panicked mid-write; the bytes are still valid.
    SharedGuard::new(SHARED.lock().unwrap_or_else(|e| e.into_inner()))
}

fn os_write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    super::stats::write();
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

/// Decide once, at start-up, whether stdout is a terminal (line buffering). Asking the OS can
/// take tens of milliseconds on Windows when stdout is a pipe or a file, and a task asking at
/// its first `console.log` held up its thread that long, so output another task printed
/// meanwhile came first.
pub fn init_mode() {
    line_buffered();
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

/// Idle-worker hook: publish what this thread still holds (output from code that ran outside a
/// task poll, such as a future dropped after it finished, has no end-of-poll publish), then write
/// the shared buffer if it holds anything.
pub fn flush_idle() {
    publish_if_buffered();
    if DIRTY.load(Ordering::Relaxed) {
        write_shared(&mut lock_shared());
    }
}

/// How long a panic or fatal error waits for another thread to finish writing out the shared
/// buffer before it writes this thread's output without it.
const EXIT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// The shared buffer for a flush on the way out, waiting at most [`EXIT_LOCK_WAIT`] for it: the
/// thread that holds the lock is usually writing it to the OS and lets go soon, but it may also
/// be the panicking thread itself (a panic under the lock): then it gives up at once.
fn lock_shared_for_exit() -> Option<SharedGuard> {
    if HOLDS_SHARED.try_with(Cell::get).unwrap_or(false) {
        return None;
    }
    let start = std::time::Instant::now();
    loop {
        match SHARED.try_lock() {
            Ok(g) => return Some(SharedGuard::new(g)),
            Err(TryLockError::Poisoned(e)) => return Some(SharedGuard::new(e.into_inner())),
            Err(TryLockError::WouldBlock) if start.elapsed() < EXIT_LOCK_WAIT => {
                #[cfg(test)]
                EXIT_WAITED.store(true, Ordering::SeqCst);
                std::thread::yield_now();
            }
            Err(TryLockError::WouldBlock) => return None,
        }
    }
}

/// Set once a flush on the way out waited for the shared buffer's lock (for tests).
#[cfg(test)]
pub(crate) static EXIT_WAITED: AtomicBool = AtomicBool::new(false);

/// The shared buffer's lock (for tests that hold it while another thread flushes).
#[cfg(test)]
pub(crate) fn lock_for_test() -> impl std::ops::DerefMut<Target = Vec<u8>> {
    lock_shared()
}

/// Flush for panic and fatal-error paths, which end the process right after: everything
/// published so far, then this thread's buffer. Never waits forever (the panicking thread may
/// hold a lock): once [`lock_shared_for_exit`] gives up, this thread's output is written alone.
/// A buffer that is being appended to (a panic while formatting a write) is left to the thread
/// buffer's destructor at exit, which then does not wait forever either ([`EXITING`]).
pub fn try_flush() {
    EXITING.store(true, Ordering::Relaxed);
    let mut shared = lock_shared_for_exit();
    if let Some(shared) = shared.as_mut() {
        write_shared(shared);
    }
    // With the shared lock, written while it is still held, so no other thread's output slips
    // in. Without it, written alone, after whatever write another thread has under way (std's
    // stdout lock), ahead of what that thread has not written yet.
    let _ = LOCAL.try_with(|l| {
        if let Ok(mut buf) = l.0.try_borrow_mut() {
            os_write(&buf);
            buf.clear();
        }
    });
    drop(shared);
}
