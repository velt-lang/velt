//! std's inlining markers, `inline()` and `noInline()` (std/lang/intrinsics/inline.vlt): the
//! checks lowering and `velt_opt` rely on.
//!
//! - A function has at most one of them: `inline()` and `noInline()` together contradict.
//! - `inline()` is not on a recursive function, directly or through others. `velt_opt` inlines a
//!   marked function at every call site, so a marked function on a cycle of direct calls could
//!   only be honoured by unrolling the cycle without end (LLVM ignores `alwaysinline` there too).
//!   The cycle is one of direct calls (`Callee::Def`), the only calls the inliner inlines: a
//!   recursion through a virtual or interface call or a function value is not one.

use rustc_hash::FxHashMap;
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic};
use crate::instantiation_cycles::Graph;
use crate::visit;

/// Reports the misplaced markers.
pub(crate) fn check(cx: &mut Ctx) {
    // Per function, the span of its `inline()` and of its `noInline()`, and its direct callees.
    let mut marked: FxHashMap<DefId, (Option<Span>, Option<Span>)> = FxHashMap::default();
    let mut calls: Vec<(DefId, DefId)> = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let me = DefId(i as u32);
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let E::Call { callee, .. } = &e.kind else {
                return;
            };
            match callee {
                Callee::Def(to, _) => calls.push((me, *to)),
                Callee::Intrinsic(Intrinsic::Inline) => {
                    marked.entry(me).or_default().0.get_or_insert(e.span);
                }
                Callee::Intrinsic(Intrinsic::NoInline) => {
                    marked.entry(me).or_default().1.get_or_insert(e.span);
                }
                _ => {}
            }
        });
    }
    if marked.is_empty() {
        return;
    }
    let mut both: Vec<Span> = (marked.values())
        .filter_map(|m| match m {
            (Some(a), Some(_)) => Some(*a),
            _ => None,
        })
        .collect();
    both.sort_by_key(|s| (s.file, s.lo));
    for span in both {
        cx.error(Diagnostic::error(
            "`inline()` and `noInline()` in one function".to_string(),
            span,
        ));
    }
    let mut always: Vec<(DefId, Span)> = (marked.iter())
        .filter_map(|(d, m)| m.0.map(|s| (*d, s)))
        .collect();
    if always.is_empty() {
        return;
    }
    always.sort_by_key(|(d, _)| d.0);
    let edges: Vec<((DefId, u32), (DefId, u32))> =
        calls.iter().map(|&(a, b)| ((a, 0), (b, 0))).collect();
    let graph = Graph::of(&edges);
    let comp = graph.components();
    let in_cycle = |d: DefId| {
        let Some(&n) = graph.index.get(&(d, 0)) else {
            return false;
        };
        calls.iter().any(|&(a, b)| {
            a == d && b == d
                || (graph.index.get(&(a, 0)) == Some(&n)
                    && graph
                        .index
                        .get(&(b, 0))
                        .is_some_and(|&m| m != n && comp[m] == comp[n]))
        })
    };
    for (d, span) in always {
        if in_cycle(d) {
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
