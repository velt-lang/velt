//! The test of a frozen mark before a field write (`crate::freezable`, #2623): in the round
//! that knows the freezable types, `o.f = v` (and `o.f += v`, `o.f ??= v`, `o.f++`) of one
//! runs `if (IsFrozen(o)) frozenWriteError("f")`, an ordinary throw of Node's `TypeError`: after
//! the value is evaluated, as in Node (`Self::frozen_after_value`). A write of an absent optional
//! field throws Node's other message, `frozenAddError`; `Object.defineProperty`'s writes throw
//! its own (`frozenDefineError`, `frozenDefineAddError`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::assign_paths::is_plain_path;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId, UseMode};
use crate::hook_calls::{self as hooks, ty, HookTy};
use crate::symbols::Presence;

impl FnCx<'_, '_> {
    /// Before writing field `prop` of `object` (a value of type `t`): the test of its frozen
    /// mark when `t` is freezable (module docs), put before the assignment (`call_temps`), where
    /// [`Self::frozen_after_value`] takes it to put it after the value.
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
        let unit = self.cx.ty.unit;
        let error = |cx: &mut Self, add: bool| {
            let name = cx.str_lit(&prop.name, prop.span);
            let name = ty::Str::of(name);
            match (cx.defining, add) {
                (true, false) => hooks::frozen_define_error(name).emit(cx, unit, span),
                (true, true) => hooks::frozen_define_add_error(name).emit(cx, unit, span),
                (false, false) => hooks::frozen_write_error(name).emit(cx, unit, span),
                (false, true) => hooks::frozen_add_error(name).emit(cx, unit, span),
            }
        };
        // An optional field left out is added by the write: Node's "Cannot add property".
        let then = match self.cx.declared_presence(t, &prop.name) {
            Some(Presence::Optional(flag)) => {
                let o = self.expr(object, None, Want::Borrow);
                let present = self.present_test(o, &prop.name, flag, span);
                let (write, add) = (error(self, false), error(self, true));
                let kind = H::If {
                    cond: Box::new(present),
                    then: Box::new(write),
                    els: Box::new(add),
                };
                self.mk(kind, unit, span)
            }
            _ => error(self, false),
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

    /// The test [`Self::frozen_write_check`] put before a write of `at`, taken back.
    pub(super) fn take_frozen_guard(&mut self, at: Span) -> Option<hir::Stmt> {
        take_guard(&mut self.call_temps, at)
    }

    /// Assignment `h` of target `at`, the frozen test (if any) among its target's hidden
    /// statements `temps` moved after its value: the value goes into a temporary, then the test
    /// runs, then the store, so `a.x = f()` on a frozen `a` calls `f` before it throws, as in
    /// Node.
    pub(super) fn frozen_after_value(
        &mut self,
        at: Span,
        mut h: hir::Expr,
        temps: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let Some(guard) = take_guard(temps, at) else {
            return h;
        };
        let span = guard.span;
        let unit = self.unit_expr(span);
        let Some(value) = assign_value(&mut h).map(|v| std::mem::replace(v, unit)) else {
            // No store of a value (a setter): the test runs first.
            temps.push(guard);
            return h;
        };
        let (vty, vspan) = (value.ty, value.span);
        let l = self.new_local("frozen_value", vty, false, vspan, LocalKind::Temp);
        let set = hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(value),
            },
            span: vspan,
        };
        let read = self.mk(H::Local(l, UseMode::Move), vty, vspan);
        let value = self.with_lets(vec![set, guard], read);
        *assign_value(&mut h).expect("ICE: the assignment found above") = value;
        h
    }
}

/// The frozen test of a write of `at` among `stmts` (`frozen_write_check`), taken out.
fn take_guard(stmts: &mut Vec<hir::Stmt>, at: Span) -> Option<hir::Stmt> {
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
    let found = stmts.iter().rposition(|s| s.span == at && guard(s));
    found.map(|i| stmts.remove(i))
}

/// Does `e` store a value: an `Assign` or `CompoundAssign`, or a block holding one?
fn stores(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Assign { .. } | H::CompoundAssign { .. } => true,
        H::Block(b) => {
            b.stmts.iter().any(|s| match &s.kind {
                hir::StmtKind::Expr(x) => stores(x),
                _ => false,
            }) || b.value.as_deref().is_some_and(stores)
        }
        _ => false,
    }
}

/// The value of the first store [`stores`] finds in `e`.
fn assign_value(e: &mut hir::Expr) -> Option<&mut hir::Expr> {
    match &mut e.kind {
        H::Assign { value, .. } | H::CompoundAssign { value, .. } => Some(value),
        H::Block(b) => {
            let i = b.stmts.iter().position(|s| match &s.kind {
                hir::StmtKind::Expr(x) => stores(x),
                _ => false,
            });
            match i {
                Some(i) => match &mut b.stmts[i].kind {
                    hir::StmtKind::Expr(x) => assign_value(x),
                    _ => None,
                },
                None => b.value.as_deref_mut().and_then(assign_value),
            }
        }
        _ => None,
    }
}
