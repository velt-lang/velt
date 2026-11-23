//! Whole-program check for the `await` steps (suspend.rs `await_step`): every `await` reads the
//! task's await word and takes a microtask step when it is not 0 (velt_rt local/micro.rs). The
//! word only becomes non-0 through the runtime functions below: a started promise's first run,
//! a `then` callback or `queueMicrotask` (`velt_rt_microtask`), a `resolve` called later
//! (`velt_rt_latch_open`), and the combinators that run or detach their losers. A program that
//! calls none of them can never observe a step, so `lower_program` lowers it once more without
//! any step code: `await`-only code compiles exactly as it did before the steps.

use crate::vir;

/// The runtime functions through which a program can make a task's await word non-0.
pub(crate) const OBSERVERS: &[&str] = &[
    "velt_rt_fut_start",
    "velt_rt_fut_start_rejecting",
    "velt_rt_microtask",
    "velt_rt_latch_open",
    "velt_rt_fut_detach",
    "velt_rt_race",
    "velt_rt_race_ok",
    "velt_rt_all_or_reject",
];

/// Does `p` take `await` steps that nothing in it can observe? A runtime function is declared
/// only once something calls it or takes its address. Under the executor in Velt the step and the
/// observers are its hooks, which declare no runtime function: `sched` says whether the program
/// takes the steps as the hook, and which of [`OBSERVERS`]' jobs it calls hooks for (#3081).
pub(crate) fn unobservable(p: &vir::Program, sched: (bool, bool)) -> bool {
    let (sched_steps, sched_observed) = sched;
    let declared = |name: &str| p.externs.iter().any(|e| e.symbol == name);
    (declared("velt_rt_await_tick") || sched_steps) && !sched_observed && !calls_observer(p)
}

/// Does code of `p` outside the executor in Velt call one of [`OBSERVERS`]? The executor's own
/// modules (std/lang/hooks/sched*.velt) call the runtime's latch to wake its workers, which is
/// none of the program's doing: counted, it would keep every program's steps (#3081).
fn calls_observer(p: &vir::Program) -> bool {
    let ids: Vec<u32> = (p.externs.iter().enumerate())
        .filter(|(_, e)| OBSERVERS.contains(&e.symbol.as_str()))
        .map(|(i, _)| i as u32)
        .collect();
    if ids.is_empty() {
        return false;
    }
    let calls = |b: &vir::BasicBlock| matches!(&b.term, vir::Terminator::Call { callee: vir::Callee::Extern(x), .. } if ids.contains(&x.0));
    (p.funcs.iter())
        .filter(|f| !in_executor(&f.symbol))
        .any(|f| f.blocks.iter().any(calls))
}

/// Is `symbol` a function of the executor in Velt: of a std/lang/hooks module whose name starts
/// with `sched` (`_V3stdP4langP5hooksP11sched__wakeN7publish`)?
fn in_executor(symbol: &str) -> bool {
    let Some(rest) = symbol.strip_prefix("_V3stdP4langP5hooksP") else {
        return false;
    };
    rest.trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with("sched")
}
