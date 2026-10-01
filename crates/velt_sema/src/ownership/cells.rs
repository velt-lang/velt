//! Shared cells for captured variables (semantics stage 2, docs/design/semantics-stage2.md §5).
//! An escaping closure captures its variables by value; when it or the enclosing function
//! assigns such a variable while the other still sees it (found by `crate::moves`), the variable
//! instead lives in a counted cell that the function and every closure capturing it share, so
//! `let count = 0; const inc = () => count++; inc(); console.log(count)` prints 1, as in JS.
//!
//! This pass marks those locals `LocalDef::boxed` — in the enclosing function and in each
//! closure capturing them (transitively) — and turns moves out of them into shares or copies:
//! a cell is never moved from, since a closure may still read it.

use std::collections::{HashMap, HashSet};

use crate::body::places::{place_root, set_place_mode};
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, LocalId, UseMode};
use crate::visit;

use super::soft::{is_moved_place, make_share};

/// Mark the variables of `boxed` (per function) and the captures of them as cells.
pub(crate) fn box_cells(cx: &mut Ctx, boxed: &HashMap<DefId, HashSet<LocalId>>) {
    let mut work: Vec<(DefId, LocalId)> = boxed
        .iter()
        .flat_map(|(d, ls)| ls.iter().map(move |l| (*d, *l)))
        .collect();
    let mut done = HashSet::new();
    while let Some((d, l)) = work.pop() {
        if done.insert((d, l)) {
            work.extend(box_local(cx, d, l));
        }
    }
}

/// Make local `l` of function `d` a cell; returns the capture locals of the closures created in
/// `d` that capture it.
fn box_local(cx: &mut Ctx, d: DefId, l: LocalId) -> Vec<(DefId, LocalId)> {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return vec![];
    };
    f.body.locals[l.0 as usize].boxed = true;
    let mut closures = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Closure(c) = e.kind {
            closures.push(c);
        } else if is_moved_place(e) && place_root(e) == Some(l) {
            if cx.is_copy(e.ty) {
                set_place_mode(e, UseMode::Copy);
            } else {
                make_share(e);
            }
        }
    });
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    let mut inner = vec![];
    for c in closures {
        if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
            inner.extend(
                cf.captures
                    .iter()
                    .filter(|cap| cap.outer == l)
                    .map(|cap| (c, cap.inner)),
            );
        }
    }
    inner
}
