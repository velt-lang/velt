//! Compound assignment and `++`/`--` through operator methods (design operators.md, #2050).
//!
//! `a op= b` on a class, struct, vector or type parameter evaluates its target once and then:
//! - calls `a[Symbol.opAssign](b)` on the place, in place, when the type has that method (no
//!   new object per step: `acc += x` on a matrix); other references to the object see the
//!   change, as with Python's `+=` on a list;
//! - else assigns `a = a op b`, `op` resolved as for `a op b` (`operators.rs`).
//!
//! The target must be assignable either way, so whether a type defines `opAssign` never decides
//! whether code compiles. `++a` and a statement `a++` are `a += 1`; a postfix `a++` whose value
//! is used is `<old> = a; a = <old> + 1; <old>`, never in place: an in-place method would change
//! the object `<old>` names too.

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{binary, block, stmt};
use super::setters::synth;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId, TyId, UseMode};
use velt_syntax::operators;

/// The temporary holding the target's value for `a = a op b`.
const OLD: &str = "<old>";
/// The temporary holding a value that names the target's variable (`v += v`).
const VALUE: &str = "<value>";

/// The variable `e` reads through (`this` for `this.a.b`), if it is a path.
fn root_name(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ast::ExprKind::Ident(x) => Some(&x.name),
        ast::ExprKind::This => Some("this"),
        ast::ExprKind::Paren(inner) => root_name(inner),
        ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } => {
            root_name(object)
        }
        _ => None,
    }
}

fn name_ref(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

/// `1`, what `++` adds.
fn one(span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Lit(ast::Lit::Int {
            value: 1,
            suffix: None,
        }),
        span,
    )
}

impl FnCx<'_, '_> {
    /// `place op= value` on a type with operator methods; `Err` gives the place back for the
    /// built-in compound assignment.
    pub(super) fn place_compound_operator(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        (target, value): (&ast::Expr, &ast::Expr),
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.operator_type(place.ty) || operators::binary_method(op).is_none() {
            return Err(place);
        }
        // The place's indices are evaluated once, first (`arr[f(0)] += arr[f(1)]` calls `f(0)`
        // first).
        let mut place = place;
        let mut stmts = vec![];
        self.hoist_indices(&mut place, &mut stmts);
        // `v += v`, `m.a += m.a.b`: the value names the target's variable. It is passed as a
        // share of the same object (the method sees `other` change as it writes `this`), which
        // keeps the exclusive-access rule from refusing the call.
        if root_name(target).is_some() && root_name(target) == root_name(value) {
            self.push_scope();
            let v = self.expr(value, None, Want::Borrow);
            let v = self.share_if_place(v);
            self.bind_temp(VALUE, v, &mut stmts);
            let e = self.place_compound(place, op, &name_ref(VALUE, span), true, span);
            self.pop_scope();
            return Ok(self.with_temps_value(stmts, e));
        }
        let e = self.place_compound(place, op, value, true, span);
        Ok(self.with_temps(stmts, e))
    }

    /// `place op= value` with the place's indices already hoisted: in place when `in_place`
    /// allows it and the type has `[Symbol.opAssign]`, else `place = place op value`.
    fn place_compound(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        value: &ast::Expr,
        in_place: bool,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        let assign = operators::assign_method(op).expect("ICE: a binary operator");
        if in_place && self.has_instance_op(place.ty, assign) {
            let prop = ast::Ident {
                name: assign.into(),
                span,
            };
            let call =
                self.method_call_on(place, &prop, &[], std::slice::from_ref(value), None, span);
            return self.with_temps(stmts, call);
        }
        let lty = place.ty;
        self.push_scope();
        let cur = self.place_read(&place, Want::Borrow);
        let cur = self.share_if_place(cur);
        self.bind_temp(OLD, cur, &mut stmts);
        let new = self.expr(
            &binary(op, name_ref(OLD, span), value.clone(), span),
            Some(lty),
            Want::Move,
        );
        self.pop_scope();
        let new = self.coerce(new, lty);
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(new),
        };
        let assign = self.mk(kind, unit, span);
        self.with_temps(stmts, assign)
    }

    /// `++place` / `place--` on a type with operator methods (`as_value`: its value is used);
    /// `Err` gives the place back for the built-in update.
    pub(super) fn place_update_operator(
        &mut self,
        mut place: hir::Expr,
        op: ast::UpdateOp,
        prefix: bool,
        as_value: bool,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.operator_type(place.ty) {
            return Err(place);
        }
        let bop = match op {
            ast::UpdateOp::Inc => ast::BinaryOp::Add,
            ast::UpdateOp::Dec => ast::BinaryOp::Sub,
        };
        let lty = place.ty;
        let mut stmts = vec![];
        self.hoist_indices(&mut place, &mut stmts);
        if !as_value {
            let e = self.place_compound(place, bop, &one(span), true, span);
            return Ok(self.with_temps(stmts, e));
        }
        if prefix {
            // `{ place += 1; place }`: the value read after the update (a new object when the
            // type has no `[Symbol.addAssign]`).
            let after = place.clone();
            let e = self.place_compound(place, bop, &one(span), true, span);
            stmts.push(stmt(hir::StmtKind::Expr(e), span));
            let read = self.place_read(&after, Want::Borrow);
            let read = self.share_if_place(read);
            let b = block(stmts, Some(read), span);
            return Ok(self.mk(H::Block(b), lty, span));
        }
        // `{ const <postfix> = place; place = <postfix> + 1; <postfix> }`.
        let cur = self.place_read(&place, Want::Borrow);
        let cur = self.share_if_place(cur);
        let old = self.new_local("<postfix>", lty, false, span, LocalKind::Temp);
        stmts.push(stmt(
            hir::StmtKind::Let {
                local: old,
                init: Some(cur),
            },
            span,
        ));
        let e = self.place_compound(place, bop, &one(span), false, span);
        stmts.push(stmt(hir::StmtKind::Expr(e), span));
        let value = self.read_temp(old, lty, span);
        let b = block(stmts, Some(value), span);
        Ok(self.mk(H::Block(b), lty, span))
    }

    /// The new value of `<old> op= v` in a read-then-write (an accessor's or an indexer's,
    /// `old` the temporary holding the value read): `{ <old>[Symbol.opAssign](v); <old> }` when
    /// its type has that method, else `<old> op v`.
    pub(super) fn rmw_compound_value(
        &mut self,
        op: ast::BinaryOp,
        (old, old_name): (LocalId, &str),
        old_ty: TyId,
        v: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let assign = operators::assign_method(op);
        if let Some(assign) = assign.filter(|a| self.has_instance_op(old_ty, a)) {
            let prop = ast::Ident {
                name: assign.into(),
                span,
            };
            let recv = self.mk(H::Local(old, UseMode::Borrow), old_ty, span);
            let call = self.method_call_on(recv, &prop, &[], std::slice::from_ref(v), None, span);
            let value = self.read_temp(old, old_ty, span);
            let b = block(
                vec![stmt(hir::StmtKind::Expr(call), span)],
                Some(value),
                span,
            );
            return self.mk(H::Block(b), old_ty, span);
        }
        let e = binary(op, name_ref(old_name, span), v.clone(), span);
        self.expr(&e, Some(old_ty), Want::Move)
    }
}
