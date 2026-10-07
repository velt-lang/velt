//! Process entry: the C `main` that the platform CRT start-up code calls
//! (`mainCRTStartup` on Windows MSVC, `__libc_start_main`/`_start` on Linux, `dyld` on macOS).
//!
//! Generated code provides `int32_t velt_main(void)`. The runtime owns `main` so that executables
//! need nothing but `<program>.o + velt_rt + native libs`.
//!
//! `main` is compiled out under `cfg(test)` so the unit-test harness (which has its own `main`)
//! links, in `velt_rt_host` (the same sources linked into `velt` for the `velt dev` JIT
//! host, which calls [`run_main`] with JIT-compiled code), and in `velt_rt_shared` (the shared
//! library debug builds link: a DLL/dylib cannot leave `velt_main` undefined, so there the
//! executable carries a small generated `main` that passes `velt_main` to [`velt_rt_start`]). The *non-test* `velt_rt` rlib still
//! contains `main`, so Rust integration tests must not link it: the ABI tests are compiled into
//! the unit-test binary instead (see `lib.rs`), and tests/link_check.rs uses the staticlib only.
//! Command-line arguments come from `argv` on Unix (musl gives Rust's `std::env::args` nothing)
//! and from `std::env::args_os` (wide APIs) on Windows; the JIT host sets them itself.

/// Initialize runtime state. Called by `main` before `velt_main`.
pub fn init() {
    crate::panic::install_hook();
    crate::process::init_clock();
    crate::process::init_script();
    ignore_sigpipe();
}

/// Writing to a socket whose peer is gone must fail with `EPIPE`, not kill the process: tokio
/// writes with plain `write(2)` on Linux (macOS sockets opt out with `SO_NOSIGPIPE`), so a
/// server whose client hung up died of `SIGPIPE`. Rust's own start-up does the same.
fn ignore_sigpipe() {
    #[cfg(unix)]
    // SAFETY: setting a signal disposition to SIG_IGN has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Standard output is a closed pipe (`program | head`): end the process the way it ended before
/// SIGPIPE was ignored (killed by the signal), instead of printing into the void.
#[cfg(unix)]
pub(crate) fn die_of_broken_pipe() -> ! {
    // SAFETY: restoring the default disposition and raising the signal terminates the process.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        libc::raise(libc::SIGPIPE);
    }
    std::process::exit(141)
}

/// Run a program: initialize the runtime, call its `velt_main`, flush stdout; returns the exit
/// code. What `main` does for linked executables, and what the JIT host does with JIT code.
pub fn run_main(velt_main: extern "C" fn() -> i32) -> i32 {
    init();
    let code = velt_main();
    // Servers and pending promises keep a program alive after `main` returns, but not after it
    // failed (an uncaught error, or an exit code): like Node, which exits on an uncaught
    // exception whatever is still listening.
    if code == 0 {
        crate::task::runtime::wait_for_keep_alive();
    }
    crate::io::flush_stdout();
    crate::str::stats::report();
    crate::io::stats::report();
    #[cfg(all(debug_assertions, not(velt_rt_host)))]
    crate::debug_alloc::check_quarantine();
    code
}

#[cfg(not(any(test, velt_rt_host, velt_rt_shared)))]
extern "C" {
    fn velt_main() -> i32;
}

#[cfg(not(any(test, velt_rt_host, velt_rt_shared)))]
///
/// # Safety
/// Called by the C runtime with `argc` valid NUL-terminated strings in `argv`.
#[no_mangle]
pub unsafe extern "C" fn main(
    argc: std::ffi::c_int,
    argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    /// Safe wrapper for the generated entry point.
    extern "C" fn program() -> i32 {
        // SAFETY: provided by the generated object, contract `int32_t velt_main(void)`.
        unsafe { velt_main() }
    }
    velt_rt_start(argc, argv, program)
}

/// Run the program whose entry point is `entry` with the process arguments `argc`/`argv`: what
/// `main` does. Exported for executables linked against the shared runtime, whose own `main`
/// (generated, see `velt_codegen_cl::emit_entry_object`) calls it with `velt_main`.
///
/// # Safety
/// `argv` must hold `argc` valid NUL-terminated strings; `entry` follows the `velt_main` contract.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_start(
    argc: std::ffi::c_int,
    argv: *const *const std::ffi::c_char,
    entry: extern "C" fn() -> i32,
) -> std::ffi::c_int {
    #[cfg(unix)]
    // SAFETY: the caller passes `argc` valid NUL-terminated strings in `argv`.
    crate::process::set_args(args_from_c(argc, argv));
    #[cfg(not(unix))]
    let _ = (argc, argv);
    run_main(entry)
}

/// The arguments `main` received. On Unix, `std::env::args` only works where the C library hands
/// `argc`/`argv` to start-up hooks (glibc does, musl does not: a Velt program on Alpine saw no
/// arguments at all), because Rust's own `main` is not the one that runs. Windows reads them
/// from the OS (wide APIs) instead, through `std::env::args_os`.
///
/// # Safety
/// `argv` must hold `argc` valid NUL-terminated strings.
#[cfg(unix)]
unsafe fn args_from_c(argc: std::ffi::c_int, argv: *const *const std::ffi::c_char) -> Vec<String> {
    (0..argc.max(0) as usize)
        .map(|i| *argv.add(i))
        .filter(|p| !p.is_null())
        .map(|p| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
        .collect()
}
