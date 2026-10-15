//! Whole-program pruning of the `await` steps (suspend.rs `await_step`): every `await` reads the
//! task's await word and takes a microtask step when it is not 0 (velt_rt local/micro.rs). The
//! word only becomes non-0 through the runtime functions below: a started promise's first run,
//! a `then` callback or `queueMicrotask` (`velt_rt_microtask`), a `resolve` called later
//! (`velt_rt_latch_open`), and the combinators that run or detach their losers. A program that
//! calls none of them can never observe a step, so its checks are removed: `await`-only code
//! costs what it did before the steps.

use crate::vir::{self, Callee, Terminator};

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

/// Remove the step checks of `p` when nothing in it can observe them.
pub(crate) fn prune(p: &mut vir::Program) {
    let ext = |name: &str| {
        p.externs
            .iter()
            .position(|e| e.symbol == name)
            .map(|i| vir::ExternId(i as u32))
    };
    let Some(tick) = ext("velt_rt_await_tick") else {
        return;
    };
    let observers: Vec<vir::ExternId> = OBSERVERS.iter().filter_map(|s| ext(s)).collect();
    let calls = |id: &vir::ExternId| {
        p.funcs.iter().any(|f| {
            f.blocks.iter().any(|b| {
                matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. } if e == id)
            })
        })
    };
    if observers.iter().any(calls) {
        return;
    }
    for f in &mut p.funcs {
        for i in 0..f.blocks.len() {
            let Terminator::Branch { then, els, .. } = f.blocks[i].term else {
                continue;
            };
            let slow = &f.blocks[then.0 as usize];
            let is_tick = slow.stmts.is_empty()
                && matches!(&slow.term, Terminator::Call { callee: Callee::Extern(e), .. } if *e == tick);
            if is_tick {
                f.blocks[i].term = Terminator::Goto(els);
            }
        }
    }
}
