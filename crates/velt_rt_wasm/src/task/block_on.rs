//! The executor's loop: `velt_rt_block_on` and the turns it is made of. A turn runs the
//! microtasks, the ready tasks and the due timers until none is left ([`run_until_idle`]); then
//! `velt_rt_block_on` sleeps until the next timer (natively and under WASI). A JS host's thread
//! cannot block: there the host calls for each turn (`crate::host_turn`).

use std::task::Context;

use super::{
    begin_poll, end_poll, fire_due_now, next_micro, next_ready, raw_cx, resuming, run_task,
    schedule, set_running_word, waker, with_exec, PollFn, READY, ROOT,
};
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
use {super::fire_due, crate::platform};

/// The earliest pending timer's deadline (`platform::monotonic_ms` time), if any.
pub fn next_deadline() -> Option<f64> {
    let sleep = with_exec(|e| e.timers.peek().map(|t| t.0.deadline));
    [sleep, crate::task::order::first_expiry()]
        .into_iter()
        .flatten()
        .reduce(f64::min)
}

/// Sleep until the next timer and fire every timer that is due; false if there is none. A JS
/// host's thread never waits: there `main`'s root runs in the host's turns (host_turn.rs).
fn wait_for_timers() -> bool {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    return false;
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        let Some(first) = next_deadline() else {
            return false;
        };
        platform::sleep_ms(first - platform::monotonic_ms());
        fire_due(platform::monotonic_ms());
        true
    }
}

/// The root task `block_on` drives: its poll function, its state, whether it finished, and
/// whether the code after it ran (`main` returned 0: the program goes on while tasks run).
#[derive(Clone, Copy)]
pub struct Root {
    poll: PollFn,
    state: *mut u8,
    done: bool,
    after: bool,
}

impl Root {
    /// The address of the root's state (its result is at offset 0).
    pub fn state(&self) -> *mut u8 {
        self.state
    }

    /// Has the root finished?
    pub fn done(&self) -> bool {
        self.done
    }

    /// Has `main` returned 0, with the program going on until its tasks are done?
    pub fn after_main(&self) -> bool {
        self.after
    }

    /// `main` returned 0: from now on the program ends once its tasks are done.
    pub fn go_on_after_main(&mut self) {
        self.after = true;
    }
}

/// Start running tasks with the root task `poll` (state at `state`) queued.
pub fn start_root(poll: PollFn, state: *mut u8) -> Root {
    with_exec(|e| e.running = true);
    set_running_word(true);
    schedule(ROOT);
    Root {
        poll,
        state,
        done: false,
        after: false,
    }
}

/// Run the microtasks, the ready tasks and the due timers until none is left (false), or until
/// what the program waits for is done (true): until `main` (the root) finished and the
/// microtasks it queued ran, so the code after it reports an uncaught error at once, as on
/// native targets; after `main` returned 0 ([`Root::after_main`]), until every task `spawn`
/// started and every started promise finished and no keep-alive reference (a ref'd timer) is
/// held, as native programs wait for them before they exit.
pub unsafe fn run_until_idle(root: &mut Root) -> bool {
    loop {
        if root.done && with_exec(|e| e.micro.is_empty()) && !resuming() {
            crate::task::local::flush_unhandled();
            if !root.after {
                return true;
            }
        }
        let quiet = with_exec(|e| e.locals == 0 && e.keep_alive == 0 && e.spawned == 0);
        if root.after && quiet {
            return true;
        }
        fire_due_now();
        let Some(id) = next_micro().or_else(|| {
            // The microtask queue is drained (and no awaiter resumes): unhandled rejections are
            // reported now.
            if !resuming() {
                crate::task::local::flush_unhandled();
            }
            next_ready()
        }) else {
            return false;
        };
        if id != ROOT {
            run_task(id);
            continue;
        }
        if root.done {
            continue;
        }
        let w = waker(ROOT);
        let mut cx = Context::from_waker(&w);
        begin_poll(ROOT);
        root.done = (root.poll)(root.state, raw_cx(&mut cx).ptr()) == READY;
        end_poll();
    }
}

/// The end of the program's tasks: those still running are abandoned.
pub fn stop() {
    with_exec(|e| e.running = false);
    set_running_word(false);
}

thread_local! {
    /// The root `velt_rt_block_on` returned with, for the tasks after `main` ([`after_main`]).
    static RETURNED: std::cell::Cell<Option<Root>> = const { std::cell::Cell::new(None) };
}

/// `async main`: drive the compiled root state machine (and every task it spawns) until the
/// root is ready and the microtasks it queued ran, or until nothing can wake any task (the
/// program ends then, with exit code 0 when the root never finished, or 13 when it waits at a
/// top-level `await`). `velt_main` then runs the code after it; when `main` returned 0, the
/// runtime runs the tasks on until they are done ([`after_main`]).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    let mut root = start_root(poll, state);
    while !run_until_idle(&mut root) && wait_for_timers() {}
    if !root.done {
        // Nothing can settle the top-level `await` the root waits at, as Node ends.
        crate::panic::velt_rt_exit(crate::top_level_await::unfinished_exit_code());
    }
    RETURNED.set(Some(root));
}

/// `main` returned 0 after `velt_rt_block_on`: run the tasks still running until they are done
/// or nothing can wake them (natively and under WASI; a JS host runs them in its turns).
pub fn after_main() {
    let Some(mut root) = RETURNED.take() else {
        return;
    };
    root.go_on_after_main();
    // SAFETY: the executor's own loop, on its thread; the root is done and never polled again.
    unsafe { while !run_until_idle(&mut root) && wait_for_timers() {} }
    stop();
}
