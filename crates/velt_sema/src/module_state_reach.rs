//! Which functions may reach module state (`super::check_locked_access`): the graph walks over
//! the [`Facts`] of every body.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Diagnostic;

use super::Facts;
use crate::ctx::Ctx;
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
/// A call through a function value has no edges: the value comes from what the callback sees,
/// so when that code calls one, the variables of the closures and functions that the code
/// making the callback makes count too, and those of the code making that code, out to a named
/// function.
pub(super) fn vars_reached(facts: &FxHashMap<DefId, Facts>, mut todo: Vec<DefId>) -> Vec<DefId> {
    let (mut seen, mut vars) = (FxHashSet::default(), vec![]);
    // The bodies whose makers' values count: the callbacks, and the bodies that call a value.
    let (mut origins, mut values) = (todo.clone(), false);
    let (mut makers, mut up) = (None::<FxHashMap<DefId, Vec<DefId>>>, FxHashSet::default());
    loop {
        while let Some(d) = todo.pop() {
            let Some(f) = facts.get(&d).filter(|_| seen.insert(d)) else {
                continue;
            };
            vars.extend(&f.vars);
            todo.extend(&f.edges);
            if f.values {
                values = true;
                origins.push(d);
            }
        }
        if !values {
            return vars;
        }
        let makers = makers.get_or_insert_with(|| {
            let mut by: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
            for (&d, f) in facts {
                for &c in &f.made {
                    by.entry(c).or_default().push(d);
                }
            }
            by
        });
        while let Some(d) = origins.pop() {
            for &m in makers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
                if up.insert(m) {
                    todo.extend(&facts[&m].made);
                    origins.push(m);
                }
            }
        }
        if todo.is_empty() {
            return vars;
        }
    }
}

/// An `@export` function may run on a thread Velt didn't start (a C library's worker, a thread
/// C starts on it), which has no task to hand the module lock back at the end of a step: the
/// lock its first use of module state takes is never released, and the next use anywhere waits
/// for ever. So module state is not reachable from one, by the walk that refuses it in a `with`
/// callback.
pub(super) fn refuse_in_exports(cx: &mut Ctx, reach: &FxHashMap<DefId, Reach>) {
    let found: Vec<_> = (cx.c_exports.iter())
        .filter_map(|e| Some((e.span, e.name.clone(), reach.get(&e.def)?.clone())))
        .collect();
    for (span, name, r) in found {
        let what = match r.via.is_empty() {
            true => r.what,
            false => {
                let chain: Vec<String> = r.via.iter().map(|&f| super::node_name(cx, f)).collect();
                format!("{} (through {})", r.what, chain.join(" -> "))
            }
        };
        cx.error(
            Diagnostic::error(
                format!("the `@export` function `{name}` may use module state: it {what}"),
                span,
            )
            .with_note("C may call it on a thread Velt didn't start, which can't take the module lock and give it back: module-level `let`s and kept constants can't be used from it")
            .with_note("pass what it needs in its arguments (C keeps a `Ptr` for it), and return what it computes"),
        );
    }
}
