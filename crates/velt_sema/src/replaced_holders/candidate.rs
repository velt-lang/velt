//! An assignment as a candidate: its target lies in an object literal read as a field of
//! another value, and its right-hand side may run code.

use rustc_hash::FxHashSet;

use velt_common::Span;

use super::{
    adt_kind, contains_itself, field_keys, field_name, has_params, is_class, text, Candidate,
    Effects, Layer, Rhs, Scan, Scope,
};
use crate::ctx::Ctx;
use crate::hir::{AdtKind, Expr, ExprKind as E, TyKind};
use crate::visit;

/// The assignment `place op= value` as a [`Candidate`], when its target lies in an object
/// literal read as a field of another value and the right-hand side may run code.
pub(super) fn candidate(
    scan: &mut Scan<'_, '_>,
    span: Span,
    place: &Expr,
    value: &Expr,
) -> Option<Candidate> {
    let cx = scan.cx;
    let E::Field { base, index, .. } = &place.kind else {
        return None;
    };
    let holder = base.ty;
    let &TyKind::Adt(d, _) = cx.ty.kind(holder) else {
        return None;
    };
    if adt_kind(cx, d) != Some(AdtKind::Anon) {
        return None;
    }
    let mut path = Vec::new();
    let h = peel(cx, base, &mut path);
    let E::Field { base: parent, .. } = &h.kind else {
        return None;
    };
    // What the right-hand side runs: it names the function's constants, not its parameters
    // (their callers are not known yet).
    let mut rhs = Rhs {
        cx,
        slots: scan.slots,
        im: scan.im,
        locals: scan.locals,
        scope: Scope {
            params: FxHashSet::default(),
            known: std::mem::take(&mut scan.scope.known),
        },
        fx: Effects::default(),
    };
    visit::expr(&mut value.clone(), &mut rhs);
    let fx = rhs.fx;
    scan.scope.known = rhs.scope.known;
    if !fx.runs_code() || contains_itself(cx, d) {
        return None;
    }
    // The holders above, up to a class object (pinned by lowering).
    let mut e: &Expr = parent;
    loop {
        if is_class(cx, e.ty) {
            break;
        }
        let start = path.len();
        let inner = peel(cx, e, &mut path);
        match &inner.kind {
            E::Field { base, .. } => e = base,
            &E::Local(l, _) => {
                // Another function reaches a variable only through a capture.
                let own = !scan.params.contains(&l)
                    && scan.locals.get(l.0 as usize).is_some_and(|d| !d.boxed);
                if own {
                    for x in &mut path[start..] {
                        x.local = Some(l);
                    }
                }
                break;
            }
            _ => break,
        }
    }
    let names = scan.names;
    Some(Candidate {
        span,
        value_span: value.span,
        value: fx,
        path,
        holder_text: text(cx, names, h),
        parent_text: text(cx, names, parent),
        target_text: format!("{}.{}", text(cx, names, h), field_name(cx, holder, *index)),
    })
}

/// `e` without the narrowing around it (`o.inner!`).
pub(super) fn unnarrowed(mut e: &Expr) -> &Expr {
    while let E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } = &e.kind {
        e = x;
    }
    e
}

/// `e` without the narrowing around it (`o.inner!`), recording a layer for each type it is
/// seen as, all in the field `e` reads (if it reads one).
fn peel<'e>(cx: &Ctx, mut e: &'e Expr, path: &mut Vec<Layer>) -> &'e Expr {
    let start = path.len();
    loop {
        path.push(Layer {
            ty: e.ty,
            field: None,
            generic: has_params(cx, e.ty),
            local: None,
        });
        match &e.kind {
            E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => e = x,
            _ => break,
        }
    }
    if let E::Field { base, index, .. } = &e.kind {
        let k = (!has_params(cx, base.ty))
            .then(|| field_keys(cx, base.ty, *index, e.ty))
            .flatten();
        for l in &mut path[start..] {
            l.field = k;
        }
    }
    e
}
