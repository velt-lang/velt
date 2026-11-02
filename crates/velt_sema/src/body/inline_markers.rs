//! std's inlining markers, `inline()` and `noInline()` (std/lang/intrinsics/inline.vlt): the
//! checks lowering and `velt_opt` rely on.
//!
//! - A function has at most one of them: `inline()` and `noInline()` together contradict.
//!   Checked as the body is (`FnCx::inline_marker`), which records the marker on the function.
//! - `inline()` is not on a recursive function, directly or through others. `velt_opt` inlines a
//!   marked function at every call site, so a marked function on a cycle of direct calls could
//!   only be honoured by unrolling the cycle without end (LLVM ignores `alwaysinline` there too).
//!   The cycle is one of direct calls (`Callee::Def`), the only calls the inliner inlines: a
//!   recursion through a virtual or interface call or a function value is not one. Only the
//!   bodies a marked function reaches are walked, so a program without one pays nothing.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use super::FnCx;
use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic};
use crate::visit;

impl FnCx<'_, '_> {
    /// Records the marker `i` (at `span`) on the function whose body is checked: an error when it
    /// already has the other one.
    pub(super) fn inline_marker(&mut self, i: Intrinsic, span: Span) {
        let Some(d) = self.body_def.filter(|_| self.outer.is_empty()) else {
            return;
        };
        match self.cx.fn_info(d).inline_marker {
            Some((had, _)) if had != i => self.cx.err(
                "`inline()` and `noInline()` in one function".to_string(),
                span,
            ),
            Some(_) => {}
            None => self.cx.fn_info_mut(d).inline_marker = Some((i, span)),
        }
    }
}

/// Reports `inline()` on a recursive function.
pub(crate) fn check(cx: &mut Ctx) {
    let marked: Vec<(DefId, Span)> = (cx.fn_defs.iter())
        .filter_map(|&d| match cx.fn_info(d).inline_marker {
            Some((Intrinsic::Inline, span)) => Some((d, span)),
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
                Diagnostic::error("`inline()` on a recursive function".to_string(), span)
                    .with_note(
                        "every call of a function marked `inline()` is inlined, and a recursive one would be copied into itself without end; leave the marker out, or move the recursion into a helper it calls"
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
