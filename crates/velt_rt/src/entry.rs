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
//! The command-line arguments are std's (std/process/args.velt reads them from the OS): `main`
//! and [`velt_rt_start`] take `argc`/`argv` and leave them.

/// Initialize runtime state. Called by `main` before `velt_main`.
pub fn init() {
    crate::panic::install_hook();
    crate::io::init_stdout();
    ignore_sigpipe();
    #[cfg(all(debug_assertions, not(velt_rt_host)))]
    crate::debug_alloc::claim_faults();
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
    finish(code)
}

/// What happens once `main` returned `code`: the program's end (returns its exit code).
fn finish(code: i32) -> i32 {
    // What can still make progress (servers, timers, tasks) kept the program alive before
    // `velt_main` returned: the executor in Velt ends it (std/lang/hooks/sched_end.velt), as
    // Node's event loop does. After a failure (an uncaught error, or an exit code) nothing waits.
    if code == 0 {
        crate::str::stats::settle(tasks_may_run);
        // The leak counters see what the runtime keeps per process as freed (std dropped module
        // state and function properties already: module_blocks.velt, fn_props_env.velt).
        // Otherwise it is kept to the end, as in Node, where nothing runs at exit.
        if crate::str::stats::counting() {
            statics_drop_at_exit();
        }
        crate::str::stats::freeze(tasks_may_run);
    }
    crate::io::flush_stdout();
    crate::str::stats::report();
    crate::io::stats::report();
    #[cfg(all(debug_assertions, not(velt_rt_host)))]
    crate::debug_alloc::check_quarantine(&crate::GLOBAL_QUARANTINE);
    code
}

/// Whether a block may still be freed (for the leak counters): a Rust waker is giving back its
/// reference to a task of the executor in Velt, which may be the last (a reactor thread's late
/// wake of a task that is done).
fn tasks_may_run() -> bool {
    crate::task::waker::giving_back()
}

/// The program's function releasing what its static atomics hold (`velt_rt_statics`).
static STATICS_DROP: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// `velt_rt_statics(t)` (rt_abi.md "Process statics"): `velt_main` hands over, first thing, the
/// table of its process statics (`statics.rs`): the generated function that releases the
/// references its static atomics hold (an `Atomic<shared<T> | null>` module constant made with
/// `null`), which the runtime calls at exit when it drops the module state, for the leak
/// counters; and std's `STR_EPOCH`, the epoch of the position cache.
///
/// # Safety
/// `t` points at a table laid out as [`crate::statics::Statics`], at least `size` bytes, whose
/// addresses live as long as the process.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_statics(t: *const crate::statics::Statics) {
    let (drop, epoch) = crate::statics::fields(t);
    if !drop.is_null() {
        STATICS_DROP.store(drop as *mut (), std::sync::atomic::Ordering::Release);
    }
    if !epoch.is_null() {
        crate::str::use_epoch(epoch);
    }
}

/// At exit, for the leak counters only: the static atomics' references, after the module state.
/// Among them is `velt/sync/epoch`'s registry, whose lists hold the values retired and not yet
/// dropped.
fn statics_drop_at_exit() {
    let f = STATICS_DROP.swap(std::ptr::null_mut(), std::sync::atomic::Ordering::Acquire);
    if !f.is_null() {
        // SAFETY: stored by `velt_rt_statics` from the table's `extern "C" fn()`.
        let f: extern "C" fn() = unsafe { std::mem::transmute::<*mut (), extern "C" fn()>(f) };
        f();
    }
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

/// Run the program whose entry point is `entry` (`argc`/`argv` are not used): what
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
    #[cfg(feature = "mimalloc")]
    crate::mem::configure();
    // std reads the arguments from the OS itself (std/process/args.velt).
    let _ = (argc, argv);
    run_main(entry)
}
