//! An `Atomic<T>` is a word in place (docs/internals/design/atomics.md): it cannot be shared,
//! which would make it a second reference to one word and give its type a count (boxing it in
//! every program). This pass, after the ownership passes (which add shares), reports each
//! share of one in a body and each closure capture that shares one, and each share of a part
//! of a `shared` value holding one (`const xs = s.items`): lowering makes that a deep copy, so
//! the atomics changed through it would be the copy's. Lowering reports the copies this pass
//! cannot see (by-reference bindings, arguments through function values: velt_vir
//! `lower/atomic.rs`) and shares made in generic code (`lower/boxing`).

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, Expr, ExprKind as E, Intrinsic, TyId};
use crate::visit;

pub(crate) fn check(cx: &mut Ctx) {
    if cx.atomic_ty().is_none() {
        return;
    }
    let mut found: Vec<(TyId, Span)> = vec![];
    let mut frozen: Vec<(TyId, Span)> = vec![];
    for d in cx.defs.iter_mut() {
        let Some(Def::Fn(f)) = d else { continue };
        for c in f.captures.iter().filter(|c| c.share) {
            let l = &f.body.locals[c.inner.0 as usize];
            found.push((l.ty, l.span));
        }
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } = &e.kind
            {
                found.push((e.ty, e.span));
                if args.first().is_some_and(in_shared) {
                    frozen.push((e.ty, e.span));
                }
            }
        });
    }
    let mut seen = rustc_hash::FxHashSet::default();
    for (t, span) in frozen {
        if cx.atomic_value(t).is_some() || !cx.holds_atomic(t) || !seen.insert(span) {
            continue;
        }
        cx.error(
            Diagnostic::error(
                "this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics would not reach the shared value",
                span,
            )
            .with_note("use it in place (`s.items[i].count.add(1)`, `for (const x of s.items)`), or pass it to a function that only reads or calls its methods"),
        );
    }
    for (t, span) in found {
        if cx.atomic_value(t).is_none() || !seen.insert(span) {
            continue;
        }
        let shown = cx.display(t);
        cx.error(
            Diagnostic::error(
                format!("an `{shown}` cannot be copied or shared: it is used where it is"),
                span,
            )
            .with_note("read it with `.load()`, or pass or keep the object that holds it"),
        );
    }
}

/// Is `e` a part of the value in a `shared` box, read in place (`Intrinsic::SharedRead`)?
fn in_shared(e: &Expr) -> bool {
    match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            ..
        } => true,
        E::Field { base, .. } | E::Index { base, .. } | E::UnwrapSome(base, _) => in_shared(base),
        _ => false,
    }
}
