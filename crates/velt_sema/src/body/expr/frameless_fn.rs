//! A named function value as the initializer of a function type outside a function body (a
//! field, module state), where no frame holds the hidden local an adapter keeps it in: wrapped
//! as `(p…) => f(p…)` calling it by name when converting it would need an adapter.

use velt_syntax::ast;

use super::callback::{Callee, Wrap};
use super::callback_wrap::Shape;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Where no frame holds a hidden local (`frameless`: module state, a field initializer): a
    /// named function value `e` (a module-level function or constant) whose type differs from
    /// the function type `exp` only in its result (`never` for another one) or in what it
    /// throws, wrapped as `(p…) => e(p…)` calling it by name and typed by `exp`, so what it
    /// throws reaches `exp`'s callers (a throwing function stored into a `() => void` field).
    /// Only a name that can't be reassigned is wrapped (a `let` would be read when called, not
    /// once). `None` for anything else.
    pub(crate) fn frameless_named_fn(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        if !self.frameless() {
            return None;
        }
        let ast::ExprKind::Ident(id) = &e.kind else {
            return None;
        };
        if self.is_local_name(&id.name) || self.cx.scopes[self.module].is_std {
            return None;
        }
        // Only a name that can't be reassigned (a function, a `const`) is wrapped: the wrapper
        // reads it when called. An unknown name is left to the read that reports it.
        let item = match self.cx.lookup_item_at(self.module, &id.name, id.span) {
            Some(crate::ctx::Item::Def(d)) => d,
            _ => return None,
        };
        let exp = exp?;
        if !matches!(self.cx.ty.kind(exp), TyKind::FnPtr { .. }) {
            return None;
        }
        let reassignable = match &self.cx.info[item.0 as usize] {
            crate::defs::DefInfo::Fn(_) => false,
            crate::defs::DefInfo::Global(g) => g.mutable,
            _ => true,
        };
        if reassignable {
            return None;
        }
        // Wrapped where converting the value would build an adapter, which needs a frame (a
        // `never` result for another one, errors the expected type allows but the value's type
        // does not say); else the value converts as it is and keeps its identity. Tried and
        // rolled back: its diagnostics are the real read's.
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let read = self.expr(e, Some(exp), Want::Borrow);
        // A `never` result for another one (not `void`) converts only through an adapter,
        // which `try_coerce` doesn't build here.
        let never_result = match (self.cx.ty.kind(read.ty), self.cx.ty.kind(exp)) {
            (
                TyKind::FnPtr {
                    params: fp,
                    ret: fr,
                    rest: fx,
                    ..
                },
                TyKind::FnPtr {
                    params: tp,
                    ret: tr,
                    rest: tx,
                    ..
                },
            ) => {
                fp == tp
                    && fx == tx
                    && *fr == self.cx.ty.never
                    && *tr != self.cx.ty.never
                    && *tr != self.cx.ty.unit
            }
            _ => false,
        };
        let adapts = never_result
            || match self.try_coerce(read, exp) {
                Ok(h) => matches!(h.kind, hir::ExprKind::Block(_) | hir::ExprKind::Closure(_)),
                Err(_) => false,
            };
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        if !adapts {
            return None;
        }
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp).clone() else {
            return None;
        };
        let shape = Shape::of(params.len(), rest);
        Some(self.wrap(
            Callee::Named(e.clone()),
            shape,
            &shape.same(),
            Wrap::Call,
            Some(exp),
        ))
    }
}
