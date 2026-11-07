//! `f.apply(self, list)` through `any` in the program's own code. Inside a `try` with a `catch` in
//! the same function the list is read at the call, where an error can be caught (`anyApplyList`).
//! Anywhere else it is read in the slot, where an error ends the program (#1174): a read at the
//! call would give the enclosing function a `throws`, which a comparator or a `toString` can't
//! have, and a type of its own, whose call through `any` isn't inlined. A typed array is read at
//! the call in both cases (`any` reads none of its elements yet, #1540). A list known to be an
//! `any[]`, `null` or `undefined` has nothing to read and takes the plain call, with no test.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::{is_path, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyId, TyKind};
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// Must the list of `apply(args)` be read at the call?
    pub(super) fn apply_reads_list(&mut self, args: &[ast::Expr]) -> bool {
        let spread = args
            .iter()
            .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)));
        if args.len() != 2 || spread {
            return false;
        }
        // Nothing to read, and the read's `throws` would give the caller a type of its own, whose
        // call through `any` is a slot that isn't inlined (bench/any_ops/apply/expr_null.velt).
        if matches!(
            args[1].kind,
            ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
        ) {
            return false;
        }
        // No type: an error in the list, which the plain path reports.
        let Some(held) = self.trial_type(&args[1]) else {
            return false;
        };
        let list = self.any_ty();
        if matches!(self.cx.ty.kind(held), TyKind::Array(e) if Some(*e) == list) {
            return false;
        }
        let typed = velt_common::std_hook!(typed_apply_list);
        self.f.catch_depth > 0 || self.cx.prelude_fn(typed).is_some()
    }

    /// `recv.apply(args)` with the list read first.
    pub(super) fn any_apply_method(
        &mut self,
        recv: hir::Expr,
        key: hir::Expr,
        text: hir::Expr,
        args: &[ast::Expr],
        any: TyId,
        span: Span,
    ) -> hir::Expr {
        // A receiver that nothing here can change is read twice, not held: that share and
        // its drop are most of the cost of an `apply` in a function.
        let simple = |a: &ast::Expr| {
            matches!(
                a.kind,
                ast::ExprKind::Ident(_) | ast::ExprKind::This | ast::ExprKind::Lit(_)
            )
        };
        if is_path(&recv) && args.iter().all(simple) {
            let Some(mut boxed) = self.dynamic_args(args, any, false) else {
                return self.error_expr(span);
            };
            let (Some(list), Some(first)) = (boxed.pop(), boxed.pop()) else {
                return self.error_expr(span);
            };
            let mut recv = recv;
            set_place_mode(&mut recv, hir::UseMode::Borrow);
            let list = self.apply_list(recv.clone(), list, any, span);
            self.note_any_call();
            let all = vec![recv, key, text, first, list];
            return self.intrinsic(Intrinsic::AnyCall, all, any, span);
        }
        let recv = self.share_if_place(recv);
        self.with_temp("<apply>", recv, span, span, |lw, f| {
            let Some(mut boxed) = lw.dynamic_args(args, any, false) else {
                return lw.error_expr(span);
            };
            let (Some(list), Some(first)) = (boxed.pop(), boxed.pop()) else {
                return lw.error_expr(span);
            };
            let held = lw.expr(f, None, Want::Borrow);
            let list = lw.apply_list(held, list, any, span);
            lw.note_any_call();
            let recv = lw.expr(f, None, Want::Borrow);
            let all = vec![recv, key, text, first, list];
            lw.intrinsic(Intrinsic::AnyCall, all, any, span)
        })
    }

    /// The list of `f.apply(self, list)` at the call (module docs).
    fn apply_list(&mut self, f: hir::Expr, list: hir::Expr, any: TyId, span: Span) -> hir::Expr {
        let caught = self.f.catch_depth > 0;
        // A typed array's elements (`any` reads none yet), in a program that has them.
        let typed = velt_common::std_hook!(typed_apply_list);
        let list = if self.cx.prelude_fn(typed).is_some() {
            let list = if self.uses_proxy() {
                let callee = f.clone();
                self.with_temp("<list>", list, span, span, |lw, v| {
                    let held = lw.expr(v, None, Want::Borrow);
                    let inner = hooks::proxy_innermost(ty::Any::of(held)).emit(lw, any, span);
                    let held = lw.expr(v, None, Want::Move);
                    let (f, inner, held) =
                        (ty::Any::of(callee), ty::Any::of(inner), ty::Any::of(held));
                    if caught {
                        hooks::typed_proxy_list(f, inner, held).emit(lw, any, span)
                    } else {
                        hooks::typed_proxy_stop(f, inner, held).emit(lw, any, span)
                    }
                })
            } else {
                list
            };
            hooks::typed_apply_list(ty::Any::of(f.clone()), ty::Any::of(list)).emit(self, any, span)
        } else {
            list
        };
        if !caught {
            return list;
        }
        let call = if self.uses_proxy() {
            hooks::proxy_apply_list(ty::Any::of(f), ty::Any::of(list))
        } else {
            hooks::any_apply_list(ty::Any::of(f), ty::Any::of(list))
        };
        call.emit(self, any, span)
    }
}
