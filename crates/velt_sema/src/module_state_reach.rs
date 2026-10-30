//! Which functions may reach module state (`super::check_locked_access`): the graph walks over
//! the [`Facts`] of every body.

use rustc_hash::{FxHashMap, FxHashSet};

use super::Facts;
use crate::hir::DefId;

/// Why a function may reach module state: the `let` or kept `const` it uses, or that it calls a
/// function value whose body is not known; with the calls that lead there.
#[derive(Clone)]
pub(super) struct Reach {
    pub what: String,
    pub via: Vec<DefId>,
}

/// Which functions may reach module state: those that use it, and those that may run one of
/// them (backwards along the edges, to a fixpoint).
pub(super) fn propagate(facts: &FxHashMap<DefId, Facts>) -> FxHashMap<DefId, Reach> {
    let mut reach: FxHashMap<DefId, Reach> = FxHashMap::default();
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    let mut todo = vec![];
    // In `DefId` order, so a diagnostic names the same path on every run.
    let mut order: Vec<(&DefId, &Facts)> = facts.iter().collect();
    order.sort_by_key(|(d, _)| **d);
    for (&d, f) in order {
        for &e in &f.edges {
            callers.entry(e).or_default().push(d);
        }
        if let Some(what) = &f.direct {
            let r = Reach {
                what: what.clone(),
                via: vec![],
            };
            reach.insert(d, r);
            todo.push(d);
        }
    }
    while let Some(d) = todo.pop() {
        let r = reach[&d].clone();
        for &c in callers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
            if reach.contains_key(&c) {
                continue;
            }
            let mut via = vec![d];
            via.extend(r.via.iter().copied());
            let what = r.what.clone();
            reach.insert(c, Reach { what, via });
            todo.push(c);
        }
    }
    reach
}

/// Every module state variable the code from `todo` may reach (along the edges), so a
/// refused variable moves in one extra check however many a callback reaches (`script_locals`).
pub(super) fn vars_reached(facts: &FxHashMap<DefId, Facts>, mut todo: Vec<DefId>) -> Vec<DefId> {
    let (mut seen, mut vars) = (FxHashSet::default(), vec![]);
    while let Some(d) = todo.pop() {
        let Some(f) = facts.get(&d).filter(|_| seen.insert(d)) else {
            continue;
        };
        vars.extend(&f.vars);
        todo.extend(&f.edges);
    }
    vars
}
