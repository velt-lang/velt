//! std's `@inline` (`decorators::known`, which records it on the function and checks where it
//! may be written): it is not on a recursive function, directly or through others. `velt_opt`
//! inlines a marked function at every call site, so a marked function on a cycle of direct calls
//! could only be honoured by unrolling the cycle without end (LLVM ignores `alwaysinline` there
//! too). The cycle is one of direct calls (`Callee::Def`), the only calls the inliner inlines: a
//! recursion through a virtual or interface call or a function value is not one. Only the bodies
//! a marked function reaches are walked, so a program without one pays nothing.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::decorators::Inline;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E};
use crate::visit;

/// Reports `@inline` on a recursive function.
pub(crate) fn check(cx: &mut Ctx) {
    let marked: Vec<(DefId, Span)> = (cx.fn_defs.iter())
        .filter_map(|&d| match cx.fn_info(d).inline_marker {
            Some((Inline::Always, span)) => Some((d, span)),
            _ => None,
        })
        .collect();
    if marked.is_empty() {
        return;
    }
    let mut callees: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    for (d, span) in marked {
        if reaches(cx, &mut callees, d) {
            cx.error(
                Diagnostic::error("`@inline` on a recursive function".to_string(), span)
                    .with_note(
                        "every call of a function marked `@inline` is inlined, and a recursive one would be copied into itself without end; leave the decorator out, or move the recursion into a helper it calls"
                            .to_string(),
                    ),
            );
        }
    }
}

/// Whether `from` calls itself through direct calls (a depth-first walk over the bodies it
/// reaches; `callees` remembers each body's direct callees).
fn reaches(cx: &mut Ctx, callees: &mut FxHashMap<DefId, Vec<DefId>>, from: DefId) -> bool {
    let mut seen: FxHashSet<DefId> = FxHashSet::default();
    let mut stack = vec![from];
    while let Some(d) = stack.pop() {
        let calls = callees.entry(d).or_insert_with(|| direct_callees(cx, d));
        for &c in calls.iter() {
            if c == from {
                return true;
            }
            if seen.insert(c) {
                stack.push(c);
            }
        }
    }
    false
}

/// The functions `d`'s body calls directly.
fn direct_callees(cx: &mut Ctx, d: DefId) -> Vec<DefId> {
    let mut out = vec![];
    if let Some(Some(Def::Fn(f))) = cx.defs.get_mut(d.0 as usize) {
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Def(to, _),
                ..
            } = &e.kind
            {
                out.push(*to);
            }
        });
    }
    out
}
