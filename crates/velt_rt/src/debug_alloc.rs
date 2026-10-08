//! `VELT_RT_DEBUG_ALLOC=1` (debug runtime only): an address-checking global allocator for
//! finding memory bugs in generated code and in the runtime without platform sanitizers.
//!
//! Every block gets a header (`LIVE` magic + size) and canary bytes before and after the user
//! area, which starts out filled with `ALLOC_FILL`. A free checks the header (double free,
//! freeing a pointer that was never allocated, wrong size) and the canaries (buffer overflow /
//! underflow), fills the block with `FREE_FILL` and parks it in a quarantine instead of
//! releasing it, so a use after free reads the poison pattern (and does not corrupt a reused
//! block); when a block leaves the quarantine, and for the blocks still in it when the program
//! ends ([`check_quarantine`]), its poison is checked (write after free). The first violation
//! prints `velt debug-alloc: <what>` to stderr and aborts.
//!
//! The mode is read once, on the first allocation, without allocating (the environment is read
//! through the OS directly). With the variable unset every call goes straight to the inner
//! allocator after one relaxed load. The release runtime and the `velt dev` host never use
//! this wrapper.

use std::alloc::{GlobalAlloc, Layout};
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// Header magic of a live block, and of a block already freed (in quarantine).
const LIVE: u64 = 0x5641_4953_4c49_5645;
const FREED: u64 = 0x5641_4953_4652_4545;
/// Fill bytes: fresh allocation, freed block, canaries. A freed block reads as a tiny number
/// (`-1.5e-130` as an `f64`), so arithmetic written back into it changes its bytes and is
/// caught (`0xDD` read as a huge `f64`, and `x + 7` wrote the same bits back, #580); as a pointer
/// it is non-canonical, so following one faults.
const ALLOC_FILL: u8 = 0xCD;
const FREE_FILL: u8 = 0xA5;
const CANARY: u8 = 0xFD;
/// Header size (magic + size) and trailing canary size.
const HEADER: usize = 16;
const TAIL: usize = 16;
/// Freed blocks kept poisoned before they are really released.
const QUARANTINE_BLOCKS: usize = 1 << 14;
const QUARANTINE_BYTES: usize = 64 << 20;

/// 0 = not decided yet, 1 = off, 2 = on.
static MODE: AtomicU8 = AtomicU8::new(0);

/// The global allocator wrapper (see the module docs) over the inner allocator `A`, parking freed
/// blocks in its own quarantine: a block leaves it through the allocator that made it (#555).
pub struct DebugAlloc<A> {
    inner: A,
    quarantine: &'static Quarantine,
}

impl<A> DebugAlloc<A> {
    /// The wrapper over `inner`. `quarantine` must not be shared with a wrapper over another
    /// allocator: the blocks it releases go back to `inner`.
    pub const fn new(inner: A, quarantine: &'static Quarantine) -> DebugAlloc<A> {
        DebugAlloc { inner, quarantine }
    }
}

/// Freed blocks of one [`DebugAlloc`], kept poisoned before they are really released.
pub struct Quarantine(Mutex<Blocks>);

struct Blocks {
    blocks: [(usize, usize, usize); QUARANTINE_BLOCKS],
    head: usize,
    len: usize,
    bytes: usize,
}

impl Quarantine {
    pub const fn new() -> Quarantine {
        Quarantine(Mutex::new(Blocks {
            blocks: [(0, 0, 0); QUARANTINE_BLOCKS],
            head: 0,
            len: 0,
            bytes: 0,
        }))
    }
}

impl Default for Quarantine {
    fn default() -> Quarantine {
        Quarantine::new()
    }
}

fn enabled() -> bool {
    match MODE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = env_flag_set();
            MODE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Is `VELT_RT_DEBUG_ALLOC` set to `1`? Read without allocating (this runs inside `alloc`).
#[cfg(windows)]
fn env_flag_set() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetEnvironmentVariableA(name: *const u8, buf: *mut u8, size: u32) -> u32;
    }
    let mut buf = [0u8; 4];
    // SAFETY: NUL-terminated name, buffer of the given size.
    let n = unsafe {
        GetEnvironmentVariableA(c"VELT_RT_DEBUG_ALLOC".as_ptr().cast(), buf.as_mut_ptr(), 4)
    };
    n == 1 && buf[0] == b'1'
}

#[cfg(unix)]
fn env_flag_set() -> bool {
    // SAFETY: NUL-terminated name; the result is a NUL-terminated string or null.
    unsafe {
        let v = libc::getenv(c"VELT_RT_DEBUG_ALLOC".as_ptr());
        !v.is_null() && *v == b'1' as libc::c_char && *v.add(1) == 0
    }
}

#[cfg(not(any(windows, unix)))]
fn env_flag_set() -> bool {
    false
}

/// Is the process exiting with its other threads already gone? On Windows, `ExitProcess`
/// terminates every other thread wherever it is, then runs the thread-local destructors of the
/// exiting thread (tokio's park state, ...), which free memory. A worker terminated while it held
/// the quarantine's lock (likely when freeing a large block: evicting it checks every byte under
/// the lock) left that lock held forever, and the exit hung (#297). Elsewhere `exit` runs no
/// destructors after ending other threads.
#[cfg(windows)]
fn exit_in_progress() -> bool {
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlDllShutdownInProgress() -> u8;
    }
    // SAFETY: a plain query of the loader's state.
    unsafe { RtlDllShutdownInProgress() != 0 }
}

#[cfg(not(windows))]
fn exit_in_progress() -> bool {
    false
}

/// Front padding: header plus canaries, a multiple of the alignment.
fn front(layout: Layout) -> usize {
    layout.align().max(HEADER)
}

fn outer(layout: Layout) -> Layout {
    let size = front(layout) + layout.size() + TAIL;
    Layout::from_size_align(size, layout.align().max(HEADER))
        .unwrap_or_else(|_| fail("layout overflow", 0, 0))
}

/// Report a violation and abort (no allocation: the heap may be corrupt).
fn fail(what: &str, ptr: usize, size: usize) -> ! {
    let mut buf = [0u8; 160];
    let mut w = &mut buf[..];
    let _ = writeln!(w, "velt debug-alloc: {what} (block {ptr:#x}, size {size})");
    let left = w.len();
    let n = buf.len() - left;
    let _ = std::io::stderr().write_all(&buf[..n]);
    std::process::abort()
}

unsafe fn fill(p: *mut u8, byte: u8, n: usize) {
    std::ptr::write_bytes(p, byte, n);
}

unsafe fn all_are(p: *const u8, byte: u8, n: usize) -> bool {
    std::slice::from_raw_parts(p, n).iter().all(|&b| b == byte)
}

impl<A: GlobalAlloc> DebugAlloc<A> {
    unsafe fn checked_alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.inner.alloc(outer(layout));
        if base.is_null() {
            return base;
        }
        let pad = front(layout);
        let user = base.add(pad);
        fill(base, CANARY, pad - HEADER);
        let hdr = user.sub(HEADER) as *mut u64;
        hdr.write_unaligned(LIVE);
        hdr.add(1).write_unaligned(layout.size() as u64);
        fill(user, ALLOC_FILL, layout.size());
        fill(user.add(layout.size()), CANARY, TAIL);
        user
    }

    unsafe fn checked_free(&self, user: *mut u8, layout: Layout) {
        let (p, size) = (user as usize, layout.size());
        let pad = front(layout);
        let hdr = user.sub(HEADER) as *const u64;
        match hdr.read_unaligned() {
            LIVE => {}
            FREED => fail("double free", p, size),
            _ => fail("free of a block that was not allocated here, or its header was overwritten (buffer underflow)", p, size),
        }
        let recorded = hdr.add(1).read_unaligned() as usize;
        if recorded != size {
            fail(
                "freed with a different size than it was allocated with",
                p,
                recorded,
            );
        }
        if !all_are(user.sub(pad), CANARY, pad - HEADER) {
            fail(
                "buffer underflow: the bytes before the block were overwritten",
                p,
                size,
            );
        }
        if !all_are(user.add(size), CANARY, TAIL) {
            fail(
                "buffer overflow: the bytes after the block were overwritten",
                p,
                size,
            );
        }
        (user.sub(HEADER) as *mut u64).write_unaligned(FREED);
        fill(user, FREE_FILL, size);
        // At exit the block is left alone: the quarantine's lock may belong to a thread the OS
        // has already ended (see `exit_in_progress`), and the memory goes away with the process.
        if exit_in_progress() {
            return;
        }
        self.quarantine(p, size, layout.align());
    }

    /// Park a freed block; release the oldest ones once the quarantine is full, after checking
    /// that nothing wrote to them since they were freed.
    unsafe fn quarantine(&self, p: usize, size: usize, align: usize) {
        let mut q = self.quarantine.0.lock().unwrap_or_else(|e| e.into_inner());
        while q.len == QUARANTINE_BLOCKS || (q.len > 0 && q.bytes + size > QUARANTINE_BYTES) {
            let (op, osize, oalign) = q.blocks[q.head];
            q.head = (q.head + 1) % QUARANTINE_BLOCKS;
            q.len -= 1;
            q.bytes -= osize;
            if !all_are(op as *const u8, FREE_FILL, osize) {
                fail("write after free: a freed block was modified", op, osize);
            }
            let l = Layout::from_size_align_unchecked(osize, oalign);
            self.inner.dealloc((op - front(l)) as *mut u8, outer(l));
        }
        let at = (q.head + q.len) % QUARANTINE_BLOCKS;
        q.blocks[at] = (p, size, align);
        q.len += 1;
        q.bytes += size;
    }
}

/// Check every block still in `quarantine` for writes after it was freed (debug runtime with
/// `VELT_RT_DEBUG_ALLOC=1`). Called when the program ends: a block freed late enough never
/// leaves the quarantine, so without this a write after free into it went unnoticed.
pub fn check_quarantine(quarantine: &Quarantine) {
    if MODE.load(Ordering::Relaxed) != 2 {
        return;
    }
    if let Some((p, size)) = modified_in_quarantine(quarantine) {
        fail("write after free: a freed block was modified", p, size);
    }
}

/// The first block in `quarantine` whose poison was overwritten: `(address, size)`. Skipped
/// (`None`) while another thread holds the lock: waiting could hang the exit (#297), and that
/// thread may be in the middle of evicting blocks.
fn modified_in_quarantine(quarantine: &Quarantine) -> Option<(usize, usize)> {
    let q = match quarantine.0.try_lock() {
        Ok(q) => q,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    (0..q.len)
        .map(|k| q.blocks[(q.head + k) % QUARANTINE_BLOCKS])
        // SAFETY: quarantined blocks stay allocated (and poisoned) until they are evicted.
        .find(|&(p, size, _)| !unsafe { all_are(p as *const u8, FREE_FILL, size) })
        .map(|(p, size, _)| (p, size))
}

// SAFETY: every block handed out is a valid, suitably aligned block of the inner allocator
// (offset by a multiple of the alignment); frees are routed back to the inner allocator with
// the layout the block was allocated with.
unsafe impl<A: GlobalAlloc> GlobalAlloc for DebugAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if enabled() {
            self.checked_alloc(layout)
        } else {
            self.inner.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if enabled() {
            self.checked_free(ptr, layout)
        } else {
            self.inner.dealloc(ptr, layout)
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if enabled() {
            let p = self.checked_alloc(layout);
            if !p.is_null() {
                fill(p, 0, layout.size());
            }
            p
        } else {
            self.inner.alloc_zeroed(layout)
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !enabled() {
            return self.inner.realloc(ptr, layout, new_size);
        }
        let new = Layout::from_size_align_unchecked(new_size, layout.align());
        let q = self.checked_alloc(new);
        if !q.is_null() {
            std::ptr::copy_nonoverlapping(ptr, q, layout.size().min(new_size));
            self.checked_free(ptr, layout);
        }
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    /// The quarantine of the tests' allocator over `System` (the global one has its own).
    static TEST_QUARANTINE: Quarantine = Quarantine::new();

    #[test]
    fn blocks_are_padded_poisoned_and_quarantined() {
        let a = DebugAlloc::new(System, &TEST_QUARANTINE);
        unsafe {
            let l = Layout::from_size_align(24, 8).unwrap();
            let p = a.checked_alloc(l);
            assert!(all_are(p, ALLOC_FILL, 24));
            assert!(all_are(p.add(24), CANARY, TAIL));
            p.write(7);
            a.checked_free(p, l);
            // The freed block stays poisoned in quarantine.
            assert!(all_are(p, FREE_FILL, 24));
            let big = Layout::from_size_align(64, 64).unwrap();
            let b = a.checked_alloc(big);
            assert_eq!(b as usize % 64, 0);
            a.checked_free(b, big);
        }
    }

    /// A write into a block that is still in quarantine when the program ends is found then; the
    /// check is skipped while another thread holds the quarantine's lock. The test has a
    /// quarantine of its own: with `TEST_QUARANTINE`, a test running in parallel holding its lock
    /// would make the check skip.
    #[test]
    fn writes_into_quarantined_blocks_are_found() {
        static QUARANTINE: Quarantine = Quarantine::new();
        let a = DebugAlloc::new(System, &QUARANTINE);
        unsafe {
            let l = Layout::from_size_align(40, 8).unwrap();
            let p = a.checked_alloc(l);
            a.checked_free(p, l);
            p.add(3).write(1);
            let hit = modified_in_quarantine(&QUARANTINE);
            let held = QUARANTINE.0.lock().unwrap();
            let skipped = modified_in_quarantine(&QUARANTINE);
            drop(held);
            // Put the poison back before anything else looks at the quarantine.
            p.add(3).write(FREE_FILL);
            assert_eq!(hit, Some((p as usize, 40)));
            assert_eq!(skipped, None);
        }
    }

    /// Set in the child process of [`a_second_allocator_keeps_its_own_quarantine`].
    const TWO_CHILD: &str = "VELT_RT_DEBUG_ALLOC_TWO_CHILD";

    /// #555: with `VELT_RT_DEBUG_ALLOC=1` the global allocator quarantines its frees too. A
    /// second `DebugAlloc` over a different inner allocator (the tests' `System`) must never
    /// release a block of the global one (mimalloc) or the reverse, however many blocks cycle
    /// through both quarantines.
    #[test]
    fn a_second_allocator_keeps_its_own_quarantine() {
        if std::env::var_os(TWO_CHILD).is_some() {
            assert!(enabled(), "the child runs with VELT_RT_DEBUG_ALLOC=1");
            blocks_are_padded_poisoned_and_quarantined();
            let a = DebugAlloc::new(System, &TEST_QUARANTINE);
            let l = Layout::from_size_align(24, 8).unwrap();
            for _ in 0..3 * QUARANTINE_BLOCKS {
                unsafe {
                    let p = a.checked_alloc(l);
                    a.checked_free(p, l);
                }
                drop(std::hint::black_box(Box::new([1u8; 24])));
            }
            return;
        }
        let exe = std::env::current_exe().expect("test executable");
        let out = crate::abi_tests::command::command(exe)
            .args([
                "--exact",
                "debug_alloc::tests::a_second_allocator_keeps_its_own_quarantine",
            ])
            .env(TWO_CHILD, "1")
            .env("VELT_RT_DEBUG_ALLOC", "1")
            .output()
            .expect("run the child");
        assert!(
            out.status.success(),
            "child: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn mode_comes_from_the_environment() {
        std::env::set_var("VELT_RT_DEBUG_ALLOC", "1");
        assert!(env_flag_set());
        std::env::set_var("VELT_RT_DEBUG_ALLOC", "0");
        assert!(!env_flag_set());
        std::env::remove_var("VELT_RT_DEBUG_ALLOC");
        assert!(!env_flag_set());
    }

    /// Set in the child process of [`exit_with_the_quarantine_locked_by_another_thread`].
    #[cfg(windows)]
    const EXIT_CHILD: &str = "VELT_RT_DEBUG_ALLOC_EXIT_CHILD";

    /// #297: a thread that held the quarantine's lock when the process exited (on Windows the
    /// OS ends it there) must not keep the exit's thread-local destructors waiting forever.
    #[cfg(windows)]
    #[test]
    fn exit_with_the_quarantine_locked_by_another_thread() {
        if std::env::var_os(EXIT_CHILD).is_some() {
            exit_while_another_thread_holds_the_lock();
        }
        let exe = std::env::current_exe().expect("test executable");
        let mut child = crate::abi_tests::command::command(exe)
            .args([
                "--exact",
                "debug_alloc::tests::exit_with_the_quarantine_locked_by_another_thread",
            ])
            .env(EXIT_CHILD, "1")
            .env("VELT_RT_DEBUG_ALLOC", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the child");
        // A hang guard, not a time limit: the child exits at once unless the exit deadlocks.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(status) = child.try_wait().expect("child status") {
                assert!(status.success(), "child: {status}");
                return;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("the process hung at exit");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Leaves a block for this thread's thread-local destructors to free, has another thread take
    /// the quarantine's lock and keep it, and exits the way a program's `main` returning does
    /// (`ExitProcess`; `std::process::exit` would first free stdout's buffer itself, while the
    /// other thread still runs).
    #[cfg(windows)]
    fn exit_while_another_thread_holds_the_lock() -> ! {
        #[link(name = "kernel32")]
        extern "system" {
            fn ExitProcess(code: u32) -> !;
        }
        thread_local! {
            static FREED_AT_EXIT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        assert!(enabled(), "the child runs with VELT_RT_DEBUG_ALLOC=1");
        FREED_AT_EXIT.with(|v| v.borrow_mut().extend_from_slice(&[1; 64]));
        let (locked, is_locked) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _held = crate::GLOBAL.quarantine.0.lock();
            locked.send(()).expect("signal");
            loop {
                std::thread::park();
            }
        });
        is_locked.recv().expect("the lock is taken");
        // SAFETY: ends the process; nothing runs after it but the OS's exit sequence.
        unsafe { ExitProcess(0) }
    }
}
