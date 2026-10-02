//! Function values that outlive a call. A closure literal passed directly as an argument is
//! non-escaping: it captures by reference and its environment lives in the caller's frame
//! (docs/reference/functions.md "Captures"), so the callee's parameter must never be kept past the call.
//! - A function-typed parameter that the body stores, returns, captures in an escaping closure,
//!   copies (`.clone()`) or passes to an async call is inferred as owned (`super::infer`), and
//!   a closure literal passed to an owned parameter becomes escaping ([`escape_closure`]): it
//!   captures by value and owns a heap environment.
//! - Where a parameter cannot become owned (closures and overridden or interface methods have a
//!   fixed ABI), keeping a copy of a borrowed function value is an error
//!   ([`borrowed_fn_copy`]); moving it is already one (`super::validate`).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{
    Def, DefId, Expr, ExprKind as E, FnDef, LocalId, PassMode, TyId, TyKind, UseMode,
};

/// Is `t` a function value (`(..) => T`, or one that may be null)?
pub(crate) fn is_fn(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
        TyKind::Option(inner) => matches!(cx.ty.kind(*inner), TyKind::FnPtr { .. }),
        _ => false,
    }
}

/// A closure literal passed to a param that takes ownership outlives the call: it becomes
/// escaping, so it captures by value instead of pointing into the caller's frame. Returns
/// whether its capture modes changed.
pub(super) fn escape_closure(cx: &mut Ctx, def: DefId) -> bool {
    if cx.fn_info(def).escaping {
        return false;
    }
    cx.fn_info_mut(def).escaping = true;
    let Some(Def::Fn(mut f)) = cx.defs[def.0 as usize].take() else {
        return true;
    };
    for k in 0..f.captures.len() {
        let inner = f.captures[k].inner;
        let copy = cx.is_copy(f.body.locals[inner.0 as usize].ty);
        let mode = match f.captures[k].mode {
            PassMode::Borrow if copy => PassMode::Copy,
            PassMode::Borrow | PassMode::BorrowMut => PassMode::Owned,
            m => m,
        };
        f.captures[k].mode = mode;
        f.params[k].mode = mode;
    }
    cx.defs[def.0 as usize] = Some(Def::Fn(f));
    true
}

/// The borrowed function-typed parameters and captures of `f` (body of `d`): closures that may
/// live in a caller's frame, which `f` must not keep (see [`borrowed_fn_copy`]).
pub(super) fn borrowed_fn_locals(cx: &Ctx, d: DefId, f: &FnDef) -> HashSet<LocalId> {
    let info = cx.fn_info(d);
    (0..f.body.locals.len())
        .map(|i| LocalId(i as u32))
        .filter(|&l| {
            let e = Expr {
                kind: E::Local(l, UseMode::Copy),
                ty: f.body.locals[l.0 as usize].ty,
                span: Span::default(),
            };
            borrowed_fn_copy(cx, f, &info.local_kinds, info.keeps_fn_params, &e).is_some()
        })
        .collect()
}

/// The error for keeping a copy of `e` (an explicit `.clone()` or an async-call argument), if
/// `e` is a borrowed function-typed parameter or capture of `f`: the closure behind it may live
/// in a caller's frame.
pub(super) fn borrowed_fn_copy(
    cx: &Ctx,
    f: &FnDef,
    kinds: &[LocalKind],
    keeps_fn_params: bool,
    e: &Expr,
) -> Option<Diagnostic> {
    let E::Local(l, _) = e.kind else {
        return None;
    };
    if !is_fn(cx, e.ty) {
        return None;
    }
    if keeps_fn_params && matches!(kinds.get(l.0 as usize), Some(LocalKind::Param)) {
        return None;
    }
    let name = &f.body.locals[l.0 as usize].name;
    let borrowed = |m: PassMode| matches!(m, PassMode::Borrow | PassMode::BorrowMut);
    match kinds.get(l.0 as usize) {
        Some(LocalKind::Param) if param_mode(f, l).is_some_and(borrowed) => Some(
            Diagnostic::error(
                format!("cannot keep a copy of `{name}`, a borrowed function parameter"),
                e.span,
            )
            .with_note(format!(
                "a closure passed as `{name}` may point into its caller's variables, which are gone after the call; parameters of closures and of overridden or interface methods are always borrowed: call `{name}` here instead, or pass it to a named function that stores it (which then owns the closure)"
            )),
        ),
        Some(LocalKind::Capture) if capture_mode(f, l).is_some_and(borrowed) => Some(
            Diagnostic::error(
                format!("cannot keep a copy of captured function `{name}`"),
                e.span,
            )
            .with_note(format!(
                "`{name}` may be a borrowed closure that points into another function's variables; call it here instead, or keep the function where it is declared"
            )),
        ),
        _ => None,
    }
}

fn param_mode(f: &FnDef, l: LocalId) -> Option<PassMode> {
    f.params.iter().find(|p| p.local == l).map(|p| p.mode)
}

fn capture_mode(f: &FnDef, l: LocalId) -> Option<PassMode> {
    f.captures.iter().find(|c| c.inner == l).map(|c| c.mode)
}
