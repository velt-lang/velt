//! std's functions whose error type the app's stage infers again (`VELT_UNITS=2`). std's stage
//! inferred every std function's error type without the program, so a std function that runs
//! the program's code (a callback or a trap through a function value, an interface method's
//! implementation, a class's field initializers) has only what std itself throws there, and so
//! do the std functions that call it. A whole-program check infers them together with the
//! program's code. Error types only grow, and what std's code alone throws is part of what the
//! whole program does, so the app's fixpoint starts them from std's types and reaches the same
//! answer (`infer::infer_all`).

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::{BodyState, ThrowSrc};
use crate::hir::DefId;

/// In the app's stage of a check by units, std's functions in the std code the program has
/// (`lazy_std::walks`) that may run the program's code, and the std functions that call them;
/// otherwise none (the fixpoint has every function already).
pub(super) fn open_std_fns(cx: &mut Ctx) -> Vec<DefId> {
    if cx.units_stage != crate::units::Stage::App {
        return vec![];
    }
    crate::body::lazy_std::prepare_walk(cx);
    let base = cx.unit_base;
    let std: Vec<DefId> = (cx.fn_defs.iter().copied())
        .filter(|d| d.0 < base && cx.fn_info(*d).state == BodyState::Done)
        .filter(|&d| crate::body::lazy_std::walks(cx, d))
        .collect();
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    let mut open = vec![];
    for &d in &std {
        let mut runs_program = false;
        for s in &cx.fn_info(d).throw_srcs {
            match s {
                ThrowSrc::Call(g, _, _) if g.0 >= base => runs_program = true,
                ThrowSrc::Call(g, _, _) => callers.entry(*g).or_default().push(d),
                ThrowSrc::Indirect(..) | ThrowSrc::Slot { .. } | ThrowSrc::Defaults(..) => {
                    runs_program = true
                }
                ThrowSrc::Direct(..) => {}
            }
        }
        if runs_program {
            open.push(d);
        }
    }
    let mut seen: rustc_hash::FxHashSet<DefId> = open.iter().copied().collect();
    let mut todo = open.clone();
    while let Some(d) = todo.pop() {
        for &c in callers.get(&d).into_iter().flatten() {
            if seen.insert(c) {
                open.push(c);
                todo.push(c);
            }
        }
    }
    open.sort();
    open
}
