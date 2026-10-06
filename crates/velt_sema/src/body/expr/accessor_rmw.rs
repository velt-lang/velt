//! Read-modify-write of an accessor: `x.p op= v`, `x.p++` / `--x.p` and `x.p ??= v` (also
//! `||=`, `&&=`) where `p` has a getter and a setter. As in JS, the receiver is evaluated once,
//! then the getter runs to completion, then the right-hand side, then the setter:
//!
//! ```text
//! { const <receiver> = x;            // only when evaluating `x` may have an effect
//!   const <old> = <receiver>.p;      // getter
//!   const <new> = <old> op v;
//!   <receiver>.p = <new>;            // setter
//!   <old> or <new> }                 // the value, where one is wanted
//! ```
//!
//! The calls are sequential uses of the receiver, so a getter that changes its object (a signal
//! recording a subscriber) never overlaps the setter's use of it. `??=`, `||=` and `&&=` call the
//! setter (and evaluate `v`) only when the old value does not decide the result. The temporaries
//! are bound under names no source identifier can have, so the synthesized expressions that read
//! them are checked like any other code.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::{side_effect_free, synth};
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId, UseMode};

const RECEIVER: &str = "<receiver>";
const OLD: &str = "<old>";
const NEW: &str = "<new>";

/// A logical assignment used as a value: the operator, when the setter runs, and the old value.
struct LogicalValue {
    op: ast::BinaryOp,
    cond: hir::Expr,
    old: LocalId,
    old_ty: hir::TyId,
}

/// What the new value is computed from the old one with.
pub(super) enum Rmw<'a> {
    /// `x.p op= v` (`op` not a logical operator).
    Compound(ast::BinaryOp, &'a ast::Expr),
    /// `x.p ??= v`, `x.p ||= v`, `x.p &&= v`.
    Logical(ast::BinaryOp, &'a ast::Expr),
    /// `x.p++` / `--x.p`; `prefix`: the value is the new one.
    Update(ast::UpdateOp, bool),
}

impl FnCx<'_, '_> {
    /// `object.prop` (`target`) updated per `rmw` through its getter and setter; `obj` is the
    /// checked receiver. `as_value`: the result is used.
    pub(super) fn accessor_rmw(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        prop: &ast::Ident,
        rmw: Rmw,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, &mut stmts);
        let e = self.accessor_rmw_in(recv, prop, rmw, as_value, span, &mut stmts);
        self.pop_scope();
        e
    }

    fn accessor_rmw_in(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        rmw: Rmw,
        as_value: bool,
        span: Span,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let old_val = match self.getter_read(recv.clone(), prop, span) {
            Ok(read) => read,
            Err(_) => return self.error_expr(span),
        };
        let old_ty = old_val.ty;
        let old = self.bind_temp(OLD, old_val, stmts);
        let at = |kind| synth(kind, span);
        let old_ref = || {
            at(ast::ExprKind::Ident(ast::Ident {
                name: OLD.into(),
                span,
            }))
        };
        let new_ref = at(ast::ExprKind::Ident(ast::Ident {
            name: NEW.into(),
            span,
        }));
        let (value, cond) = match rmw {
            Rmw::Compound(op, v) => (binary(op, old_ref(), v.clone(), span), None),
            Rmw::Update(op, _) => {
                if !self.cx.ty.is_numeric(old_ty) {
                    let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
                    let tn = self.cx.display(old_ty);
                    self.cx
                        .err(format!("cannot apply `{opname}` to type `{tn}`"), span);
                    return self.error_expr(span);
                }
                let one = at(ast::ExprKind::Lit(ast::Lit::Int {
                    value: 1,
                    suffix: None,
                }));
                let op = match op {
                    ast::UpdateOp::Inc => ast::BinaryOp::Add,
                    ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                };
                (binary(op, old_ref(), one, span), None)
            }
            Rmw::Logical(op @ (ast::BinaryOp::Or | ast::BinaryOp::And), _)
                if self.reject_logical_assign(op, old_ty, span) =>
            {
                return self.error_expr(span);
            }
            Rmw::Logical(op, v) if as_value => {
                let cond = self.decides(op, old_ref(), span);
                let lv = LogicalValue {
                    op,
                    cond,
                    old,
                    old_ty,
                };
                return self.logical_value(recv, prop, v, lv, span, stmts);
            }
            Rmw::Logical(op, v) => (v.clone(), Some(self.decides(op, old_ref(), span))),
        };
        let mut set = vec![];
        let new_val = self.expr(&value, Some(old_ty), Want::Move);
        let new = self.bind_temp(NEW, new_val, &mut set);
        let call = self.setter_call(recv, prop, &new_ref, span);
        set.push(stmt(hir::StmtKind::Expr(call), span));
        let unit = self.cx.ty.unit;
        match cond {
            // The setter runs only when the old value does not decide the result.
            Some(cond) => {
                let then = self.mk(H::Block(block(set, None, span)), unit, span);
                let els = self.mk(H::Block(block(vec![], None, span)), unit, span);
                let skip = hir::ExprKind::If {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                };
                let skip = self.mk(skip, unit, span);
                stmts.push(stmt(hir::StmtKind::Expr(skip), span));
            }
            None => stmts.extend(set),
        }
        let value = match rmw {
            _ if !as_value => None,
            Rmw::Update(_, false) => Some(self.read_temp(old, old_ty, span)),
            _ => Some(self.read_temp(new, old_ty, span)),
        };
        let ty = value.as_ref().map_or(unit, |v| v.ty);
        let b = block(std::mem::take(stmts), value, span);
        self.mk(H::Block(b), ty, span)
    }

    /// `(x.p ??= v)` (also `||=`, `&&=`) used as a value: `if (<cond>) { <new> = v; x.p = <new>;
    /// <new> } else { <old> }`. For `??=` and `||=` on a nullable accessor the old value is
    /// non-null where it decides, so the value is non-null when `v` is (TypeScript's type).
    fn logical_value(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        v: &ast::Expr,
        lv: LogicalValue,
        span: Span,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let LogicalValue {
            op,
            cond,
            old,
            old_ty,
        } = lv;
        let inner = self.cx.ty.opt_payload(old_ty);
        let strips_null = inner.is_some() && op != ast::BinaryOp::And;
        let mut set = vec![];
        let new_val = match inner.filter(|_| strips_null) {
            Some(t) => {
                let h = self.expr(v, Some(t), Want::Move);
                self.try_coerce(h, t)
                    .unwrap_or_else(|h| self.coerce(h, old_ty))
            }
            None => self.expr_coerce(v, old_ty, Want::Move),
        };
        let ty = new_val.ty;
        let new = self.bind_temp(NEW, new_val, &mut set);
        let new_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: NEW.into(),
                span,
            }),
            span,
        );
        let call = self.setter_call(recv, prop, &new_ref, span);
        set.push(stmt(hir::StmtKind::Expr(call), span));
        let then_val = self.read_temp(new, ty, span);
        let then = self.mk(H::Block(block(set, Some(then_val), span)), ty, span);
        // Where the old value decides, it is the result (non-null when `ty` is).
        let old_val = if ty == old_ty {
            self.read_temp(old, old_ty, span)
        } else {
            let base = self.mk(H::Local(old, UseMode::Borrow), old_ty, span);
            let mode = self.use_mode(ty, Want::Move);
            self.mk(H::UnwrapSome(Box::new(base), mode), ty, span)
        };
        let els = self.mk(H::Block(block(vec![], Some(old_val), span)), ty, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        let pick = self.mk(kind, ty, span);
        let b = block(std::mem::take(stmts), Some(pick), span);
        self.mk(H::Block(b), ty, span)
    }

    /// `(x.p = v)` used as a value, `p` a setter: `v` converted to the setter's parameter type,
    /// as in JavaScript (the getter is not read again). The receiver is evaluated first.
    pub(super) fn accessor_assign_value(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        prop: &ast::Ident,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, &mut stmts);
        let v = match self.setter_param_ty(recv.ty, &prop.name) {
            Some(t) => self.expr_coerce(value, t, Want::Move),
            None => self.expr(value, None, Want::Move),
        };
        let ty = v.ty;
        let new = self.bind_temp(NEW, v, &mut stmts);
        let new_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: NEW.into(),
                span,
            }),
            span,
        );
        let call = self.setter_call(recv, prop, &new_ref, span);
        stmts.push(stmt(hir::StmtKind::Expr(call), span));
        let value = self.read_temp(new, ty, span);
        self.pop_scope();
        self.mk(H::Block(block(stmts, Some(value), span)), ty, span)
    }

    /// The receiver to read and write through: `obj` itself when evaluating it again has no
    /// effect (a variable, `this`, a path of fields), else a temporary holding it. A path through
    /// a getter (`h.inner.value++`) runs that getter once, as in JS.
    fn receiver_once(
        &mut self,
        mut obj: hir::Expr,
        object: &ast::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        if side_effect_free(object) && is_field_path(&obj) {
            return obj;
        }
        set_place_mode(&mut obj, UseMode::Move);
        let (ty, span) = (obj.ty, obj.span);
        let l = self.bind_temp(RECEIVER, obj, stmts);
        self.mk(H::Local(l, UseMode::Borrow), ty, span)
    }

    /// `if` condition under which `old op= v` assigns: `old == null` for `??=`, `!old` for
    /// `||=`, `old` for `&&=`.
    fn decides(&mut self, op: ast::BinaryOp, old: ast::Expr, span: Span) -> hir::Expr {
        let test = match op {
            ast::BinaryOp::Nullish => {
                let null = synth(ast::ExprKind::Lit(ast::Lit::Null), span);
                binary(ast::BinaryOp::Eq, old, null, span)
            }
            ast::BinaryOp::Or => synth(
                ast::ExprKind::Unary {
                    op: ast::UnaryOp::Not,
                    expr: Box::new(old),
                },
                span,
            ),
            _ => old,
        };
        self.cond(&test)
    }

    /// `const <name> = init` (appended to `stmts`), visible by `name` in the innermost scope.
    fn bind_temp(&mut self, name: &str, init: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> LocalId {
        let (ty, span) = (init.ty, init.span);
        let l = self.new_local(name, ty, false, span, LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name.to_string(), l);
        let init = Some(init);
        stmts.push(stmt(hir::StmtKind::Let { local: l, init }, span));
        l
    }

    /// The value of temporary `l` as the result of the block.
    fn read_temp(&mut self, l: LocalId, ty: hir::TyId, span: Span) -> hir::Expr {
        let mode = if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            UseMode::Move
        };
        self.mk(H::Local(l, mode), ty, span)
    }
}

/// Is `e` a variable or a path of fields of one (no getter or other call on the way)?
fn is_field_path(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) | H::Global(_) => true,
        H::Field { base, .. } => is_field_path(base),
        _ => false,
    }
}

fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    )
}

fn stmt(kind: hir::StmtKind, span: Span) -> hir::Stmt {
    hir::Stmt { kind, span }
}

fn block(stmts: Vec<hir::Stmt>, value: Option<hir::Expr>, span: Span) -> hir::Block {
    hir::Block {
        stmts,
        value: value.map(Box::new),
        span,
    }
}
