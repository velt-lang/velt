//! Error type parameters that only a function argument can fix: `E` of
//! `(x) => T | Promise<T, E>` (a sync-or-async callback), and `E` of a callback's `throws E`.
//! Nothing in the arguments' types fixes `E` before the function is checked against the
//! union, so a trial check of the argument finds it ([`FnCx::infer_union_error`]); one nothing
//! fixes is `never` (`args::default_slots`).

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
    /// first, it infers what it rejects with (`FnCx::async_arrow_callback`). Returns the slot it
    /// fixed; one nothing fixes is `never`.
    pub(super) fn infer_union_error(
        &mut self,
        arg: &ast::Expr,
        pty: TyId,
        known: &[Option<TyId>],
        slots: &mut [Option<TyId>],
    ) -> Option<usize> {
        let arrow = as_arrow(arg);
        let is_async = matches!(
            arrow.map(|a| &a.kind),
            Some(ast::ExprKind::Arrow { is_async: true, .. })
        );
        // An async arrow needs no trial: checked first, it infers what it rejects with
        // (`FnCx::async_arrow_callback`).
        if is_async || (arrow.is_none() && !matches!(arg.kind, ast::ExprKind::Ident(_))) {
            return None;
        }
        // A server's handler is checked as an async arrow (`FnCx::thread_arrow`), which infers
        // its error type itself.
        if self.thread_task == Some(arg.span) || self.thread_callback == Some(arg.span) {
            return None;
        }
        let fn_ty = self.cx.ty.opt_payload(pty).unwrap_or(pty);
        let TyKind::FnPtr {
            params,
            ret,
            throws,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        let inner = self.cx.ty.opt_payload(ret).unwrap_or(ret);
        let members = self.cx.union_members(inner)?;
        let (promises, values): (Vec<TyId>, Vec<TyId>) = members
            .iter()
            .partition(|m| self.cx.ty.promise_payload(**m).is_some());
        let ([promise], [value]) = (promises.as_slice(), values.as_slice()) else {
            return None;
        };
        let k = (0..slots.len())
            .find(|&k| known[k].is_none() && error_only(self.cx, fn_ty, k as u32))?;
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
                });
                let expected = self.cx.subst_known(open, known);
                let found = self.closure(a, Some(expected), false).ty;
                let ret = match self.cx.ty.kind(found) {
                    TyKind::FnPtr { ret, .. } if self.cx.ty.promise_payload(*ret).is_some() => {
                        *promise
                    }
                    _ => *value,
                };
                let throws = if ret == *promise {
                    self.cx.ty.never
                } else {
                    throws
                };
                let trial = self.cx.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                });
                (trial, found)
            }
            None => {
                let h = self.expr(arg, None, Want::Borrow);
                let (ret, throws) = match self.cx.ty.kind(h.ty) {
                    TyKind::FnPtr { ret, .. } if self.cx.ty.promise_payload(*ret).is_some() => {
                        (*promise, self.cx.ty.never)
                    }
                    _ => (*value, throws),
                };
                let trial = self.cx.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                });
                (trial, h.ty)
            }
        };
        let mut fixed: Vec<Option<TyId>> = slots.to_vec();
        self.cx.match_ty(trial, found, &mut fixed);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        // Nothing the function throws or rejects with: `E` is `never`.
        let e = fixed.get(k).copied().flatten().unwrap_or(self.cx.ty.never);
        slots[k] = Some(e);
        Some(k)
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

/// (occurs as an error type, occurs elsewhere) for type parameter `k` in `t`. A union is
/// looked at through its members (its type arguments don't say where `k` sits).
fn param_uses(cx: &mut crate::ctx::Ctx, t: TyId, k: u32) -> (bool, bool) {
    let mut acc = (false, false);
    let mut add = |(e, p): (bool, bool)| {
        acc.0 |= e;
        acc.1 |= p;
    };
    let error_pos = |cx: &mut crate::ctx::Ctx, e: TyId| match cx.ty.kind(e) {
        TyKind::Param(n) if *n == k => (true, false),
        _ => param_uses(cx, e, k),
    };
    if let Some(members) = cx.union_def(t).and_then(|_| cx.union_members(t)) {
        for m in members {
            add(param_uses(cx, m, k));
        }
        return acc;
    }
    match cx.ty.kind(t).clone() {
        TyKind::Param(n) => add((false, n == k)),
        TyKind::Promise(v, e) => {
            add(param_uses(cx, v, k));
            add(error_pos(cx, e));
        }
        TyKind::FnPtr {
            params,
            ret,
            throws,
        } => {
            for p in params {
                add(param_uses(cx, p, k));
            }
            add(param_uses(cx, ret, k));
            add(error_pos(cx, throws));
        }
        TyKind::Adt(_, xs) | TyKind::Dyn(_, xs) | TyKind::Tuple(xs) => {
            for x in xs {
                add(param_uses(cx, x, k));
            }
        }
        TyKind::Array(x) | TyKind::Option(x) | TyKind::Shared(x) => add(param_uses(cx, x, k)),
        TyKind::Map(a, b) | TyKind::Result(a, b) => {
            add(param_uses(cx, a, k));
            add(param_uses(cx, b, k));
        }
        _ => {}
    }
    acc
}
