//! The variables a closure that doesn't escape lets an escaping closure keep (#2807): an executor
//! storing a callback holds what that callback captured, as an escaping capture does.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{Capture, DefId, LocalId};

/// Per closure that doesn't escape (a `new Promise` executor, a callback a call only runs): the
/// enclosing variables it captures that an escaping closure created in it, directly or through
/// another such closure, keeps by value (#2807). Creating the outer closure lets them be held by
/// one that escapes, so a later assignment to one needs a cell, as after an escaping capture.
pub(super) fn kept_captures(
    cx: &mut Ctx,
    escaping: &FxHashSet<DefId>,
    captures: &FxHashMap<DefId, Vec<Capture>>,
) -> FxHashMap<DefId, FxHashSet<LocalId>> {
    let mut children: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    for (c, p) in crate::ownership::enclosing_fns(cx) {
        children.entry(p).or_default().push(c);
    }
    let mut memo = FxHashMap::default();
    let mut out = FxHashMap::default();
    for (&d, caps) in captures {
        if escaping.contains(&d) {
            continue;
        }
        let inner = kept_locals(d, &children, escaping, captures, &mut memo, 0);
        let outer: FxHashSet<LocalId> = (caps.iter())
            .filter(|c| inner.contains(&c.inner))
            .map(|c| c.outer)
            .collect();
        if !outer.is_empty() {
            out.insert(d, outer);
        }
    }
    out
}

/// The locals of function `d` that the escaping closures created in it keep by value, directly or
/// through the closures created in it that don't escape.
fn kept_locals(
    d: DefId,
    children: &FxHashMap<DefId, Vec<DefId>>,
    escaping: &FxHashSet<DefId>,
    captures: &FxHashMap<DefId, Vec<Capture>>,
    memo: &mut FxHashMap<DefId, FxHashSet<LocalId>>,
    depth: u32,
) -> FxHashSet<LocalId> {
    if let Some(s) = memo.get(&d) {
        return s.clone();
    }
    let mut out = FxHashSet::default();
    for &c in children.get(&d).into_iter().flatten() {
        let Some(caps) = captures.get(&c) else {
            continue;
        };
        let by_value = |m: crate::hir::PassMode| {
            matches!(m, crate::hir::PassMode::Owned | crate::hir::PassMode::Copy)
        };
        if escaping.contains(&c) {
            out.extend(
                caps.iter()
                    .filter(|cap| by_value(cap.mode))
                    .map(|cap| cap.outer),
            );
        } else if depth < 64 {
            let inner = kept_locals(c, children, escaping, captures, memo, depth + 1);
            out.extend(
                caps.iter()
                    .filter(|cap| inner.contains(&cap.inner))
                    .map(|cap| cap.outer),
            );
        }
    }
    memo.insert(d, out.clone());
    out
}
