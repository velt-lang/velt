//! std's inlining markers, `inline()` and `noInline()` (std/lang/intrinsics/inline.velt): the
//! checks lowering and `velt_opt` rely on.
//!
//! - A marker is the first statement of a declared function's or method's body, and not of a
//!   generator's or an async function's: lowering compiles those into a resume function the
//!   marker would not reach, and a marker anywhere else (after other statements, in a branch
//!   that may not run, in a closure) would say something about the function it does not mean.
//!   So a function has at most one: a second marker, of either kind, is not the first statement.
//!   Checked as the body is (`FnCx::inline_marker`), which records the marker on the function.
//! - `inline()` is not on a recursive function, directly or through others. `velt_opt` inlines a
//!   marked function at every call site, so a marked function on a cycle of direct calls could
//!   only be honoured by unrolling the cycle without end (LLVM ignores `alwaysinline` there too).
//!   The cycle is one of direct calls (`Callee::Def`), the only calls the inliner inlines: a
//!   recursion through a virtual or interface call or a function value is not one. Only the
//!   bodies a marked function reaches are walked, so a program without one pays nothing.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use velt_syntax::ast;

use super::FnCx;
use crate::ctx::Ctx;
use crate::defs::FnSource;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic};
use crate::visit;

impl FnCx<'_, '_> {
    /// Records the marker `i` (at `span`) on the function whose body is checked: an error unless
    /// it is the first statement of a declared function or method that is not a generator or
    /// async.
    pub(super) fn inline_marker(&mut self, i: Intrinsic, span: Span) {
        let first = (self.body_def)
            .filter(|_| self.outer.is_empty() && !self.f.is_async && self.f.yield_ty.is_none())
            .filter(|&d| self.first_statement_is(d, span));
        let Some(d) = first else {
            let name = if i == Intrinsic::Inline {
                "inline"
            } else {
                "noInline"
            };
            self.cx.err(
                format!("`{name}()` must be the first statement of a function or method (not of a closure, a generator or an async function)"),
                span,
            );
            return;
        };
        self.cx.fn_info_mut(d).inline_marker = Some((i, span));
    }
}

impl FnCx<'_, '_> {
    /// Whether the first statement of `d`'s body is the call at `span`.
    fn first_statement_is(&self, d: DefId, span: Span) -> bool {
        let body = match &self.cx.fn_info(d).source {
            Some(FnSource::Decl(decl)) => &decl.body,
            Some(FnSource::Default(_, body)) => *body,
            None => return false,
        };
        match body.stmts.first().map(|s| &s.kind) {
            Some(ast::StmtKind::Expr(e)) => e.span == span,
            _ => false,
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
