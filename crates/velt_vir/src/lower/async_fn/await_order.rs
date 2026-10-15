//! Whole-program check for the `await` steps (suspend.rs `await_step`): every `await` reads the
//! task's await word and takes a microtask step when it is not 0 (velt_rt local/micro.rs). The
//! word only becomes non-0 through the runtime functions below: a started promise's first run,
//! a `then` callback or `queueMicrotask` (`velt_rt_microtask`), a `resolve` called later
//! (`velt_rt_latch_open`), and the combinators that run or detach their losers. A program that
//! calls none of them can never observe a step, so `lower_program` lowers it once more without
//! any step code: `await`-only code compiles exactly as it did before the steps.

use crate::vir;

/// The runtime functions through which a program can make a task's await word non-0.
const OBSERVERS: &[&str] = &[
    "velt_rt_fut_start",
    "velt_rt_microtask",
    "velt_rt_latch_open",
    "velt_rt_fut_detach",
    "velt_rt_race",
    "velt_rt_race_ok",
    "velt_rt_all_or_reject",
];

/// Does `p` take `await` steps that nothing in it can observe? A runtime function is declared
/// only once something calls it or takes its address.
pub(crate) fn unobservable(p: &vir::Program) -> bool {
    let declared = |name: &str| p.externs.iter().any(|e| e.symbol == name);
    declared("velt_rt_await_tick") && !OBSERVERS.iter().any(|s| declared(s))
}
