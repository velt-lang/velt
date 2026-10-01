//! `VELT_RT_DEBUG_ALLOC=1` (debug runtime only): an address-checking global allocator for
//! finding memory bugs in generated code and in the runtime without platform sanitizers.
//!
//! Every block gets a header (`LIVE` magic + size) and canary bytes before and after the user
//! area, which starts out filled with `ALLOC_FILL`. A free checks the header (double free,
//! freeing a pointer that was never allocated, wrong size) and the canaries (buffer overflow /
//! underflow), fills the block with `FREE_FILL` and parks it in a quarantine instead of
//! releasing it, so a use after free reads the poison pattern (and does not corrupt a reused
//! block); when a block leaves the quarantine its poison is checked (write after free). The
//! first violation prints `velt debug-alloc: <what>` to stderr and aborts.
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
/// Fill bytes: fresh allocation, freed block, canaries.
const ALLOC_FILL: u8 = 0xCD;
const FREE_FILL: u8 = 0xDD;
const CANARY: u8 = 0xFD;
/// Header size (magic + size) and trailing canary size.
const HEADER: usize = 16;
const TAIL: usize = 16;
/// Freed blocks kept poisoned before they are really released.
const QUARANTINE_BLOCKS: usize = 1 << 14;
const QUARANTINE_BYTES: usize = 64 << 20;

/// 0 = not decided yet, 1 = off, 2 = on.
static MODE: AtomicU8 = AtomicU8::new(0);

/// The global allocator wrapper (see the module docs).
pub struct DebugAlloc<A>(pub A);

struct Quarantine {
    blocks: [(usize, usize, usize); QUARANTINE_BLOCKS],
    head: usize,
    len: usize,
    bytes: usize,
}

static QUARANTINE: Mutex<Quarantine> = Mutex::new(Quarantine {
    blocks: [(0, 0, 0); QUARANTINE_BLOCKS],
    head: 0,
    len: 0,
    bytes: 0,
});

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
        let base = self.0.alloc(outer(layout));
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
        self.quarantine(p, size, layout.align());
    }

    /// Park a freed block; release the oldest ones once the quarantine is full, after checking
    /// that nothing wrote to them since they were freed.
    unsafe fn quarantine(&self, p: usize, size: usize, align: usize) {
        let mut q = QUARANTINE.lock().unwrap_or_else(|e| e.into_inner());
        while q.len == QUARANTINE_BLOCKS || (q.len > 0 && q.bytes + size > QUARANTINE_BYTES) {
            let (op, osize, oalign) = q.blocks[q.head];
            q.head = (q.head + 1) % QUARANTINE_BLOCKS;
            q.len -= 1;
            q.bytes -= osize;
            if !all_are(op as *const u8, FREE_FILL, osize) {
                fail("write after free: a freed block was modified", op, osize);
            }
            let l = Layout::from_size_align_unchecked(osize, oalign);
            self.0.dealloc((op - front(l)) as *mut u8, outer(l));
        }
        let at = (q.head + q.len) % QUARANTINE_BLOCKS;
        q.blocks[at] = (p, size, align);
        q.len += 1;
        q.bytes += size;
    }
}

// SAFETY: every block handed out is a valid, suitably aligned block of the inner allocator
// (offset by a multiple of the alignment); frees are routed back to the inner allocator with
// the layout the block was allocated with.
unsafe impl<A: GlobalAlloc> GlobalAlloc for DebugAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if enabled() {
            self.checked_alloc(layout)
        } else {
            self.0.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if enabled() {
            self.checked_free(ptr, layout)
        } else {
            self.0.dealloc(ptr, layout)
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
            self.0.alloc_zeroed(layout)
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !enabled() {
            return self.0.realloc(ptr, layout, new_size);
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

    #[test]
    fn blocks_are_padded_poisoned_and_quarantined() {
        let a = DebugAlloc(System);
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

    #[test]
    fn mode_comes_from_the_environment() {
        std::env::set_var("VELT_RT_DEBUG_ALLOC", "1");
        assert!(env_flag_set());
        std::env::set_var("VELT_RT_DEBUG_ALLOC", "0");
        assert!(!env_flag_set());
        std::env::remove_var("VELT_RT_DEBUG_ALLOC");
        assert!(!env_flag_set());
    }
}
