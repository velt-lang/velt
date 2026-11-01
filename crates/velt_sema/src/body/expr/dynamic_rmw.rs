//! Compound assignments and `++` / `--` of `any` places and of members of an `any` (#924):
//! `x += v`, `o.n++`, `xs[i] -= 1`. The place (or the receiver and the key) is evaluated once,
//! read, combined as `x op v` is (dynamic_arith.rs), and the result written back boxed. The
//! value of `x op= v` is the value written; of `x++` the old number, of `++x` the new one.

use velt_common::Span;
use velt_syntax::ast;

use super::dynamic_arith::DynRmw;
use super::dynamic_write::{strip, WriteKey};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, UseMode};

impl FnCx<'_, '_> {
    /// `place op= v` / `place++` on a place of type `any` (a variable, a field, an element).
    pub(super) fn dynamic_place_rmw(
        &mut self,
        place: hir::Expr,
        rmw: DynRmw<'_>,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let mut place = place;
        let mut stmts = Vec::new();
        self.hoist_indices(&mut place, &mut stmts);
        let cur = self.place_read(&place, Want::Borrow);
        let Some((new, value)) = self.dynamic_rmw_value(cur, rmw, &mut stmts, span) else {
            return self.error_expr(span);
        };
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place.clone()),
            value: Box::new(new),
        };
        let assign = self.mk(kind, unit, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(assign),
            span,
        });
        let value = match value {
            Some(v) if as_value => Some(v),
            None if as_value => Some(self.place_read(&place, Want::Borrow)),
            _ => None,
        };
        let ty = value.as_ref().map_or(unit, |v| v.ty);
        let block = hir::Block {
            stmts,
            value: value.map(Box::new),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// `place op= v` on a typed place (`n += a` with `n: number`) and an `any` `v`: `place op v`
    /// as the operator gives it on an `any` (dynamic_arith.rs), converted back to the place's
    /// type (`+` gives an `any`, the others a number). The place is read before `v` is
    /// evaluated, as in JavaScript.
    pub(super) fn number_place_any_rmw(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        v: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut place = place;
        let mut stmts = Vec::new();
        self.hoist_indices(&mut place, &mut stmts);
        let cur = self.place_read(&place, Want::Borrow);
        let r = match op {
            ast::BinaryOp::Add => self.dynamic_concat(cur, v, span),
            _ => self.dynamic_arith(op, cur, v, span),
        };
        let lty = place.ty;
        let r = match self.cx.dynamic(r.ty) {
            Some(_) => self.coerce(r, lty),
            None => self.coerce_number(r, lty),
        };
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(r),
        };
        let assign = self.mk(kind, unit, span);
        self.with_temps(stmts, assign)
    }

    /// `obj.f op= v`, `obj[k]++` and the like (`target`) on an `any` `obj`: the receiver and the
    /// key are bound first, the member read once and written once.
    pub(super) fn dynamic_member_rmw(
        &mut self,
        obj: hir::Expr,
        target: &ast::Expr,
        rmw: DynRmw<'_>,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let key = match &strip(target).kind {
            ast::ExprKind::Member { prop, .. } => {
                self.note_dynamic_write(&prop.name, prop.span);
                let ty = self.cx.ty.str_;
                Some(WriteKey::Str(self.mk(
                    H::Lit(Lit::Str(prop.name.clone())),
                    ty,
                    prop.span,
                )))
            }
            ast::ExprKind::Index { index, .. } => self.dynamic_write_key(index),
            _ => None,
        };
        let Some(key) = key else {
            if let DynRmw::Assign(_, value) = rmw {
                self.expr(value, None, Want::Borrow);
            }
            return self.error_expr(span);
        };
        let mut stmts = vec![];
        let obj = self.hoist_dynamic(obj, &mut stmts);
        let key = match key {
            WriteKey::Str(k) => WriteKey::Str(self.hoist_dynamic(k, &mut stmts)),
            WriteKey::Num(k) => WriteKey::Num(self.hoist_dynamic(k, &mut stmts)),
            WriteKey::Sym(n, k) => WriteKey::Sym(n, self.hoist_dynamic(k, &mut stmts)),
            WriteKey::Keyed(k) => WriteKey::Keyed(self.hoist_dynamic(k, &mut stmts)),
        };
        let read = self.dynamic_member_read(&obj, &key, any, span);
        let cur = self.new_local("<current>", any, false, span, LocalKind::Temp);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: cur,
                init: Some(read),
            },
            span,
        });
        let held = self.mk(H::Local(cur, UseMode::Borrow), any, span);
        let Some((new, value)) = self.dynamic_rmw_value(held, rmw, &mut stmts, span) else {
            return self.error_expr(span);
        };
        let tmp = self.new_local("<assigned>", any, false, span, LocalKind::Temp);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(new),
            },
            span,
        });
        let t = self.mk(H::Local(tmp, UseMode::Borrow), any, span);
        let share = self.intrinsic(Intrinsic::Share, vec![t], any, span);
        let set = self.dynamic_set(obj, key, share, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(set),
            span,
        });
        let value = match value {
            Some(v) if as_value => Some(v),
            None if as_value => Some(self.mk(H::Local(tmp, UseMode::Move), any, span)),
            _ => None,
        };
        let ty = value.as_ref().map_or(self.cx.ty.unit, |v| v.ty);
        let block = hir::Block {
            stmts,
            value: value.map(Box::new),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}
