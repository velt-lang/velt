//! Startup code for programs (and the shared runtime DLL) linked with the bundled `lld-link`
//! for `x86_64-pc-windows-msvc`: what Visual Studio's CRT objects (`msvcrt.lib`'s static part,
//! `vcruntime.lib`) provide when `link.exe` links. The C runtime itself is the Universal CRT
//! that ships with Windows 10 and later (`ucrtbase.dll`, imported through the kit's `ucrt.lib`).
//!
//! Compiled once per toolchain by `cargo xtask link-kit` into `velt_crt.obj` (executables) and,
//! with `--cfg velt_crt_dll`, `velt_crt_dll.obj` (DLLs):
//! `rustc --crate-type lib --emit obj -C panic=abort -C opt-level=2 -C overflow-checks=off`.
//! It must not reference anything but the Windows DLL imports below: the object is linked on
//! its own, without `core` (no panic handler either: the runtime's `std` defines one, and
//! nothing here can panic). See docs/internals/linking.md for why each piece is needed.

#![no_std]
#![allow(non_upper_case_globals, non_snake_case, clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_void};
use core::ptr::{addr_of, null};
#[cfg(velt_crt_dll)]
use core::ptr::addr_of_mut;

unsafe fn fastfail(code: u32) -> ! {
    unsafe { core::arch::asm!("int 0x29", in("ecx") code, options(noreturn, nostack)) }
}

type Pvfv = Option<unsafe extern "C" fn()>;
type Pifv = Option<unsafe extern "C" fn() -> c_int>;
type TlsCallback = Option<unsafe extern "system" fn(*mut c_void, u32, *mut c_void)>;

// ---- Initializer tables -------------------------------------------------------------------
// The linker sorts `.CRT$X??` sections by name and merges them into `.rdata`: C initializers
// (`.CRT$XI?`), C++ constructors (`.CRT$XC?`, mimalloc is compiled as C++ by MSVC), dynamic
// thread-local initializers (`.CRT$XD?`) and TLS callbacks (`.CRT$XL?`) lie between these
// markers.

#[used]
#[link_section = ".CRT$XIA"]
static mut __xi_a: Pifv = None;
#[used]
#[link_section = ".CRT$XIZ"]
static mut __xi_z: Pifv = None;
#[used]
#[link_section = ".CRT$XCA"]
static mut __xc_a: Pvfv = None;
#[used]
#[link_section = ".CRT$XCZ"]
static mut __xc_z: Pvfv = None;
#[used]
#[link_section = ".CRT$XDA"]
static mut __xd_a: Pvfv = None;
#[used]
#[link_section = ".CRT$XDZ"]
static mut __xd_z: Pvfv = None;
#[used]
#[link_section = ".CRT$XLA"]
static mut __xl_a: TlsCallback = None;
#[used]
#[link_section = ".CRT$XLC"]
static mut __xl_c: TlsCallback = Some(dyn_tls_init);
#[used]
#[link_section = ".CRT$XLZ"]
static mut __xl_z: TlsCallback = None;

// ---- Thread-local storage -----------------------------------------------------------------
// The TLS directory: the loader copies `.tls` (from `_tls_start` to `_tls_end`) for every
// thread and stores the module's slot in `_tls_index`. Rust's `thread_local!` and C++
// `thread_local` both compile to `.tls$` data addressed through `_tls_index`.

#[no_mangle]
#[used]
static mut _tls_index: u32 = 0;
#[no_mangle]
#[used]
#[link_section = ".tls"]
static mut _tls_start: u8 = 0;
#[no_mangle]
#[used]
#[link_section = ".tls$ZZZ"]
static mut _tls_end: u8 = 0;

#[repr(C)]
struct TlsDirectory {
    start: *const u8,
    end: *const u8,
    index: *const u32,
    callbacks: *const TlsCallback,
    zero_fill: u32,
    characteristics: u32,
}
unsafe impl Sync for TlsDirectory {}

/// `IMAGE_TLS_DIRECTORY64`; the linker points the PE TLS data directory at this symbol.
#[no_mangle]
#[used]
#[link_section = ".rdata$T"]
static _tls_used: TlsDirectory = TlsDirectory {
    start: addr_of!(_tls_start),
    end: addr_of!(_tls_end),
    index: addr_of!(_tls_index),
    // The callback list starts after the `.CRT$XLA` marker and ends at the null `.CRT$XLZ`.
    callbacks: unsafe { addr_of!(__xl_a).add(1) },
    zero_fill: 0,
    characteristics: 0,
};

// MSVC's on-demand initialization of C++ `thread_local`s (`/Zc:tlsGuards`): code reads the
// thread's `__tls_guard` byte and calls `__dyn_tls_on_demand_init` while it is 0.
core::arch::global_asm!(
    ".section .tls$,\"dw\"",
    ".globl __tls_guard",
    "__tls_guard:",
    ".byte 0",
    ".text",
    ".p2align 4",
    "velt_crt_tls_guard:",
    "mov eax, dword ptr [rip + _tls_index]",
    "mov rcx, qword ptr gs:[0x58]",
    "mov rcx, qword ptr [rcx + 8*rax]",
    "lea rax, [rcx + __tls_guard@SECREL32]",
    "ret",
);

extern "C" {
    fn velt_crt_tls_guard() -> *mut u8;
}

#[no_mangle]
pub unsafe extern "C" fn __dyn_tls_on_demand_init() {
    unsafe {
        let guard = velt_crt_tls_guard();
        if *guard != 0 {
            return;
        }
        *guard = 1;
        run_pvfv(addr_of!(__xd_a), addr_of!(__xd_z));
    }
}

unsafe extern "system" fn dyn_tls_init(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    // DLL_PROCESS_ATTACH, DLL_THREAD_ATTACH
    if reason == 1 || reason == 2 {
        unsafe { __dyn_tls_on_demand_init() }
    }
}

unsafe fn run_pvfv(mut it: *const Pvfv, end: *const Pvfv) {
    while it < end {
        // `read_volatile`: the optimizer would otherwise assume the markers are `None` and the
        // range empty.
        if let Some(f) = unsafe { it.read_volatile() } {
            unsafe { f() }
        }
        it = unsafe { it.add(1) };
    }
}

// ---- /GS stack cookies ----------------------------------------------------------------------
// The C code in the runtime (SQLite, mimalloc) is compiled with MSVC's default `/GS`.

const DEFAULT_COOKIE: u64 = 0x0000_2B99_2DDF_A232;

#[no_mangle]
#[used]
static mut __security_cookie: u64 = DEFAULT_COOKIE;
#[no_mangle]
#[used]
static mut __security_cookie_complement: u64 = !DEFAULT_COOKIE;

#[no_mangle]
pub unsafe extern "C" fn __security_init_cookie() {
    unsafe {
        if __security_cookie != DEFAULT_COOKIE {
            return;
        }
        let mut time = 0u64;
        GetSystemTimeAsFileTime(&mut time);
        let mut counter = 0i64;
        QueryPerformanceCounter(&mut counter);
        let mut cookie = time
            ^ u64::from(GetCurrentThreadId())
            ^ u64::from(GetCurrentProcessId())
            ^ ((counter as u64) << 32)
            ^ counter as u64;
        cookie ^= addr_of!(cookie) as u64;
        cookie &= 0x0000_FFFF_FFFF_FFFF;
        if cookie == DEFAULT_COOKIE {
            cookie += 1;
        }
        __security_cookie = cookie;
        __security_cookie_complement = !cookie;
    }
}

core::arch::global_asm!(
    ".text",
    ".globl __security_check_cookie",
    ".p2align 4",
    "__security_check_cookie:",
    "cmp rcx, qword ptr [rip + __security_cookie]",
    "jne 2f",
    "ret",
    "2:",
    // FAST_FAIL_STACK_COOKIE_CHECK_FAILURE
    "mov ecx, 2",
    "int 0x29",
);

/// Exception handler of functions with both `/GS` cookies and SEH/C++ EH: the real one checks
/// the frame's cookie before unwinding through it; the function's own epilogue still checks.
#[no_mangle]
pub unsafe extern "C" fn __GSHandlerCheck(
    _record: *mut c_void,
    _frame: *mut c_void,
    _context: *mut c_void,
    _dispatcher: *mut c_void,
) -> c_int {
    1 // ExceptionContinueSearch
}

#[no_mangle]
pub unsafe extern "C" fn __report_rangecheckfailure() -> ! {
    // FAST_FAIL_RANGE_CHECK_FAILURE
    unsafe { fastfail(8) }
}

// ---- C++ support the runtime's objects reference ---------------------------------------------

/// `type_info`'s vtable (`const type_info::vftable`). Rust's MSVC panics (`_CxxThrowException`)
/// put a pointer to it in their type descriptor; exception matching compares type names, so
/// the vtable is never called.
#[export_name = "??_7type_info@@6B@"]
#[used]
static TYPE_INFO_VFTABLE: [unsafe extern "C" fn(); 1] = [type_info_never_called];

unsafe extern "C" fn type_info_never_called() {
    unsafe { fastfail(7) }
}

/// `std::get_new_handler()`: mimalloc's C++ `operator new` asks for the handler to call when
/// allocation fails; there is none.
#[export_name = "?get_new_handler@std@@YAP6AXXZXZ"]
pub extern "C" fn get_new_handler() -> *const c_void {
    null()
}

// ---- atexit ------------------------------------------------------------------------------------

#[repr(C)]
struct OnexitTable {
    first: *mut c_void,
    last: *mut c_void,
    end: *mut c_void,
}

#[cfg(velt_crt_dll)]
static mut MODULE_ONEXIT: OnexitTable = OnexitTable {
    first: core::ptr::null_mut(),
    last: core::ptr::null_mut(),
    end: core::ptr::null_mut(),
};

#[no_mangle]
pub unsafe extern "C" fn atexit(f: unsafe extern "C" fn()) -> c_int {
    #[cfg(not(velt_crt_dll))]
    unsafe {
        _crt_atexit(f)
    }
    // A DLL's atexit functions run when it is unloaded, from its own table.
    #[cfg(velt_crt_dll)]
    unsafe {
        _register_onexit_function(addr_of_mut!(MODULE_ONEXIT), f)
    }
}

// ---- Entry points ------------------------------------------------------------------------------

unsafe fn initialize() -> bool {
    unsafe {
        __security_init_cookie();
        let mut it = addr_of!(__xi_a);
        while it < addr_of!(__xi_z) {
            if let Some(f) = it.read_volatile() {
                if f() != 0 {
                    return false;
                }
            }
            it = it.add(1);
        }
        run_pvfv(addr_of!(__xc_a), addr_of!(__xc_z));
        true
    }
}

#[cfg(not(velt_crt_dll))]
extern "C" {
    fn main(argc: c_int, argv: *mut *mut c_char, envp: *mut *mut c_char) -> c_int;
}

/// Executable entry point (`/SUBSYSTEM:CONSOLE`).
#[cfg(not(velt_crt_dll))]
#[no_mangle]
pub unsafe extern "C" fn mainCRTStartup() -> u32 {
    unsafe {
        _set_app_type(1); // _crt_console_app
        _configure_narrow_argv(1); // _crt_argv_unexpanded_arguments
        _initialize_narrow_environment();
        if !initialize() {
            return 255;
        }
        let code = main(*__p___argc(), *__p___argv(), _get_initial_narrow_environment());
        exit(code)
    }
}

/// DLL entry point.
#[cfg(velt_crt_dll)]
#[no_mangle]
pub unsafe extern "system" fn _DllMainCRTStartup(
    _module: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> c_int {
    unsafe {
        match reason {
            1 => {
                // DLL_PROCESS_ATTACH
                _initialize_onexit_table(addr_of_mut!(MODULE_ONEXIT));
                c_int::from(initialize())
            }
            0 => {
                // DLL_PROCESS_DETACH
                _execute_onexit_table(addr_of_mut!(MODULE_ONEXIT));
                1
            }
            _ => 1,
        }
    }
}

// ---- Imports -------------------------------------------------------------------------------------

extern "system" {
    // kernel32.dll
    fn GetSystemTimeAsFileTime(time: *mut u64);
    fn QueryPerformanceCounter(counter: *mut i64) -> c_int;
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcessId() -> u32;
}

#[allow(dead_code)]
extern "C" {
    // ucrtbase.dll
    fn _set_app_type(kind: c_int);
    fn _configure_narrow_argv(mode: c_int) -> c_int;
    fn _initialize_narrow_environment() -> c_int;
    fn _get_initial_narrow_environment() -> *mut *mut c_char;
    fn __p___argc() -> *mut c_int;
    fn __p___argv() -> *mut *mut *mut c_char;
    fn exit(code: c_int) -> !;
    fn _crt_atexit(f: unsafe extern "C" fn()) -> c_int;
    fn _initialize_onexit_table(table: *mut OnexitTable) -> c_int;
    fn _register_onexit_function(table: *mut OnexitTable, f: unsafe extern "C" fn()) -> c_int;
    fn _execute_onexit_table(table: *mut OnexitTable) -> c_int;
}
