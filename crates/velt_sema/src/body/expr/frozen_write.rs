//! The test of a frozen mark before a field write (`crate::freezable`, #2623): in the round
//! that knows the freezable types, `o.f = v` (and `o.f += v`, `o.f ??= v`, `o.f++`) of one
//! first runs `if (IsFrozen(o)) frozenWriteError("f")`, an ordinary throw of Node's
//! `TypeError` (`Object.defineProperty`'s: `frozenDefineError`). The test runs before the value
//! is evaluated, where Node evaluates it first.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::assign_paths::is_plain_path;
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId};
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// Before writing field `prop` of `object` (a value of type `t`): the test of its frozen
    /// mark when `t` is freezable (module docs), put before the assignment (`call_temps`).
    pub(super) fn frozen_write_check(
        &mut self,
        object: &ast::Expr,
        t: TyId,
        prop: &ast::Ident,
        span: Span,
    ) {
        if !self.cx.frozen_checked(t) {
            return;
        }
        if !is_plain_path(object) {
            let shown = self.cx.display(t);
            self.cx.error(
                Diagnostic::error(
                    format!("a write to a field of a `{shown}` reached through a call, in a program that freezes `{shown}` values, is not supported yet"),
                    object.span,
                )
                .with_note("the write is checked against the object's frozen mark (`Object.freeze`): hold the object in a variable first"),
            );
            return;
        }
        // The module of the errors, which no program loads by default (`ON_DEMAND_MODULES`).
        self.cx
            .demand_std(velt_common::std_module!(lang_hooks_frozen));
        let bool_ = self.cx.ty.bool_;
        let x = self.expr(object, None, Want::Borrow);
        let cond = self.intrinsic(Intrinsic::IsFrozen, vec![x], bool_, span);
        let name = self.str_lit(&prop.name, prop.span);
        let unit = self.cx.ty.unit;
        let then = match self.defining {
            true => hooks::frozen_define_error(ty::Str::of(name)).emit(self, unit, span),
            false => hooks::frozen_write_error(ty::Str::of(name)).emit(self, unit, span),
        };
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(self.unit_expr(span)),
        };
        let guard = self.mk(kind, unit, span);
        self.call_temps.push(hir::Stmt {
            kind: hir::StmtKind::Expr(guard),
            span,
        });
    }

    /// The test [`Self::frozen_write_check`] put before a write of `at` (a logical assignment's
    /// target, `assign_checks.rs`), taken back: `true` when there was one.
    pub(super) fn take_frozen_guard(&mut self, at: Span) -> bool {
        let guard = |s: &hir::Stmt| match &s.kind {
            hir::StmtKind::Expr(e) => match &e.kind {
                H::If { cond, .. } => matches!(
                    cond.kind,
                    H::Call {
                        callee: Callee::Intrinsic(Intrinsic::IsFrozen),
                        ..
                    }
                ),
                _ => false,
            },
            _ => false,
        };
        let found = (self.call_temps.iter()).rposition(|s| s.span == at && guard(s));
        found.map(|i| self.call_temps.remove(i)).is_some()
    }
}
