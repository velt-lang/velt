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

use super::assign_paths::is_pure_value;
use super::setters::{side_effect_free, synth};
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId, UseMode};

pub(super) const RECEIVER: &str = "<receiver>";
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
        let later = match rmw {
            Rmw::Compound(_, v) | Rmw::Logical(_, v) => Some(v),
            Rmw::Update(..) => None,
        };
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, later, &mut stmts);
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
        // The setter's type decides: what it takes is what gets rounded (a getter of another type
        // is read as a `number`).
        let set_ty = self.setter_param_ty(recv.ty, &prop.name).unwrap_or(old_ty);
        if self.cx.ty.is_storage_float(set_ty) && !matches!(rmw, Rmw::Logical(..)) {
            let to = (old_val, set_ty);
            return self.storage_accessor_rmw(recv, prop, to, rmw, as_value, span, stmts);
        }
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
        let mut computed = None;
        let (value, cond) = match rmw {
            Rmw::Compound(op, v) => {
                let new = self.rmw_compound_value(op, (old, OLD), old_ty, v, span);
                computed = Some(new);
                (old_ref(), None)
            }
            Rmw::Update(op, _) => {
                if !self.cx.ty.is_numeric(old_ty) && !self.operator_type(old_ty) {
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
        let new_val = match computed {
            Some(new) => new,
            None => self.expr(&value, Some(old_ty), Want::Move),
        };
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

    /// `x.p op= v` or `x.p++` where `p`'s setter takes an `f16`/`bf16` (#2190), as on a storage
    /// place (`storage_rmw`): the getter's value (`old_val`) read as a `number`, the operation in
    /// f64, the result rounded once into the setter's argument (of type `set_ty`). The expression's value is the unrounded
    /// result, or for postfix `++`/`--` the old value, both `number`s, as JS gives.
    #[allow(clippy::too_many_arguments)]
    fn storage_accessor_rmw(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        (old_val, set_ty): (hir::Expr, hir::TyId),
        rmw: Rmw,
        as_value: bool,
        span: Span,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        let Some(old_n) = self.storage_operand(old_val, span) else {
            return self.error_expr(span);
        };
        let old = self.bind_temp(OLD, old_n, stmts);
        let read = |s: &mut Self, l| s.mk(H::Local(l, UseMode::Copy), f64_, span);
        let cur = read(self, old);
        let (wide, postfix) = match rmw {
            Rmw::Compound(op, v) => {
                // As on a field: a `number` hint, brands stripped, an `any` needs `as`.
                let v = self.expr(v, Some(f64_), Want::Borrow);
                let v = self.unbrand(v);
                if self.cx.dynamic(v.ty).is_some() {
                    return self.any_as_storage(set_ty, span);
                }
                let wide = self
                    .storage_operand(v, span)
                    .and_then(|v| self.storage_op(cur, op, v, span));
                (wide, false)
            }
            Rmw::Update(op, prefix) => {
                let one = self.mk(H::Lit(hir::Lit::Float(1.0)), f64_, span);
                let op = match op {
                    ast::UpdateOp::Inc => hir::BinOp::Add,
                    ast::UpdateOp::Dec => hir::BinOp::Sub,
                };
                let kind = H::Binary {
                    op,
                    lhs: Box::new(cur),
                    rhs: Box::new(one),
                };
                (Some(self.mk(kind, f64_, span)), !prefix)
            }
            Rmw::Logical(..) => unreachable!("ICE: logical assignment on the storage path"),
        };
        let Some(wide) = wide else {
            return self.error_expr(span);
        };
        let wide = self.bind_temp("<wide>", wide, stmts);
        let stored = read(self, wide);
        let stored = self.mk(H::Cast(Box::new(stored)), set_ty, span);
        self.bind_temp(NEW, stored, stmts);
        let new_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: NEW.into(),
                span,
            }),
            span,
        );
        let call = self.setter_call(recv, prop, &new_ref, span);
        stmts.push(stmt(hir::StmtKind::Expr(call), span));
        let unit = self.cx.ty.unit;
        let value = as_value.then(|| read(self, if postfix { old } else { wide }));
        let ty = if as_value { f64_ } else { unit };
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
        let new_val = match strips_null {
            true => self.value_for(v, old_ty),
            false => self.expr_coerce(v, old_ty, Want::Move),
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
        let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
        let v = match self.setter_param_ty(recv.ty, &prop.name) {
            Some(t) => self.value_for(value, t),
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

    /// `v` converted to `ty`, except that for a nullable `ty` a value that is never null keeps
    /// its non-null type (TypeScript's type of the expression; the setter call wraps it).
    fn value_for(&mut self, v: &ast::Expr, ty: hir::TyId) -> hir::Expr {
        let h = self.expr_coerce(v, ty, Want::Move);
        match h.kind {
            H::WrapSome(inner) => *inner,
            kind => hir::Expr { kind, ..h },
        }
    }

    /// The receiver to read and write through: `obj` itself when evaluating it again names the
    /// same object whatever runs in between (`this`, a `const`, or a variable that only this
    /// function's own code assigns while `later`, the value evaluated between the read and the
    /// write, assigns nothing), else a temporary holding it. A path through a getter
    /// (`h.inner.value++`) runs that getter once, as in JS; and the setter runs on the object
    /// named before the getter and `later` ran, even when one of them assigns another object
    /// to the variable, field or module `let` naming it (`holder.x.p += f()`).
    pub(super) fn receiver_once(
        &mut self,
        mut obj: hir::Expr,
        object: &ast::Expr,
        later: Option<&ast::Expr>,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let stable = is_this(object)
            || self.const_local(&obj)
            || (later.is_none_or(is_pure_value) && self.only_written_here(&obj));
        if stable && side_effect_free(object) && is_field_path(&obj) {
            return obj;
        }
        set_place_mode(&mut obj, UseMode::Move);
        let (ty, span) = (obj.ty, obj.span);
        let l = self.bind_temp(RECEIVER, obj, stmts);
        self.mk(H::Local(l, UseMode::Borrow), ty, span)
    }

    /// Is `obj` a variable that no call can assign another object to: a module `const`, or a
    /// local of this function that no closure assigns? (A field, or a module `let`, may be
    /// assigned by the getter that runs before the setter.)
    fn only_written_here(&self, obj: &hir::Expr) -> bool {
        match obj.kind {
            H::Global(d) => self.cx.global(d).is_some_and(|g| !g.mutable),
            H::Local(l, _) => {
                let kind = self.f.kinds.get(l.0 as usize);
                let name = &self.f.locals[l.0 as usize].name;
                !matches!(kind, Some(LocalKind::Capture) | None)
                    && !self.f.closure_assigned.contains_key(name)
            }
            _ => false,
        }
    }

    /// Is `obj` a `const` of this function? Nothing evaluated later can make it name another
    /// object, so it needs no temporary (one would count a reference per statement).
    fn const_local(&self, obj: &hir::Expr) -> bool {
        let H::Local(l, _) = obj.kind else {
            return false;
        };
        // A capture of an enclosing function's `const` is one too.
        let (mut frame, mut l) = (&self.f, l);
        let mut outer = self.outer.iter().rev();
        loop {
            match frame.kinds.get(l.0 as usize) {
                Some(LocalKind::Const | LocalKind::Using) => return true,
                Some(LocalKind::Capture) => {
                    let Some(c) = frame.captures.iter().find(|c| c.inner == l) else {
                        return false;
                    };
                    let Some(up) = outer.next() else {
                        return false;
                    };
                    (frame, l) = (up, c.outer);
                }
                _ => return false,
            }
        }
    }

    /// `if` condition under which `old op= v` assigns: `old == null` for `??=`, `!old` for
    /// `||=`, `old` for `&&=`.
    pub(super) fn decides(&mut self, op: ast::BinaryOp, old: ast::Expr, span: Span) -> hir::Expr {
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
    pub(super) fn bind_temp(
        &mut self,
        name: &str,
        init: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> LocalId {
        let (ty, span) = (init.ty, init.span);
        let l = self.new_local(name, ty, false, span, LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name.to_string(), l);
        let init = Some(init);
        stmts.push(stmt(hir::StmtKind::Let { local: l, init }, span));
        l
    }

    /// The value of temporary `l` as the result of the block.
    pub(super) fn read_temp(&mut self, l: LocalId, ty: hir::TyId, span: Span) -> hir::Expr {
        let mode = if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            UseMode::Move
        };
        self.mk(H::Local(l, mode), ty, span)
    }
}

/// `this` (parentheses removed), which nothing can assign another object to.
fn is_this(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::This => true,
        ast::ExprKind::Paren(inner) => is_this(inner),
        _ => false,
    }
}

/// Is `e` a variable or a path of fields of one (no getter or other call on the way)?
pub(super) fn is_field_path(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) | H::Global(_) => true,
        H::Field { base, .. } => is_field_path(base),
        _ => false,
    }
}

pub(super) fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    )
}

pub(super) fn stmt(kind: hir::StmtKind, span: Span) -> hir::Stmt {
    hir::Stmt { kind, span }
}

pub(super) fn block(stmts: Vec<hir::Stmt>, value: Option<hir::Expr>, span: Span) -> hir::Block {
    hir::Block {
        stmts,
        value: value.map(Box::new),
        span,
    }
}
