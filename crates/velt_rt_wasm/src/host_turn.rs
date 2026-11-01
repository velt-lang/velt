//! Turns the JS host drives (wasm32-unknown-unknown, rt_abi_async.md §2.5). The thread belongs
//! to the host's event loop, so nothing waits on it: `velt_main` hands the root task of `main`
//! to [`velt_rt_host_main`], which moves its state to the heap and runs the first turn, and
//! `velt_start` returns. Each later turn is a call from the host that returns normally:
//! `velt_turn()` when the timer it asked for is due, `velt_settle(id)` when a host promise
//! settles (host_promise.rs). A turn runs the microtasks, the ready tasks and the due timers
//! until none is left, as `velt_rt_block_on` does between its sleeps, then asks the host for a
//! wake-up at the earliest timer (`velt.set_timer`; every turn asks again, as hosts round
//! delays). Once the root is done, the runtime runs the code after it (`finish`: the uncaught
//! error, the exit code); when that is 0, the tasks `main` started go on, as natively, until they
//! are done or nothing can wake them. The exit code reaches the host through `velt.done`.
//! Nothing unwinds through the module.

use std::alloc::Layout;
use std::cell::{Cell, RefCell};

use crate::task::executor::{self, Root};
use crate::task::PollFn;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// Call `velt_turn` in `ms` milliseconds, replacing the previous request (none for a
        /// negative `ms`).
        pub fn set_timer(ms: f64);
        /// The program ended with exit code `code`.
        pub fn done(code: i32);
    }
}

/// The glue's imports for the native unit tests: they record what they were called with.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod imports {
    use std::cell::Cell;

    thread_local! {
        pub static TIMER: Cell<Option<f64>> = const { Cell::new(None) };
        pub static DONE: Cell<Option<i32>> = const { Cell::new(None) };
    }

    pub unsafe fn set_timer(ms: f64) {
        TIMER.set(Some(ms));
    }

    pub unsafe fn done(code: i32) {
        DONE.set(Some(code));
    }
}

/// The code after the root of `main`: the exit code, given the root's state.
type FinishFn = unsafe extern "C" fn(state: *mut u8) -> i32;

/// The program the host runs: the root of `main`, its state's heap layout, and its finish.
struct Program {
    root: Root,
    layout: Layout,
    finish: FinishFn,
}

thread_local! {
    static PROGRAM: RefCell<Option<Program>> = const { RefCell::new(None) };
    /// `velt_main` handed its root over (so `velt_start` does not report the end itself).
    static HOSTED: Cell<bool> = const { Cell::new(false) };
    /// A program runs (from `velt_rt_host_main` until `velt.done`), also during a turn.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// `velt_rt_host_main(poll, state, size, align, finish)`: run the root task `poll`, whose
/// unpolled state of `size` bytes is at `state`, in the turns the host drives; `finish` runs once
/// it is done. Runs the first turn.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_host_main(
    poll: PollFn,
    state: *const u8,
    size: u64,
    align: u64,
    finish: FinishFn,
) {
    if RUNNING.replace(true) {
        crate::panic::fatal("`velt_start` ran while a program is running");
    }
    let layout = Layout::from_size_align(size.max(1) as usize, align.max(1) as usize)
        .unwrap_or_else(|_| crate::panic::fatal("the state of `main` has no valid layout"));
    let heap = std::alloc::alloc(layout);
    if heap.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    std::ptr::copy_nonoverlapping(state, heap, size as usize);
    let root = executor::start_root(poll, heap);
    PROGRAM.with(|p| {
        *p.borrow_mut() = Some(Program {
            root,
            layout,
            finish,
        })
    });
    HOSTED.set(true);
    velt_turn();
}

/// Did `velt_main` hand its root to the host's turns?
pub fn hosted() -> bool {
    HOSTED.get()
}

/// `velt_turn()`: a turn (see the module docs). Nothing to do once the program ended.
#[no_mangle]
pub unsafe extern "C" fn velt_turn() {
    let Some(mut program) = PROGRAM.with(|p| p.borrow_mut().take()) else {
        return;
    };
    let code = loop {
        let settled = executor::run_until_idle(&mut program.root);
        crate::io::flush_stdout();
        if !settled && can_wake() {
            let now = crate::platform::monotonic_ms();
            let ms = executor::next_deadline().map_or(-1.0, |d| (d - now).max(0.0));
            imports::set_timer(ms);
            PROGRAM.with(|p| *p.borrow_mut() = Some(program));
            return;
        }
        if !program.root.done() {
            // Nothing can settle the top-level `await` the root waits at, as Node ends.
            std::alloc::dealloc(program.root.state(), program.layout);
            break crate::top_level_await::unfinished_exit_code();
        }
        if program.root.after_main() {
            break 0;
        }
        // `main` is done: the code after it, then (when it returned 0) its tasks, as natively.
        let code = (program.finish)(program.root.state());
        std::alloc::dealloc(program.root.state(), program.layout);
        if code != 0 {
            break code;
        }
        program.root.go_on_after_main();
    };
    imports::set_timer(-1.0);
    executor::stop();
    RUNNING.set(false);
    crate::io::flush_stdout();
    imports::done(code);
}

/// Can a timer or a host promise still wake a task?
fn can_wake() -> bool {
    executor::next_deadline().is_some() || crate::host_promise::pending()
}

/// `velt_settle(id)`: the JS promise the glue keeps under `id` settled: wake its task, run a
/// turn.
#[no_mangle]
pub unsafe extern "C" fn velt_settle(id: u32) {
    crate::host_promise::settle(id);
    velt_turn();
}

/// End the program `velt_main` returned `code` for, unless it handed its root to the host.
pub fn main_returned(code: i32) {
    if !hosted() {
        // SAFETY: a plain import taking a number.
        unsafe { imports::done(code) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};
    use std::ffi::c_void;

    /// A root that awaits `sleep(0)` then `sleep(60_000)`: state `{ step, fut }`.
    unsafe extern "C" fn sleeper(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut usize;
        loop {
            if *st.add(1) == 0 {
                let ms = if *st == 0 { 0 } else { 60_000 };
                *st.add(1) = crate::task::velt_rt_sleep(ms) as usize;
            }
            let f = *st.add(1) as *mut crate::task::VeltFut;
            if velt_rt_fut_poll(f, cx) == PENDING {
                return PENDING;
            }
            velt_rt_fut_drop(f);
            *st.add(1) = 0;
            *st += 1;
        }
    }

    /// The exit code is 40 plus the word at the root's state.
    unsafe extern "C" fn finish(state: *mut u8) -> i32 {
        40 + *(state as *const usize) as i32
    }

    #[test]
    fn turns_run_the_root_and_ask_for_a_timer_each_time() {
        unsafe {
            let state = [0usize; 2];
            velt_rt_host_main(sleeper, state.as_ptr() as *const u8, 16, 8, finish);
            assert!(hosted());
            // The first turn went on past `sleep(0)` to the long sleep, and asked for a wake-up.
            assert!(imports::TIMER.get().is_some_and(|ms| ms > 50_000.0));
            // An early call (hosts round delays) fires nothing, and asks again.
            imports::TIMER.set(None);
            velt_turn();
            assert!(imports::TIMER.get().is_some());
            assert_eq!(imports::DONE.get(), None);
            assert!(PROGRAM.with(|p| p.borrow().is_some()));
        }
    }

    /// A root that awaits the host promise 5.
    unsafe extern "C" fn waits(s: *mut u8, cx: *mut c_void) -> u32 {
        let f = s as *mut *mut crate::task::VeltFut;
        if (*f).is_null() {
            *f = crate::host_promise::velt_rt_host_promise(5);
        }
        if velt_rt_fut_poll(*f, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(*f);
        *(s as *mut usize) = 2;
        READY
    }

    #[test]
    fn a_settled_host_promise_wakes_its_task_and_the_program_ends() {
        unsafe {
            let state = [0usize];
            velt_rt_host_main(waits, state.as_ptr() as *const u8, 8, 8, finish);
            assert_eq!(imports::TIMER.get(), Some(-1.0), "no timer to wait for");
            assert_eq!(
                imports::DONE.get(),
                None,
                "the host promise can still settle"
            );
            velt_settle(5);
            assert_eq!(imports::DONE.get(), Some(42));
            // The program ended: a late call does nothing.
            imports::DONE.set(None);
            velt_turn();
            assert_eq!(imports::DONE.get(), None);
        }
    }

    /// A root that spawns a task waiting for host promise 6, holds a keep-alive reference (a
    /// ref'd timer's), and finishes.
    unsafe extern "C" fn spawns(_: *mut u8, _: *mut c_void) -> u32 {
        let leaf = crate::host_promise::velt_rt_host_promise(6);
        executor::spawn_task(leaf, 72, None);
        executor::velt_rt_keep_alive_acquire();
        READY
    }

    unsafe extern "C" fn returns_0(_: *mut u8) -> i32 {
        0
    }

    #[test]
    fn spawned_tasks_keep_the_program_after_main_returns_0() {
        unsafe {
            velt_rt_host_main(spawns, [0usize].as_ptr() as *const u8, 8, 8, returns_0);
            assert_eq!(imports::DONE.get(), None, "the spawned task waits");
            executor::velt_rt_keep_alive_release();
            velt_settle(6);
            assert_eq!(imports::DONE.get(), Some(0));
        }
    }

    #[test]
    fn an_exit_code_ends_the_program_at_once() {
        unsafe {
            // `finish` gives 40: an exit code, with the spawned task waiting and a keep-alive
            // reference held.
            velt_rt_host_main(spawns, [0usize].as_ptr() as *const u8, 8, 8, finish);
            assert_eq!(imports::DONE.get(), Some(40));
        }
    }
}
