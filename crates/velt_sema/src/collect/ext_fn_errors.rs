//! What the function types of an `extend` target throw (#1878).
//!
//! A function type written without `throws` in an `extend` target fits a function that throws
//! anything: its error type is a type parameter of the block, one more than it declares
//! (`extend<R> (() => R)` is `extend<R, E> (() => R throws E)`; for a function type returning a
//! `Promise`, the promise's rejection type is). A method's `this` is then the receiver's own
//! type, what it throws included, so the block chosen for a function, directly or through a
//! bound, is chosen by its parameters and result, the same in sema and in lowering (velt_vir
//! lower/callee.rs, `match_ty`), and calling `this` throws what the receiver throws.
//!
//! Elsewhere such a function type is a site whose error type is inferred from the closures
//! stored into it (throws/fn_types.rs). A target is no place a value is stored, so what its
//! function types throw would be an inferred set no closure adds to: the type parameter takes
//! its place, also in a type alias the target names (`extend<R> Thunk<R>`).
//!
//! A target can't name a fixed error type (`() => R throws Error`). A type parameter (`throws
//! E`) binds to what the receiver throws, but a fixed one would make a block fit a function
//! depending on what is inferred for the function's type, which sema knows only after the
//! program is checked, and lowering only then.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::Generics;
use crate::hir::{TyId, TyKind};
use crate::resolve::TyEnv;

/// Resolving the target of one `extend` block.
struct Target {
    /// How many type parameters the block declares.
    declared: u32,
    /// The index of the next one added.
    next: u32,
    /// A fixed error type met.
    fixed: Option<TyId>,
}

/// Resolve the target `t` of an `extend` block with type parameters `generics`, adding one
/// for the error type of each function type in it written without `throws`.
pub(super) fn resolve_target(
    cx: &mut Ctx,
    t: &ast::TypeExpr,
    env: &TyEnv,
    generics: &mut Generics,
) -> TyId {
    let resolved = cx.resolve_type(t, env);
    let n = generics.len() as u32;
    let mut at = Target {
        declared: n,
        next: n,
        fixed: None,
    };
    let target = with_error_params(cx, resolved, &mut at, 0);
    for _ in n..at.next {
        let name = unused_name(generics);
        generics.push(&name);
    }
    if let Some(e) = at.fixed {
        report_fixed(cx, e, t.span);
    }
    target
}

/// `t` with the error types of its function types made type parameters (the module docs).
fn with_error_params(cx: &mut Ctx, t: TyId, at: &mut Target, depth: u32) -> TyId {
    if depth > 32 {
        return t;
    }
    let each = |cx: &mut Ctx, xs: &[TyId], at: &mut Target| -> Vec<TyId> {
        xs.iter()
            .map(|&x| with_error_params(cx, x, at, depth + 1))
            .collect()
    };
    let k = match cx.ty.kind(t).clone() {
        TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } => {
            let free = cx.ty.inferred_fns.contains(&t);
            let params = each(cx, &params, at);
            let (ret, throws) = match cx.ty.kind(ret).clone() {
                TyKind::Promise(v, e) if throws == cx.ty.never => {
                    let v = with_error_params(cx, v, at, depth + 1);
                    let e = error_param(cx, e, free, at);
                    (cx.ty.intern(TyKind::Promise(v, e)), throws)
                }
                _ => {
                    let ret = with_error_params(cx, ret, at, depth + 1);
                    (ret, error_param(cx, throws, free, at))
                }
            };
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            }
        }
        TyKind::Adt(d, xs) => TyKind::Adt(d, each(cx, &xs, at)),
        TyKind::Dyn(d, xs) => TyKind::Dyn(d, each(cx, &xs, at)),
        TyKind::Tuple(xs) => TyKind::Tuple(each(cx, &xs, at)),
        TyKind::Array(x) => TyKind::Array(with_error_params(cx, x, at, depth + 1)),
        TyKind::Shared(x) => TyKind::Shared(with_error_params(cx, x, at, depth + 1)),
        TyKind::Option(x, m) => TyKind::Option(with_error_params(cx, x, at, depth + 1), m),
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
            let ab = each(cx, &[a, b], at);
            match cx.ty.kind(t) {
                TyKind::Map(..) => TyKind::Map(ab[0], ab[1]),
                TyKind::Result(..) => TyKind::Result(ab[0], ab[1]),
                _ => TyKind::Promise(ab[0], ab[1]),
            }
        }
        _ => return t,
    };
    cx.ty.intern(k)
}

/// What a function type of the target throws: `e`, if it is a type parameter the block
/// declares, else a new one (`free`: the function type was written without `throws`).
fn error_param(cx: &mut Ctx, e: TyId, free: bool, at: &mut Target) -> TyId {
    if matches!(cx.ty.kind(e), TyKind::Param(i) if *i < at.declared) {
        return e;
    }
    let unwritten = free || e == cx.ty.never || cx.only_err_vars(e);
    if !unwritten && !cx.ty.has_error(e) {
        at.fixed.get_or_insert(e);
    }
    let p = cx.ty.param(at.next);
    at.next += 1;
    p
}

/// `E`, or else the first of `E1`, `E2`, … the block doesn't name (for messages).
fn unused_name(g: &Generics) -> String {
    let taken = |s: &str| g.names.iter().any(|n| n == s);
    (0..)
        .map(|k| match k {
            0 => "E".to_string(),
            _ => format!("E{k}"),
        })
        .find(|s| !taken(s))
        .unwrap_or_default()
}

fn report_fixed(cx: &mut Ctx, e: TyId, span: Span) {
    let en = cx.display(e);
    cx.error(
        Diagnostic::error(
            format!("a function type in an `extend` target can't throw a fixed type (`{en}`)"),
            span,
        )
        .with_note("leave `throws` out to fit a function that throws anything, or write a type parameter of the block (`extend<R, E> (() => R throws E)`)"),
    );
}
