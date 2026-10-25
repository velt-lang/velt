//! Program entry. Generated code exports `int32_t velt_main(void)` (rt_abi.md "Process entry");
//! the runtime calls it:
//! - WASI: wasi-libc's `_start` (crt1-command.o) calls `__main_void`, defined here, and exits
//!   with its result;
//! - browser: the JS glue calls the exported `velt_start` and reads its result (an explicit
//!   exit or a panic throws out of it instead, see `platform`).

use crate::{io, panic};

/// Run a program: install the panic hook, call `velt_main`, flush stdout; returns the exit code.
pub fn run_main(velt_main: extern "C" fn() -> i32) -> i32 {
    panic::install_hook();
    run_program(velt_main)
}

/// [`run_main`] without the panic hook. The hook is process-wide and exits on any panic, so the
/// unit tests call this: with the hook installed, every later `#[should_panic]` test in the same
/// test process (`cargo test`) would exit the process instead of unwinding.
fn run_program(velt_main: extern "C" fn() -> i32) -> i32 {
    let code = velt_main();
    io::flush_stdout();
    code
}

#[cfg(all(target_family = "wasm", not(test)))]
extern "C" {
    fn velt_main() -> i32;
}

/// Safe wrapper for the generated `velt_main`.
#[cfg(all(target_family = "wasm", not(test)))]
extern "C" fn program() -> i32 {
    // SAFETY: provided by the generated object, contract `int32_t velt_main(void)`.
    unsafe { velt_main() }
}

/// WASI command entry (called by crt1-command.o's `_start`).
#[cfg(all(target_os = "wasi", not(test)))]
#[no_mangle]
pub extern "C" fn __main_void() -> i32 {
    run_main(program)
}

/// Browser entry, exported for the JS glue.
#[cfg(all(target_family = "wasm", target_os = "unknown", not(test)))]
#[no_mangle]
pub extern "C" fn velt_start() -> i32 {
    run_main(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_program_result() {
        extern "C" fn seven() -> i32 {
            7
        }
        assert_eq!(run_program(seven), 7);
        // The test process keeps Rust's own hook: a panic still unwinds (the runtime's hook would
        // exit with 101 here, and in every `#[should_panic]` test that ran after this one).
        assert!(std::panic::catch_unwind(|| panic!("unwinds")).is_err());
    }
}
