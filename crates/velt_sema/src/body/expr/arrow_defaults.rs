//! Type parameter defaults in a generic call (`function f<T = {}>(…)`).
//!
//! - [`FnCx::default_slots`]: slots nothing inferred take their defaults once every argument is
//!   checked.
//! - [`FnCx::fix_for_context_sensitive`]: before an arrow function with an untyped parameter is
//!   checked, the type parameters in its parameter types that nothing inferred yet are fixed to
//!   their defaults, as TypeScript fixes them before checking a context-sensitive function: sigx's
//!   `component(({ signal }) => …)` types `ctx` as `ComponentSetupContext<…, {}, …>` from
//!   `component<TCombined = {}, …>`. A type parameter only in the function's result still comes
//!   from what the arrow returns.

use velt_syntax::ast;

use super::args::{as_arrow, Callable};
use super::error_slots::{error_only, occurs_plain, union_member_only};
use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Slots nothing inferred take their defaults (while the slots before them are known); an
    /// error type parameter nothing fixed (it appears only as a promise's or function's error
    /// type) is `never`.
    pub(super) fn default_slots(&mut self, c: &Callable, slots: &mut [Option<TyId>]) {
        for (k, slot) in slots.iter_mut().enumerate() {
            if slot.is_some() || c.defaults.get(k).is_some_and(|d| d.is_some()) {
                continue;
            }
            let tys: Vec<TyId> = c.params.iter().map(|p| p.ty).collect();
            let unconstrained_error = tys.iter().any(|t| error_only(self.cx, *t, k as u32))
                && !tys.iter().any(|t| occurs_plain(self.cx, *t, k as u32));
            // A type parameter only ever a member of a parameter's union (`x: T | string`)
            // that the arguments left open (`f("s")`, which the `string` member takes) holds
            // nothing here: TypeScript infers `unknown`, under which `T | string` is `unknown`;
            // `never` keeps it `string`.
            let mut member_only = || {
                let mut any_member = false;
                for t in &tys {
                    match union_member_only(self.cx, *t, k as u32) {
                        Some(m) => any_member |= m,
                        None => return false,
                    }
                }
                any_member
            };
            if unconstrained_error || member_only() {
                *slot = Some(self.cx.ty.never);
            }
        }
        for k in 0..slots.len() {
            let Some(Some(d)) = c.defaults.get(k) else {
                continue;
            };
            if slots[k].is_some() {
                continue;
            }
            let Some(before) = slots[..k].iter().copied().collect::<Option<Vec<TyId>>>() else {
                break;
            };
            slots[k] = Some(self.cx.subst(*d, &before));
        }
        // An overload signature calling its implementation: a type parameter of the
        // implementation that the signature's parameters leave open is `void` (TypeScript
        // infers `unknown`), as in `function f(x: string): string;` over
        // `function f<T>(x: string | T[]): string | T`.
        if slots.iter().any(Option::is_none) && self.signature_body().is_some() {
            for slot in slots.iter_mut().filter(|s| s.is_none()) {
                *slot = Some(self.cx.ty.unit);
            }
        }
    }

    /// Fix the slots in the parameter types of `pty`, the function type an arrow `arg` with an
    /// untyped parameter is checked against, that nothing inferred: each to its default, once
    /// the slots before it are known (in `slots` and in `known`, the types the arrow is checked
    /// with).
    pub(super) fn fix_for_context_sensitive(
        &mut self,
        c: &Callable,
        arg: &ast::Expr,
        pty: TyId,
        slots: &mut [Option<TyId>],
        known: &mut [Option<TyId>],
    ) {
        let Some(ast::ExprKind::Arrow { params, .. }) = as_arrow(arg).map(|a| &a.kind) else {
            return;
        };
        if params.iter().all(|p| p.ty.is_some()) {
            return;
        }
        let core = self.cx.ty.core(pty).unwrap_or(pty);
        let TyKind::FnPtr { params: ptys, .. } = self.cx.ty.kind(core).clone() else {
            return;
        };
        let mut used = vec![];
        for t in ptys {
            crate::types::collect_params(&self.cx.ty, t, &mut used);
        }
        for k in 0..known.len() {
            let Some(Some(d)) = c.defaults.get(k) else {
                continue;
            };
            if known[k].is_some() || !used.contains(&(k as u32)) {
                continue;
            }
            let Some(before) = known[..k].iter().copied().collect::<Option<Vec<TyId>>>() else {
                continue;
            };
            let t = self.cx.subst(*d, &before);
            known[k] = Some(t);
            slots[k] = Some(t);
        }
    }
}
