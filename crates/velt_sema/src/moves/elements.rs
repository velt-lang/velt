//! Variables bound by reference into an element (a `for…of` variable, or a destructured part
//! that borrows the matched place) that a closure assigns, stored or called on the spot
//! (`for (let x of xs) { const f = () => { x += "!"; }; f(); }`): such a variable lives in a
//! cell, which gets its own copy of the element each time the binding binds, so the closure
//! never writes into the array (#799). One that cannot live in a cell is an error.

use std::collections::{HashMap, HashSet};

use velt_common::Diagnostic;

use super::{assigned_locals, Outcome};
use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, LocalId, Pat, PatKind, UseMode};

/// Put the variables of [`element_writes`] in cells (`out.boxed`) when they may live in one
/// (`shared` or `copy`, per function and local, and not in `async_captured`: captured by an
/// async closure that may run on another thread); report the others in `all`.
pub(super) fn box_element_writes(
    cx: &mut Ctx,
    (shared, copy): (&[Vec<bool>], &[Vec<bool>]),
    async_captured: &HashSet<(DefId, LocalId)>,
    out: &mut Outcome,
    all: &mut Vec<Diagnostic>,
) {
    for (p, outer, c, inner) in element_writes(cx) {
        let (i, l) = (p.0 as usize, outer.0 as usize);
        if (shared[i][l] || copy[i][l]) && !async_captured.contains(&(p, outer)) {
            out.boxed.entry(p).or_default().insert(outer);
            continue;
        }
        let Some(Def::Fn(f)) = &cx.defs[i] else {
            continue;
        };
        let (name, declared) = (f.body.locals[l].name.clone(), f.body.locals[l].span);
        let why = match async_captured.contains(&(p, outer)) {
            true => {
                format!("an async closure that may run on another thread captures `{name}` too")
            }
            false => format!("`{name}` holds a promise, which cannot be shared"),
        };
        let at = crate::ownership::assignment_in(cx, c, inner)
            .or_else(|| cx.def_spans.get(c.0 as usize).copied())
            .unwrap_or(declared);
        all.push(
            Diagnostic::error(
                format!("cannot assign to `{name}`, which borrows an array element"),
                at,
            )
            .with_note(format!(
                "a closure that assigns `{name}` needs a copy of the element in a cell of its own, and {why}; copy it into a variable of its own (`let v = {name};`) and assign that one"
            )),
        );
    }
}

/// The variables bound by reference into an element (a `for…of` variable or a destructured
/// part that borrows the matched place: `LocalKind::Elem` or `LocalKind::Bind` with a borrowing
/// binding) that a closure created in their function assigns, itself or through a closure
/// created in it: `(function, variable, closure, the closure's capture of it)`.
fn element_writes(cx: &mut Ctx) -> Vec<(DefId, LocalId, DefId, LocalId)> {
    use crate::visit::VisitMut;
    struct ByRef<'a> {
        kinds: &'a [LocalKind],
        found: HashSet<LocalId>,
        closures: Vec<DefId>,
    }
    impl VisitMut for ByRef<'_> {
        fn pat(&mut self, p: &mut Pat) {
            if let PatKind::Binding(l, UseMode::Borrow | UseMode::BorrowMut) = p.kind {
                let kind = self.kinds.get(l.0 as usize);
                if matches!(kind, Some(LocalKind::Elem | LocalKind::Bind)) {
                    self.found.insert(l);
                }
            }
        }
        fn expr(&mut self, e: &mut crate::hir::Expr) {
            if let crate::hir::ExprKind::Closure(c) = e.kind {
                self.closures.push(c);
            }
        }
    }
    let (mut memo, mut direct) = (HashMap::new(), HashMap::new());
    let mut out = vec![];
    for i in 0..cx.defs.len() {
        let p = DefId(i as u32);
        let Some(info) = cx.try_fn(p) else { continue };
        let kinds = info.local_kinds.clone();
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        let mut v = ByRef {
            kinds: &kinds,
            found: HashSet::new(),
            closures: vec![],
        };
        crate::visit::block(&mut f.body.block, &mut v);
        cx.defs[i] = Some(Def::Fn(f));
        if v.found.is_empty() {
            continue;
        }
        for c in v.closures {
            let assigned = assigned_locals(cx, c, &mut memo, &mut direct);
            if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
                out.extend(
                    cf.captures
                        .iter()
                        .filter(|cap| assigned.contains(&cap.inner) && v.found.contains(&cap.outer))
                        .map(|cap| (p, cap.outer, c, cap.inner)),
                );
            }
        }
    }
    out
}
