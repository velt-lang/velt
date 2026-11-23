//! An assignment to an element of an integer array (`Buffer`, `u8[]`, `i32[]`, …) used as a
//! value. In JavaScript its value is the value assigned, before the store converts it to the
//! element (element_store.rs): `(b[0] = "0x20")` is `"0x20"` while `b[0]` holds 32, and
//! `(b[0] += "7")` is what `+` gives (`"07"`). So the value is kept in a temporary, the store is
//! given it (converted when it is an `any`), and the temporary is the result:
//!
//! - `(b[i] = v)`: `{ const <new> = v; b[i] = <new>; <new> }`;
//! - `(b[i] op= v)` with an `any` or a `number` `v` (a `number` computed as one, #3182):
//!   `{ const <old> = b[i]; const <new> = <old> op v; b[i] = <new>; <new> }`, the element read
//!   before `v` is evaluated (with a `v` of another type, the compound assignment itself and
//!   the element read back, which is its value);
//! - `(b[i] ||= v)` (`&&=`, `??=`): `if (!b[i]) { const <new> = v; b[i] = <new>; <new> } else
//!   { b[i] }`, `v` evaluated only when the element doesn't decide (#3435).
//!
//! The target's indexes are free of effects here (`element_assign_value` binds them). A `v`
//! with effects may give `b` another array, so `b` is bound first (`const <object> = b`): the
//! element stored is the one the target names when it is evaluated, as in JavaScript.

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId};

use super::accessor_rmw::{block, stmt};
use super::assign::AssignTarget;
use super::assign_paths::is_pure_value;
use super::setters::synth;

/// The value assigned, what an `any` one converts to, the element read by a compound
/// assignment, and the array the element is in.
const NEW: &str = "<new>";
const CONV: &str = "<conv>";
const OLD: &str = "<old>";
const OBJECT: &str = "<object>";

fn name_ref(name: &str, span: Span) -> ast::Expr {
    let id = ast::Ident {
        name: name.into(),
        span,
    };
    synth(ast::ExprKind::Ident(id), span)
}

/// The assignment [`FnCx::element_write_value`] writes.
#[derive(Clone, Copy)]
pub(crate) enum ElementWrite {
    /// `b[i] = v`.
    Plain,
    /// `b[i] op= v`, `op` arithmetic or bitwise.
    Compound(ast::BinaryOp),
    /// `b[i] ||= v`, `&&=`, `??=`.
    Logical(ast::BinaryOp),
}

impl FnCx<'_, '_> {
    /// Assignment `w` of `value` to `target`, an element of type `elem` of an integer array and
    /// a pure place, used as a value (module docs).
    pub(crate) fn element_write_value(
        &mut self,
        w: ElementWrite,
        elem: TyId,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut outer = Vec::new();
        self.push_scope();
        let target = &self.object_bound(target, value, &mut outer);
        let e = match w {
            ElementWrite::Plain => {
                let hint = self.value_hint(elem, None, value);
                let v = self.expr(value, Some(hint), Want::Move);
                let v = self.element_typed(v, elem);
                self.kept_store(elem, target, v, &mut outer, span)
            }
            ElementWrite::Compound(op) => {
                self.element_compound_value(op, elem, target, value, &mut outer, span)
            }
            ElementWrite::Logical(op) => self.element_logical_value(op, elem, target, value, span),
        };
        self.pop_scope();
        let ty = e.ty;
        self.mk(H::Block(block(outer, Some(e), span)), ty, span)
    }

    /// `v` as it is when it is an `any`, or a `number` an element of type `elem` converts
    /// (element_store.rs, #3182), else at the element's type.
    fn element_typed(&mut self, v: hir::Expr, elem: TyId) -> hir::Expr {
        match self.cx.dynamic(v.ty) == Some(Dynamic::Any) || self.number_value(&v, elem) {
            true => v,
            false => self.coerce(v, elem),
        }
    }

    /// Is `v` a `number` that a store into an element of type `elem` converts?
    fn number_value(&self, v: &hir::Expr, elem: TyId) -> bool {
        v.ty == self.cx.ty.f64 && self.number_stores_into(elem)
    }

    /// The type `value` of `e = value` (`op`: `e op= value`) on an element of type `elem` is
    /// checked at: `number` for a literal the element's arithmetic doesn't compute as
    /// JavaScript does (`FnCx::stores_as_number`), else `elem`.
    fn value_hint(&self, elem: TyId, op: Option<ast::BinaryOp>, value: &ast::Expr) -> TyId {
        match self.stores_as_number(elem, op, value) {
            true => self.cx.ty.f64,
            false => elem,
        }
    }

    /// `const <new> = v; target = <new>` (appended to `stmts`), with the element given the
    /// number an `any` `<new>` converts to, or a `number` `<new>` (what a compound operator on
    /// an `any` gives) as a number is stored; `<new>` read is the result.
    fn kept_store(
        &mut self,
        elem: TyId,
        target: &ast::Expr,
        v: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> hir::Expr {
        let ty = v.ty;
        let new = self.bind_temp(NEW, v, stmts);
        let stored = match ty == elem {
            true => NEW,
            false => {
                let read = self.mk(H::Local(new, hir::UseMode::Borrow), ty, span);
                let converted = match self.cx.dynamic(ty) == Some(Dynamic::Any) {
                    true => self.any_element_value(read, elem),
                    false => self.number_element_value(read, elem),
                };
                self.bind_temp(CONV, converted, stmts);
                CONV
            }
        };
        let write = self.assign(None, target, &name_ref(stored, span), span);
        stmts.push(stmt(hir::StmtKind::Expr(write), span));
        self.read_temp(new, ty, span)
    }

    /// `(b[i] op= v)` (module docs).
    fn element_compound_value(
        &mut self,
        op: ast::BinaryOp,
        elem: TyId,
        target: &ast::Expr,
        value: &ast::Expr,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> hir::Expr {
        let hint = self.value_hint(elem, Some(op), value);
        let v = self.expr(value, Some(hint), Want::Borrow);
        let any = self.cx.dynamic(v.ty) == Some(Dynamic::Any);
        if !any && !self.number_value(&v, elem) {
            // The compound assignment as a statement, then the element read back.
            let Some(AssignTarget::Place(place)) = self.assign_target(target, span) else {
                return self.error_expr(span);
            };
            let write = self.place_compound_checked(place, op, v, span);
            stmts.push(stmt(hir::StmtKind::Expr(write), span));
            return self.expr(target, None, Want::Borrow);
        }
        // The element is read before `v` is evaluated: `<old>` is bound first.
        let cur = self.expr(target, None, Want::Borrow);
        let old_ty = cur.ty;
        let old = self.bind_temp(OLD, cur, stmts);
        let cur = self.mk(H::Local(old, hir::UseMode::Copy), old_ty, span);
        let r = match op {
            _ if !any => {
                // A `number` `v`: computed as a number, the element read as one (#3182).
                let f64_ = self.cx.ty.f64;
                let cur = self.mk(H::Cast(Box::new(cur)), f64_, span);
                let Some(r) = self.storage_op(cur, op, v, span) else {
                    return self.error_expr(span);
                };
                self.coerce(r, f64_)
            }
            ast::BinaryOp::Add => self.dynamic_concat(cur, v, span),
            _ => self.dynamic_arith(op, cur, v, span),
        };
        self.kept_store(elem, target, r, stmts, span)
    }

    /// `(b[i] ||= v)` (module docs).
    fn element_logical_value(
        &mut self,
        op: ast::BinaryOp,
        elem: TyId,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let cond = self.decides(op, target.clone(), span);
        let mut stmts = Vec::new();
        let v = self.expr(value, Some(elem), Want::Move);
        let v = self.element_typed(v, elem);
        let result = self.kept_store(elem, target, v, &mut stmts, span);
        let ty = result.ty;
        let then = self.mk(H::Block(block(stmts, Some(result), span)), ty, span);
        let old = self.expr(target, None, Want::Borrow);
        let old = self.coerce(old, ty);
        let els = self.mk(H::Block(block(vec![], Some(old), span)), ty, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, ty, span)
    }

    /// `target` (`b[i]`) with its array `b` bound to `<object>` (appended to `stmts`) when
    /// `value` may have effects, else as it is.
    fn object_bound(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> ast::Expr {
        let ast::ExprKind::Index {
            object,
            index,
            more,
            optional,
        } = &target.kind
        else {
            return target.clone();
        };
        if is_pure_value(value) {
            return target.clone();
        }
        let o = self.expr(object, None, Want::Move);
        self.bind_temp(OBJECT, o, stmts);
        let kind = ast::ExprKind::Index {
            object: Box::new(name_ref(OBJECT, object.span)),
            index: index.clone(),
            more: more.clone(),
            optional: *optional,
        };
        synth(kind, target.span)
    }
}
