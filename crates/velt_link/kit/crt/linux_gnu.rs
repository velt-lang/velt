//! Startup code for programs linked with the bundled `ld.lld` for `*-unknown-linux-gnu`: what
//! glibc's `Scrt1.o`, `crti.o`/`crtn.o`, gcc's `crtbeginS.o` and `libc_nonshared.a` provide when
//! `cc` links. The C library itself is the system's glibc, linked through the kit's stub shared
//! libraries (glibc 2.31 and later).
//!
//! Compiled once per toolchain by `cargo xtask link-kit` into `crt1.o`:
//! `rustc --crate-type lib --emit obj -C panic=abort -C opt-level=2 -C relocation-model=pic`.
//! It must not reference anything but glibc: the object is linked on its own, without `core` (no
//! panic handler either: the runtime's `std` defines one, and nothing here can panic). See
//! docs/internals/linking.md.

#![no_std]
#![allow(non_upper_case_globals, clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_void};

type InitFn = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char);

// The program entry: the kernel starts here with argc, argv and the environment on the stack and
// (from the dynamic loader) the loader's finalizer in rdx / x0. Hands everything to
// `__libc_start_main(main, argc, argv, init, fini, rtld_fini, stack_end)`.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".text",
    ".globl _start",
    ".type _start,@function",
    "_start:",
    "xor ebp, ebp",
    "mov r9, rdx",
    "pop rsi",
    "mov rdx, rsp",
    "and rsp, -16",
    "push rax",
    "push rsp",
    "xor r8d, r8d",
    "lea rcx, [rip + {init}]",
    "mov rdi, qword ptr [rip + main@GOTPCREL]",
    "call qword ptr [rip + __libc_start_main@GOTPCREL]",
    "hlt",
    init = sym velt_crt_init,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".text",
    ".globl _start",
    ".type _start,%function",
    "_start:",
    "mov x29, #0",
    "mov x30, #0",
    "mov x5, x0",
    "ldr x1, [sp]",
    "add x2, sp, #8",
    "mov x6, sp",
    "adrp x0, :got:main",
    "ldr x0, [x0, :got_lo12:main]",
    "adrp x3, {init}",
    "add x3, x3, :lo12:{init}",
    "mov x4, #0",
    "bl __libc_start_main",
    "brk #0",
    init = sym velt_crt_init,
);

extern "C" {
    static __preinit_array_start: [Option<InitFn>; 0];
    static __preinit_array_end: [Option<InitFn>; 0];
    static __init_array_start: [Option<InitFn>; 0];
    static __init_array_end: [Option<InitFn>; 0];
}

/// The `init` argument of `__libc_start_main`. glibc before 2.34 calls it instead of running the
/// executable's `.preinit_array` and `.init_array` itself (later versions run them when it is
/// null, and call it otherwise); Rust's `std` reads `argv` from an `.init_array` function.
unsafe extern "C" fn velt_crt_init(argc: c_int, argv: *mut *mut c_char, envp: *mut *mut c_char) {
    unsafe {
        run(
            __preinit_array_start.as_ptr(),
            __preinit_array_end.as_ptr(),
            argc,
            argv,
            envp,
        );
        run(
            __init_array_start.as_ptr(),
            __init_array_end.as_ptr(),
            argc,
            argv,
            envp,
        );
    }
}

unsafe fn run(
    mut it: *const Option<InitFn>,
    end: *const Option<InitFn>,
    argc: c_int,
    argv: *mut *mut c_char,
    envp: *mut *mut c_char,
) {
    while it < end {
        if let Some(f) = unsafe { it.read_volatile() } {
            unsafe { f(argc, argv, envp) }
        }
        it = unsafe { it.add(1) };
    }
}

// `__dso_handle` identifies the executable to `__cxa_atexit` / `__cxa_thread_atexit_impl`. Like
// gcc's `crtbeginS.o`, it is hidden (each module has its own) and points to itself.
core::arch::global_asm!(
    ".data",
    ".p2align 3",
    ".globl __dso_handle",
    ".hidden __dso_handle",
    "__dso_handle:",
    ".quad __dso_handle",
);

extern "C" {
    static mut __dso_handle: *mut c_void;
}

// ---- libc_nonshared.a ------------------------------------------------------------------------
// Before glibc 2.33 these were small static wrappers around versioned entry points, linked into
// every executable rather than exported by libc.so.6.

#[cfg(target_arch = "x86_64")]
const STAT_VER: c_int = 1;
#[cfg(target_arch = "aarch64")]
const STAT_VER: c_int = 0;

extern "C" {
    fn __xstat64(ver: c_int, path: *const c_char, buf: *mut c_void) -> c_int;
    fn __lxstat64(ver: c_int, path: *const c_char, buf: *mut c_void) -> c_int;
    fn __fxstat64(ver: c_int, fd: c_int, buf: *mut c_void) -> c_int;
    fn __fxstatat64(ver: c_int, dirfd: c_int, path: *const c_char, buf: *mut c_void, flags: c_int)
        -> c_int;
    fn __xmknodat(ver: c_int, dirfd: c_int, path: *const c_char, mode: u32, dev: *mut u64) -> c_int;
    fn __cxa_atexit(f: unsafe extern "C" fn(*mut c_void), arg: *mut c_void, dso: *mut c_void)
        -> c_int;
    fn __register_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
        dso: *mut c_void,
    ) -> c_int;
}

// `struct stat` and `struct stat64` have the same layout on both 64-bit targets.
#[no_mangle]
pub unsafe extern "C" fn stat64(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { __xstat64(STAT_VER, path, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { __xstat64(STAT_VER, path, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn lstat64(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { __lxstat64(STAT_VER, path, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut c_void) -> c_int {
    unsafe { __lxstat64(STAT_VER, path, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn fstat64(fd: c_int, buf: *mut c_void) -> c_int {
    unsafe { __fxstat64(STAT_VER, fd, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn fstat(fd: c_int, buf: *mut c_void) -> c_int {
    unsafe { __fxstat64(STAT_VER, fd, buf) }
}
#[no_mangle]
pub unsafe extern "C" fn fstatat64(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_void,
    flags: c_int,
) -> c_int {
    unsafe { __fxstatat64(STAT_VER, dirfd, path, buf, flags) }
}
#[no_mangle]
pub unsafe extern "C" fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_void,
    flags: c_int,
) -> c_int {
    unsafe { __fxstatat64(STAT_VER, dirfd, path, buf, flags) }
}
#[no_mangle]
pub unsafe extern "C" fn mknodat(dirfd: c_int, path: *const c_char, mode: u32, dev: u64) -> c_int {
    let mut dev = dev;
    // _MKNOD_VER: 0 on both targets.
    unsafe { __xmknodat(0, dirfd, path, mode, &mut dev) }
}
#[no_mangle]
pub unsafe extern "C" fn mknod(path: *const c_char, mode: u32, dev: u64) -> c_int {
    // AT_FDCWD
    unsafe { mknodat(-100, path, mode, dev) }
}

#[no_mangle]
pub unsafe extern "C" fn atexit(f: unsafe extern "C" fn()) -> c_int {
    // `void (*)(void)` called with one argument it ignores, as glibc's own atexit does.
    unsafe {
        __cxa_atexit(
            core::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(*mut c_void)>(f),
            core::ptr::null_mut(),
            core::ptr::addr_of_mut!(__dso_handle).cast(),
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_atfork(
    prepare: Option<unsafe extern "C" fn()>,
    parent: Option<unsafe extern "C" fn()>,
    child: Option<unsafe extern "C" fn()>,
) -> c_int {
    unsafe {
        __register_atfork(
            prepare,
            parent,
            child,
            core::ptr::addr_of_mut!(__dso_handle).cast(),
        )
    }
}
