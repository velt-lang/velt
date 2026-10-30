//! Error type parameters that only a function argument can fix: `E` of
//! `(x) => T | Promise<T, E>` (a sync-or-async callback), and `E` of a callback's `throws E`.
//! Nothing in the arguments' types fixes `E` before the function is checked against the
//! union, so a trial check of the argument finds it ([`FnCx::infer_union_error`]); one nothing
//! fixes is `never` (`args::default_slots`).

use rustc_hash::FxHashSet;
use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::{FnCx, Want};
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// A function passed for `(…) => T | Promise<T, E>` (possibly `throws E`) with `E` still
    /// unknown: the arguments' types can't fix `E` before the function is checked against the
    /// union, so a trial check against one member finds it, and is rolled back. A sync arrow
    /// is tried as `(…) => T throws E` (`E`: what it throws), a named function by its own type
    /// (what it throws, or what its promise rejects with). An async arrow needs none: checked
    /// first, it infers what it rejects with (`FnCx::async_arrow_callback`). Returns the slots
    /// it fixed: every unknown error-only one (`E` of the promise and `F` of `throws F` may be two
    /// parameters); one nothing fixes is `never`.
    pub(super) fn infer_union_error(
        &mut self,
        arg: &ast::Expr,
        pty: TyId,
        known: &[Option<TyId>],
        slots: &mut [Option<TyId>],
    ) -> Vec<usize> {
        let arrow = as_arrow(arg);
        let is_async = matches!(
            arrow.map(|a| &a.kind),
            Some(ast::ExprKind::Arrow { is_async: true, .. })
        );
        // An async arrow needs no trial: checked first, it infers what it rejects with
        // (`FnCx::async_arrow_callback`).
        if is_async || (arrow.is_none() && !matches!(arg.kind, ast::ExprKind::Ident(_))) {
            return vec![];
        }
        let fn_ty = self.cx.ty.core(pty).unwrap_or(pty);
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return vec![];
        };
        let inner = self.cx.ty.core(ret).unwrap_or(ret);
        let Some(members) = self.cx.union_members(inner) else {
            return vec![];
        };
        let (promises, values): (Vec<TyId>, Vec<TyId>) = members
            .iter()
            .partition(|m| self.cx.ty.promise_payload(**m).is_some());
        let ([promise], [value]) = (promises.as_slice(), values.as_slice()) else {
            return vec![];
        };
        let open: Vec<usize> = (0..slots.len())
            .filter(|&k| known[k].is_none() && error_only(self.cx, fn_ty, k as u32))
            .collect();
        if open.is_empty() {
            return vec![];
        }
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let (trial, found) = match arrow {
            Some(a) => {
                // Checked without a result type first: what it returns says which member it
                // fills (`(n) => work(n)` returns a promise).
                let error = self.cx.ty.error;
                let open = self.cx.ty.intern(TyKind::FnPtr {
                    params: params.clone(),
                    ret: error,
                    throws: error,
                    rest,
                });
                let expected = self.cx.subst_known(open, known);
                let found = self.closure(a, Some(expected), false).ty;
                let ret = match self.cx.ty.kind(found) {
                    TyKind::FnPtr { ret, .. } if self.cx.ty.promise_payload(*ret).is_some() => {
                        *promise
                    }
                    _ => *value,
                };
                // A promise's rejections fill its own error slot; what the arrow throws before
                // returning it fills `throws` only when that is another slot (`throws F` beside
                // `Promise<T, E>`), not the same `E`.
                let same =
                    matches!(self.cx.ty.kind(*promise), TyKind::Promise(_, e) if *e == throws);
                let throws = if ret == *promise && same {
                    self.cx.ty.never
                } else {
                    throws
                };
                let trial = self.cx.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                    rest,
                });
                (trial, found)
            }
            None => {
                let h = self.expr(arg, None, Want::Borrow);
                let (ret, throws) = match self.cx.ty.kind(h.ty).clone() {
                    TyKind::FnPtr { ret: r, .. } if self.cx.ty.promise_payload(r).is_some() => {
                        (*promise, self.cx.ty.never)
                    }
                    // A function of the same sync-or-async shape (`T | Promise<T, E2>`): the
                    // whole union is matched, which fixes both error slots.
                    TyKind::FnPtr { ret: r, .. }
                        if self
                            .cx
                            .union_members(self.cx.ty.core(r).unwrap_or(r))
                            .is_some_and(|ms| {
                                ms.iter().any(|m| self.cx.ty.promise_payload(*m).is_some())
                            }) =>
                    {
                        (ret, throws)
                    }
                    _ => (*value, throws),
                };
                let trial = self.cx.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                    rest,
                });
                (trial, h.ty)
            }
        };
        let mut fixed: Vec<Option<TyId>> = slots.to_vec();
        self.cx.match_ty(trial, found, &mut fixed);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        // Nothing the function throws or rejects with: `E` is `never`.
        for &k in &open {
            slots[k] = Some(fixed.get(k).copied().flatten().unwrap_or(self.cx.ty.never));
        }
        open
    }
}

/// Whether type parameter `k` occurs in `t`, and only as an error type (`Promise<T, E>`'s `E`,
/// a function type's `throws`).
pub(super) fn error_only(cx: &mut crate::ctx::Ctx, t: TyId, k: u32) -> bool {
    let (in_error, plain) = param_uses(cx, t, k);
    in_error && !plain
}

/// Whether type parameter `k` occurs in `t` other than as an error type.
pub(super) fn occurs_plain(cx: &mut crate::ctx::Ctx, t: TyId, k: u32) -> bool {
    param_uses(cx, t, k).1
}

/// `Some(true)` when type parameter `k` is a member of union `t` (`T | string`, `T | null`)
/// and occurs nowhere else in it, `Some(false)` when `t` doesn't mention `k`, `None` when it
/// occurs some other way.
pub(super) fn union_member_only(cx: &mut crate::ctx::Ctx, t: TyId, k: u32) -> Option<bool> {
    let is_k = |cx: &crate::ctx::Ctx, m: TyId| matches!(cx.ty.kind(m), TyKind::Param(p) if *p == k);
    let core = cx.ty.opt_payload(t).unwrap_or(t);
    let members = match cx.union_members(core) {
        Some(ms) => ms,
        None if cx.ty.opt_payload(t).is_some() => vec![core],
        None => return (!occurs_plain(cx, t, k)).then_some(false),
    };
    let mut member = false;
    for m in members {
        if is_k(cx, m) {
            member = true;
        } else if occurs_plain(cx, m, k) {
            return None;
        }
    }
    Some(member)
}

/// (occurs as an error type, occurs elsewhere) for type parameter `k` in `t`. A union is
/// looked at through its members (its type arguments don't say where `k` sits).
fn param_uses(cx: &mut crate::ctx::Ctx, t: TyId, k: u32) -> (bool, bool) {
    let mut acc = (false, false);
    param_uses_in(cx, t, k, &mut FxHashSet::default(), &mut acc);
    acc
}

/// [`param_uses`] of `t` added to `acc`. Each type is looked at once (`seen`): a recursive
/// union (`type Tree = string | Tree[]`) has itself among its members' parts, and what a type
/// holds doesn't depend on where it sits (only `k` itself does, in an error position).
fn param_uses_in(
    cx: &mut crate::ctx::Ctx,
    t: TyId,
    k: u32,
    seen: &mut FxHashSet<TyId>,
    acc: &mut (bool, bool),
) {
    if !seen.insert(t) {
        return;
    }
    let mut walk = |cx: &mut crate::ctx::Ctx, x: TyId, acc: &mut (bool, bool)| {
        param_uses_in(cx, x, k, seen, acc)
    };
    if let Some(members) = cx.union_def(t).and_then(|_| cx.union_members(t)) {
        for m in members {
            walk(cx, m, acc);
        }
        return;
    }
    // `k` as an error type (a promise's `E`, a function type's `throws`).
    let error_pos =
        |cx: &crate::ctx::Ctx, e: TyId| matches!(cx.ty.kind(e), TyKind::Param(n) if *n == k);
    match cx.ty.kind(t).clone() {
        TyKind::Param(n) => acc.1 |= n == k,
        TyKind::Promise(v, e) => {
            walk(cx, v, acc);
            match error_pos(cx, e) {
                true => acc.0 = true,
                false => walk(cx, e, acc),
            }
        }
        TyKind::FnPtr {
            params,
            ret,
            throws,
            ..
        } => {
            for p in params {
                walk(cx, p, acc);
            }
            walk(cx, ret, acc);
            match error_pos(cx, throws) {
                true => acc.0 = true,
                false => walk(cx, throws, acc),
            }
        }
        TyKind::Adt(_, xs) | TyKind::Dyn(_, xs) | TyKind::Tuple(xs) => {
            for x in xs {
                walk(cx, x, acc);
            }
        }
        TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => walk(cx, x, acc),
        TyKind::Map(a, b) | TyKind::Result(a, b) => {
            walk(cx, a, acc);
            walk(cx, b, acc);
        }
        _ => {}
    }
}
