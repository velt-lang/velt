//! Indexing a class of the standard library that defines `__index(i)`, and `__setIndex(i, v)`
//! for writes: the typed arrays (`Uint8Array`, `Float64Array`, …, std/typed_array.vlt), whose
//! elements live in an `ArrayBuffer` rather than in a Velt array, and `RegExpMatch`. `xs[i]`
//! reads `xs.__index(i)`; `xs[i] = v` is `xs.__setIndex(i, v)`, and `xs[i] op= v`, `xs[i]++`
//! and `xs[i] ??= v` read, then write, as in JS:
//!
//! ```text
//! { const <receiver> = xs;           // only when evaluating `xs` may have an effect
//!   const <index> = i;               // only when `i` is not a literal
//!   const <old> = <receiver>.__index(<index>);
//!   const <new> = <old> op v;
//!   <receiver>.__setIndex(<index>, <new>);
//!   <old> or <new> }                 // the value, where one is wanted
//! ```
//!
//! Only std classes are indexed this way: `__index` is not a protocol user classes can join.

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{binary, block, is_field_path, stmt, Rmw};
use super::assign_paths::is_pure_value;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, TyId};

/// The read method's name.
const INDEX: &str = "__index";
/// The write method's name.
const SET_INDEX: &str = "__setIndex";
/// An array's checked read (`T | undefined`), std/prelude/array.vlt.
const GET: &str = "__get";
const INDEX_TEMP: &str = "<index>";
const OLD: &str = "<old>";
const NEW: &str = "<new>";

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

fn name_ref(name: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Ident(ident(name, span)), span)
}

/// `(object, index)` of `object[index]` (parentheses removed).
fn index_parts(target: &ast::Expr) -> Option<(&ast::Expr, &ast::Expr)> {
    match &target.kind {
        ast::ExprKind::Paren(inner) => index_parts(inner),
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } => Some((object, index)),
        _ => None,
    }
}

impl FnCx<'_, '_> {
    /// Is `t` a std class that defines `__index`?
    pub(crate) fn has_indexer(&mut self, t: TyId) -> bool {
        let Some((d, _)) = self.cx.class_of(t) else {
            return false;
        };
        let std = self
            .cx
            .adt(d)
            .is_some_and(|a| a.qual_name.starts_with("std/"));
        std && self.resolve_method(t, INDEX).is_some()
    }

    /// `obj[index]` read through `__index`.
    pub(super) fn indexer_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        self.read_through(obj, index, INDEX, span)
    }

    /// `xs[i]` on an array where the read is expected to be `T | undefined` (`return xs[0]`
    /// from a function returning `T | undefined`, `const v: T | undefined = xs[i]`): a checked
    /// read, `xs.__get(i)`, which gives `undefined` for a position outside the array, as in
    /// JS (`T | undefined` at `T = U | undefined` is `U | undefined` itself). A read typed `T`
    /// stays the plain index (an out-of-range one panics); `Err` gives the object back for it.
    pub(super) fn checked_array_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(elem) = self.cx.ty.array_elem(obj.ty) else {
            return Err(obj);
        };
        let allows_undefined = |s: &Self, t: TyId| {
            matches!(
                s.cx.ty.nothing_of(t),
                Some(Nothing::Undefined | Nothing::Both)
            )
        };
        // Expected as `T | undefined` of the element type itself (not a type a generic call
        // is still inferring), or an element that may itself be `undefined`
        // (`(T | undefined)[]`), which is read this way wherever it is used.
        // `T | null | undefined` of a `(T | null)[]` too: its payload is the element type.
        let expected = exp.is_some_and(|t| match self.cx.ty.nothing_of(t) {
            Some(Nothing::Undefined) => self.cx.ty.core(t) == Some(elem),
            Some(Nothing::Both) => {
                self.cx.ty.opt_payload(t) == Some(elem) || self.cx.ty.core(t) == Some(elem)
            }
            _ => false,
        });
        let wanted = expected || allows_undefined(self, elem);
        // The standard library reads its arrays as it states (its code is not TypeScript's).
        if !wanted
            || self.cx.scopes[self.module].is_std
            || want == Want::BorrowMut
            || self.cx.ty.is_bottom(elem)
            || self
                .peek_ty(index)
                .is_some_and(|t| !self.cx.ty.is_numeric(t))
        {
            return Err(obj);
        }
        Ok(self.read_through(obj, index, GET, span))
    }

    /// `obj.<method>(index)`, the index a number evaluated once, after the object.
    fn read_through(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        method: &str,
        span: Span,
    ) -> hir::Expr {
        let prop = ident(method, span);
        if self.number_index(index) {
            return self.method_call_on(obj, &prop, &[], std::slice::from_ref(index), None, span);
        }
        // An integer or computed index: evaluated once, after the object, as a number.
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.object_once(obj, &mut stmts);
        let idx = self.index_once(index, &mut stmts);
        let call = self.method_call_on(recv, &prop, &[], &[idx], None, span);
        self.pop_scope();
        self.temps_then(stmts, call, span)
    }

    /// `object[index] = value` (`op` None) or `object[index] op= value` on an indexer class;
    /// `obj` is the checked object. `as_value`: the assignment's value is used.
    pub(super) fn indexer_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let Some((object, index)) = index_parts(target) else {
            return self.error_expr(span);
        };
        match op {
            // `xs[i] = v` as a statement: one call, its arguments evaluated in JS's order.
            None if !as_value && self.number_index(index) && is_pure_value(value) => {
                let prop = ident(SET_INDEX, span);
                let args = [index.clone(), value.clone()];
                self.method_call_on(obj, &prop, &[], &args, None, span)
            }
            // Otherwise the object, the index and the value are evaluated first, in that order
            // (`u[0] = u[1] = 7`: the value may use the object itself).
            None if !as_value => {
                let mut stmts = vec![];
                self.push_scope();
                let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
                let idx = self.index_once(index, &mut stmts);
                let v = self.expr(value, None, Want::Move);
                self.bind_temp(NEW, v, &mut stmts);
                let args = [idx, name_ref(NEW, span)];
                let call =
                    self.method_call_on(recv, &ident(SET_INDEX, span), &[], &args, None, span);
                self.pop_scope();
                self.temps_then(stmts, call, span)
            }
            None => self.indexer_assign_value(obj, object, index, value, span),
            Some(op @ (ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish)) => {
                self.indexer_rmw(obj, object, index, Rmw::Logical(op, value), as_value, span)
            }
            Some(op) => {
                self.indexer_rmw(obj, object, index, Rmw::Compound(op, value), as_value, span)
            }
        }
    }

    /// `object[index]++` / `--object[index]` on an indexer class.
    pub(super) fn indexer_update(
        &mut self,
        obj: hir::Expr,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let Some((object, index)) = index_parts(target) else {
            return self.error_expr(span);
        };
        self.indexer_rmw(obj, object, index, Rmw::Update(op, prefix), as_value, span)
    }

    /// The block of the module comment.
    fn indexer_rmw(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        index: &ast::Expr,
        rmw: Rmw,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let later = match rmw {
            Rmw::Compound(_, v) | Rmw::Logical(_, v) => Some(v),
            Rmw::Update(..) => None,
        };
        let recv = self.receiver_once(obj, object, later, &mut stmts);
        let idx = self.index_once(index, &mut stmts);
        let e = self.indexer_rmw_in(recv, idx, rmw, as_value, span, &mut stmts);
        self.pop_scope();
        e
    }

    fn indexer_rmw_in(
        &mut self,
        recv: hir::Expr,
        idx: ast::Expr,
        rmw: Rmw,
        as_value: bool,
        span: Span,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let unit = self.cx.ty.unit;
        let set_prop = ident(SET_INDEX, span);
        let old_val = self.indexer_read(recv.clone(), &idx, span);
        let old_ty = old_val.ty;
        if self.cx.ty.is_bottom(old_ty) {
            return self.error_expr(span);
        }
        let old = self.bind_temp(OLD, old_val, stmts);
        let (value, cond) = match rmw {
            Rmw::Compound(op, v) => (binary(op, name_ref(OLD, span), v.clone(), span), None),
            Rmw::Update(op, _) => {
                if !self.cx.ty.is_numeric(old_ty) {
                    let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
                    let tn = self.cx.display(old_ty);
                    self.cx
                        .err(format!("cannot apply `{opname}` to type `{tn}`"), span);
                    return self.error_expr(span);
                }
                let one = synth(
                    ast::ExprKind::Lit(ast::Lit::Int {
                        value: 1,
                        suffix: None,
                    }),
                    span,
                );
                let op = match op {
                    ast::UpdateOp::Inc => ast::BinaryOp::Add,
                    ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                };
                (binary(op, name_ref(OLD, span), one, span), None)
            }
            Rmw::Logical(_, _) if as_value => {
                self.cx.err(
                    "`??=`, `||=` and `&&=` on an element of a typed array are not supported as a value yet",
                    span,
                );
                return self.error_expr(span);
            }
            Rmw::Logical(op, v) => (v.clone(), Some(self.decides(op, name_ref(OLD, span), span))),
        };
        let mut set = vec![];
        let new_val = self.expr(&value, Some(old_ty), Want::Move);
        let new_ty = new_val.ty;
        let new = self.bind_temp(NEW, new_val, &mut set);
        let args = [idx, name_ref(NEW, span)];
        let call = self.method_call_on(recv, &set_prop, &[], &args, None, span);
        set.push(stmt(hir::StmtKind::Expr(call), span));
        match cond {
            // The write happens only when the old value does not decide the result.
            Some(cond) => {
                let then = self.mk(H::Block(block(set, None, span)), unit, span);
                let els = self.mk(H::Block(block(vec![], None, span)), unit, span);
                let skip = H::If {
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
            _ => Some(self.read_temp(new, new_ty, span)),
        };
        let ty = value.as_ref().map_or(unit, |v| v.ty);
        let b = block(std::mem::take(stmts), value, span);
        self.mk(H::Block(b), ty, span)
    }

    /// `(xs[i] = v)` used as a value on an indexer class: `v`, written through `__setIndex`
    /// after the object and index are evaluated.
    fn indexer_assign_value(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        index: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
        let idx = self.index_once(index, &mut stmts);
        let v = self.expr(value, None, Want::Move);
        let ty = v.ty;
        let new = self.bind_temp(NEW, v, &mut stmts);
        let args = [idx, name_ref(NEW, span)];
        let call = self.method_call_on(recv, &ident(SET_INDEX, span), &[], &args, None, span);
        stmts.push(stmt(hir::StmtKind::Expr(call), span));
        let value = self.read_temp(new, ty, span);
        self.pop_scope();
        self.mk(H::Block(block(stmts, Some(value), span)), ty, span)
    }

    /// `index` to evaluate once: a literal itself, else a temporary holding it, converted to a
    /// number when it is an integer (`xs[k]` with `k: usize`, as for an array index).
    fn index_once(&mut self, index: &ast::Expr, stmts: &mut Vec<hir::Stmt>) -> ast::Expr {
        if let ast::ExprKind::Lit(_) = &index.kind {
            return index.clone();
        }
        let mut i = self.expr(index, None, Want::Move);
        if self.cx.ty.is_int(i.ty) {
            let (f64_, span) = (self.cx.ty.f64, i.span);
            i = self.mk(H::Cast(Box::new(i)), f64_, span);
        }
        self.bind_temp(INDEX_TEMP, i, stmts);
        name_ref(INDEX_TEMP, index.span)
    }

    /// Is `index` passed to `__index` as it is: a literal, or a variable or field path that
    /// holds a number (an integer one is converted, and any other index is evaluated once,
    /// after the object: [`Self::index_once`])?
    fn number_index(&mut self, index: &ast::Expr) -> bool {
        if let ast::ExprKind::Lit(_) = &index.kind {
            return true;
        }
        self.peek_ty(index).is_some_and(|t| t == self.cx.ty.f64)
    }

    /// The object, evaluated once: itself when it is a variable or a path of fields, else a
    /// temporary holding it.
    fn object_once(&mut self, mut obj: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> hir::Expr {
        if is_field_path(&obj) {
            return obj;
        }
        crate::body::places::set_place_mode(&mut obj, crate::hir::UseMode::Move);
        let (ty, span) = (obj.ty, obj.span);
        let l = self.bind_temp("<receiver>", obj, stmts);
        self.mk(H::Local(l, crate::hir::UseMode::Borrow), ty, span)
    }

    /// `{ stmts; value }`, or `value` alone without statements.
    fn temps_then(&mut self, stmts: Vec<hir::Stmt>, value: hir::Expr, span: Span) -> hir::Expr {
        if stmts.is_empty() {
            return value;
        }
        let ty = value.ty;
        self.mk(H::Block(block(stmts, Some(value), span)), ty, span)
    }
}
