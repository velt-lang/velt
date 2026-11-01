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

/// The root task `block_on` drives: its poll function, its state, and whether it finished.
#[derive(Clone, Copy)]
pub struct Root {
    poll: PollFn,
    state: *mut u8,
    done: bool,
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
    }
}

/// Run the microtasks, the ready tasks and the due timers until none is left. True once the
/// program may end: the root finished, every started promise finished and no keep-alive
/// reference (a ref'd timer) is held.
pub unsafe fn run_until_idle(root: &mut Root) -> bool {
    loop {
        if root.done && with_exec(|e| e.micro.is_empty()) && !resuming() {
            crate::task::local::flush_unhandled();
        }
        if root.done && with_exec(|e| e.locals == 0 && e.keep_alive == 0) {
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

/// `async main`: drive the compiled root state machine (and every task it spawns) until the
/// root is ready, every started promise finished and no keep-alive reference (a ref'd timer) is
/// held, or until nothing can wake any of them (the program ends then, with exit code 0 when the
/// root never finished, or 13 when it waits at a top-level `await`). Spawned tasks still running
/// afterwards are abandoned.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    let mut root = start_root(poll, state);
    while !run_until_idle(&mut root) && wait_for_timers() {}
    if !root.done {
        // Nothing can settle the top-level `await` the root waits at, as Node ends.
        crate::panic::velt_rt_exit(crate::top_level_await::unfinished_exit_code());
    }
    stop();
}
