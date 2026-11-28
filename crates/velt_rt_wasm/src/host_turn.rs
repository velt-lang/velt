//! The end of a program on a JS host (wasm32-unknown-unknown, rt_abi_async.md §2.5): `velt_start`
//! tells the host `main`'s exit code through `velt.done`. A program that runs tasks hands its
//! root to the turns the host drives, which are the executor in Velt's (std's
//! `std/lang/hooks/sched_host.velt`, #3905): it tells the glue so first (`velt:turns.hosted`),
//! so the glue waits for the end those turns report and ignores this one.

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// The program ended with exit code `code`.
        pub fn done(code: i32);
    }
}

/// The glue's import for the native unit tests: it records what it was called with.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod imports {
    use std::cell::Cell;

    thread_local! {
        pub static DONE: Cell<Option<i32>> = const { Cell::new(None) };
    }

    pub unsafe fn done(code: i32) {
        DONE.set(Some(code));
    }
}

/// `velt_main` returned `code`: the host is told (a hosted program's glue waits for its turns').
pub fn main_returned(code: i32) {
    // SAFETY: a plain import taking a number.
    unsafe { imports::done(code) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_code_reaches_the_host() {
        main_returned(3);
        assert_eq!(imports::DONE.get(), Some(3));
    }
}
