//! The poison of freed blocks, and how a read of it is caught (#872).
//!
//! When the checking allocator turns on it reserves a range of address space that is never
//! accessible (`GUARD_SPAN` bytes on each side of its middle) and fills freed blocks with one
//! 8-byte word, [`poison`]: an address a little past that middle. So whatever generated code
//! reads from a freed block is caught where it is used:
//!
//! - as a **pointer** (a field holding an object, a string's or an array's buffer), following it
//!   faults inside the reservation, before or after the poison address alike, and the fault
//!   handler installed here reports `use after free` instead of the plain crash a non-canonical
//!   address gave (#819);
//! - as a **number**, it is a tiny subnormal `f64` (or a large `i64`) that the runtime's number
//!   formatting recognises ([`crate::freed::check_value`]): printing it, or making a string of
//!   it, aborts;
//! - as a **string's count** (the string value is live but its buffer was freed), the runtime's
//!   retain and release recognise it.
//!
//! Arithmetic written back into a freed block (a counted object's count, `x += 1`) changes the
//! word, which the quarantine finds when the block leaves it or when the program ends. Without
//! the reservation (a failed reservation, or a `DebugAlloc` that is not the global allocator in
//! tests) the poison is the byte `0xA5` repeated, as before. The release runtime contains none
//! of this: the checks compile out.

use std::sync::atomic::{AtomicU64, Ordering};

/// The poison when no reservation was made: `0xA5` bytes, a tiny `f64` and a non-canonical
/// pointer.
pub(super) const BYTE_POISON: u64 = 0xA5A5_A5A5_A5A5_A5A5;

/// Inaccessible bytes reserved on each side of the poison address: a pointer read from a freed
/// block faults with a field offset or an index up to this far from it in either direction (a
/// string's count is read before its bytes).
const GUARD_SPAN: usize = 256 << 20;
/// The poison address's offset past the middle of the reservation: low bytes that are not
/// zero, so a `bool` or 32-bit field read from a freed block does not read as `false` or `0`,
/// and still 8-aligned.
const POISON_OFFSET: usize = 0x00A5_A5A0;

/// The poison word (0 until the checking allocator reserved its guard range; also published
/// to [`crate::freed`], which recognises it in values).
static POISON: AtomicU64 = AtomicU64::new(0);
/// The reservation: `[GUARD_START, GUARD_START + 2 * GUARD_SPAN)`.
static GUARD_START: AtomicU64 = AtomicU64::new(0);

/// The word freed blocks are filled with.
#[inline]
pub(super) fn poison() -> u64 {
    match POISON.load(Ordering::Relaxed) {
        0 => BYTE_POISON,
        p => p,
    }
}

/// Print `velt debug-alloc: use after free: <what> (address …)` without allocating or locking
/// (it runs in a fault handler).
fn report(what: &str, addr: usize) {
    let mut buf = [0u8; 256];
    let n = crate::freed::message(&mut buf, what, addr);
    write_stderr(&buf[..n]);
}

/// Is `addr` inside the reservation?
fn in_guard(addr: usize) -> bool {
    let start = GUARD_START.load(Ordering::Relaxed) as usize;
    start != 0 && addr >= start && addr - start < 2 * GUARD_SPAN
}

const FOLLOWED: &str = "followed a pointer read from a freed block";

/// Reserve the guard range and install the fault handler; called once, when the checking
/// allocator turns on (before its first allocation). Nothing here allocates.
pub(super) fn init() {
    let Some(start) = reserve(2 * GUARD_SPAN) else {
        return;
    };
    GUARD_START.store(start as u64, Ordering::Relaxed);
    install_fault_handler();
    let word = (start + GUARD_SPAN + POISON_OFFSET) as u64;
    POISON.store(word, Ordering::Relaxed);
    crate::freed::set_poison(word);
}

#[cfg(windows)]
mod os {
    use std::ffi::c_void;

    const MEM_RESERVE: u32 = 0x2000;
    const PAGE_NOACCESS: u32 = 0x01;
    const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    #[repr(C)]
    struct ExceptionRecord {
        code: u32,
        flags: u32,
        record: *mut ExceptionRecord,
        address: *mut c_void,
        params: u32,
        info: [usize; 15],
    }

    #[repr(C)]
    struct ExceptionPointers {
        record: *mut ExceptionRecord,
        context: *mut c_void,
    }

    type Handler = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn AddVectoredExceptionHandler(first: u32, handler: Handler) -> *mut c_void;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn WriteFile(
            file: *mut c_void,
            buf: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    pub(super) fn reserve(size: usize) -> Option<usize> {
        // SAFETY: reserves fresh address space, accessible to nobody.
        let p = unsafe { VirtualAlloc(std::ptr::null_mut(), size, MEM_RESERVE, PAGE_NOACCESS) };
        (!p.is_null()).then_some(p as usize)
    }

    /// An access violation inside the reservation is a pointer read from a freed block; anything
    /// else goes on to the next handler.
    unsafe extern "system" fn on_fault(p: *mut ExceptionPointers) -> i32 {
        let r = &*(*p).record;
        if r.code == STATUS_ACCESS_VIOLATION && r.params >= 2 && super::in_guard(r.info[1]) {
            super::report(super::FOLLOWED, r.info[1]);
            std::process::abort();
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    pub(super) fn install_fault_handler() {
        // SAFETY: registers a handler that only reads the exception record.
        unsafe { AddVectoredExceptionHandler(1, on_fault) };
    }

    pub(super) fn write_stderr(bytes: &[u8]) {
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        let mut n = 0u32;
        // SAFETY: writes `bytes` to the process's stderr handle.
        unsafe {
            WriteFile(
                GetStdHandle(STD_ERROR_HANDLE),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            )
        };
    }
}

#[cfg(unix)]
mod os {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) fn reserve(size: usize) -> Option<usize> {
        // SAFETY: maps fresh address space, accessible to nobody.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        (p != libc::MAP_FAILED).then_some(p as usize)
    }

    /// The handlers that were installed before ours, put back for a fault outside the guard.
    static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 2] = [
        std::mem::MaybeUninit::zeroed(),
        std::mem::MaybeUninit::zeroed(),
    ];
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn fault_addr(info: *mut libc::siginfo_t) -> usize {
        (*info).si_addr() as usize
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe fn fault_addr(info: *mut libc::siginfo_t) -> usize {
        (*info).si_addr as usize
    }

    /// A fault inside the reservation is a pointer read from a freed block. Any other fault puts
    /// the previous handler back and returns: the faulting instruction runs again and faults
    /// into it (or the default action).
    extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        // SAFETY: the kernel passes a valid siginfo; `PREVIOUS` was written before the handler
        // was installed and is only read afterwards.
        unsafe {
            let addr = fault_addr(info);
            if super::in_guard(addr) {
                super::report(super::FOLLOWED, addr);
                libc::abort();
            }
            let k = usize::from(sig != libc::SIGSEGV);
            let prev = std::ptr::addr_of!(PREVIOUS[k]).cast::<libc::sigaction>();
            libc::sigaction(sig, prev, std::ptr::null_mut());
        }
    }

    pub(super) fn install_fault_handler() {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: plain signal-handling calls; `PREVIOUS` is written here once, before the
        // handler that reads it can run.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fault as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            for (k, sig) in SIGNALS.into_iter().enumerate() {
                let prev = std::ptr::addr_of_mut!(PREVIOUS[k]).cast::<libc::sigaction>();
                libc::sigaction(sig, &sa, prev);
            }
        }
    }

    pub(super) fn write_stderr(bytes: &[u8]) {
        // SAFETY: writes `bytes` to fd 2 (async-signal-safe).
        unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
    }
}

#[cfg(not(any(windows, unix)))]
mod os {
    pub(super) fn reserve(_size: usize) -> Option<usize> {
        None
    }
    pub(super) fn install_fault_handler() {}
    pub(super) fn write_stderr(_bytes: &[u8]) {}
}

use os::{install_fault_handler, reserve, write_stderr};

#[cfg(test)]
mod tests {
    use crate::mem::{velt_rt_alloc, velt_rt_free};
    use crate::str::VeltStr;

    /// Names the use after free the child process of [`run_child`] commits.
    const CHILD: &str = "VELT_RT_DEBUG_ALLOC_UAF_CHILD";
    const TEST: &str = "debug_alloc::guard::tests::reads_of_freed_blocks_abort";

    /// The pattern of #819: `keep2` borrows an object `A { b: B { c: C { v } } }` that is
    /// freed (with what it owns) while the borrow lives on; `keep2.b.c.v` then followed `b`, the
    /// poison, and crashed (or printed garbage without the checking allocator).
    unsafe fn dangling_field_chain() {
        let c = velt_rt_alloc(8, 8);
        (c as *mut f64).write(1.0);
        let b = velt_rt_alloc(8, 8);
        (b as *mut *mut u8).write(c);
        let a = velt_rt_alloc(8, 8);
        (a as *mut *mut u8).write(b);
        let keep2 = a;
        velt_rt_free(c, 8, 8);
        velt_rt_free(b, 8, 8);
        velt_rt_free(a, 8, 8);
        let b = std::ptr::read_volatile(keep2 as *const *const u8);
        let c = std::ptr::read_volatile(b as *const *const u8);
        let v = std::ptr::read_volatile(c as *const f64);
        println!("{v}");
    }

    /// A number read from a freed block and printed.
    unsafe fn freed_number_printed() {
        let p = velt_rt_alloc(16, 8);
        (p as *mut f64).add(1).write(2.5);
        velt_rt_free(p, 16, 8);
        let v = std::ptr::read_volatile((p as *const f64).add(1));
        crate::io::velt_rt_write_f64(1, v);
    }

    /// A string kept (a bitwise copy, no count; `VeltStr` has no `Drop`) after its buffer was
    /// freed, then copied.
    unsafe fn freed_string_buffer() {
        let mut s = VeltStr::from_vec(vec![b'x'; 100]);
        let kept = std::ptr::read(&s);
        s.release();
        std::hint::black_box(kept.share());
    }

    fn run_child(case: &str) -> String {
        let exe = std::env::current_exe().expect("test executable");
        let out = crate::abi_tests::command::command(exe)
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, case)
            .env("VELT_RT_DEBUG_ALLOC", "1")
            .output()
            .expect("run the child");
        assert!(!out.status.success(), "{case}: the child did not abort");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// #872: with `VELT_RT_DEBUG_ALLOC=1` a read of a freed block aborts with `use after free`
    /// where it is used (each case in a child process, which must abort).
    #[test]
    fn reads_of_freed_blocks_abort() {
        if let Some(case) = std::env::var_os(CHILD) {
            assert!(
                super::super::enabled(),
                "the child runs with the checking allocator"
            );
            assert_ne!(super::poison(), super::BYTE_POISON, "the guard is reserved");
            unsafe {
                match case.to_str() {
                    Some("chain") => dangling_field_chain(),
                    Some("number") => freed_number_printed(),
                    Some("string") => freed_string_buffer(),
                    _ => panic!("unknown case"),
                }
            }
            // Reaching this point is the failure the parent reports.
            std::process::exit(0);
        }
        for (case, what) in [
            ("chain", "followed a pointer read from a freed block"),
            ("number", "a number read from a freed block"),
            ("string", "a string whose buffer was freed"),
        ] {
            let err = run_child(case);
            let want = format!("velt debug-alloc: use after free: {what} (address 0x");
            assert!(err.contains(&want), "{case}: stderr was:\n{err}");
        }
    }

    /// Without the checking allocator nothing is reserved and no value is taken for poison.
    #[test]
    fn values_pass_without_the_checking_allocator() {
        if std::env::var_os("VELT_RT_DEBUG_ALLOC").is_none() {
            crate::freed::check_value(super::BYTE_POISON, "unreachable");
            crate::freed::check_value(0, "unreachable");
        }
    }
}
