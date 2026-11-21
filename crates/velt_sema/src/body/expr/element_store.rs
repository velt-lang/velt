//! A store of an `any` into an element of an integer array (`buf[i] = v` on a `Buffer`, a
//! `u8[]`, an `i32[]`): JavaScript's own store, which converts what it is given, where every
//! other conversion of an `any` to a number type is a checked type claim
//! (hir_encodings.md "any and unknown"). The value is converted with `ToNumber` (`ToPrimitive`
//! first for an object: `"7"` is 7, `true` 1, `null` 0, `[5]` 5, `"abc"` and `undefined` NaN),
//! and that number becomes the element as a number held in an `any` does: truncated, modulo
//! 2^N, NaN as 0.
//!
//! A number held, the usual case, is converted as it is, after one test of the value's tag; only
//! another value takes the conversion and a box for its result. A plain `number[]` is not
//! converted: JavaScript stores the value itself there, so the type claim stands.
//!
//! A typed array (`Uint8Array`, `Int32Array`, `Float64Array`, …: std's classes that extend
//! `TypedArray`) stores through its index setter, which takes a `number` and converts it to
//! its element itself: there the `any` is converted with `ToNumber` and given to the setter.

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, BinOp, ExprKind as H, TyId};

use super::accessor_rmw::{block, stmt};
use super::assign_paths::is_pure_value;
use super::dynamic_arith::{Side, TAG_NUMBER};
use super::setters::synth;

/// The assignment's value in [`FnCx::logical_element_value`], what an `any` one converts to, and
/// the array the element is in.
const NEW: &str = "<new>";
const CONV: &str = "<conv>";
const OBJECT: &str = "<object>";

fn name_ref(name: &str, span: Span) -> ast::Expr {
    let id = ast::Ident {
        name: name.into(),
        span,
    };
    synth(ast::ExprKind::Ident(id), span)
}

impl FnCx<'_, '_> {
    /// Is `place` an element of an array of an integer type, and `value` an `any`: a store
    /// JavaScript converts ([`Self::any_element_value`])?
    pub(crate) fn converting_store(&self, place: &hir::Expr, value: &hir::Expr) -> bool {
        let H::Index { base, .. } = &place.kind else {
            return false;
        };
        self.cx.ty.array_elem(base.ty) == Some(place.ty)
            && self.cx.ty.is_int(place.ty)
            && self.cx.dynamic(value.ty) == Some(Dynamic::Any)
    }

    /// Is `place` an element of an integer array, for `b[i] ||= v` (`&&=`, `??=`)? Such an
    /// assignment is written as `if (!b[i]) b[i] = v` (`logical_assign`): `v` is evaluated only
    /// when the element doesn't decide, as in JavaScript, and its store converts an `any`
    /// whatever expression gives it (`b[i] || v` would claim an `any` `v` as the element type
    /// inside the `||`).
    pub(crate) fn logical_element_store(&self, place: &hir::Expr) -> bool {
        let H::Index { base, .. } = &place.kind else {
            return false;
        };
        self.cx.ty.array_elem(base.ty) == Some(place.ty) && self.cx.ty.is_int(place.ty)
    }

    /// `(b[i] ||= v)` (`&&=`, `??=`) used as a value, `b[i]` an element of type `elem` of an
    /// integer array and `target` a pure place: `if (!b[i]) { const <new> = v; b[i] = <new>;
    /// <new> } else { b[i] }`. As in JavaScript, its value is `v` when the store happens: an
    /// `any` `v` as it is, not the element its store converts it to (#3435). A `v` with effects
    /// may give `b` another array, so `b` is bound first (`const <object> = b`): the element
    /// stored is the one read, as JavaScript's reference to it names it.
    pub(crate) fn logical_element_value(
        &mut self,
        op: ast::BinaryOp,
        elem: TyId,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut outer = Vec::new();
        self.push_scope();
        let target = &self.object_bound(target, value, &mut outer);
        let cond = self.decides(op, target.clone(), span);
        let mut stmts = Vec::new();
        let v = self.expr(value, Some(elem), Want::Move);
        let any = self.cx.dynamic(v.ty) == Some(Dynamic::Any);
        let v = if any { v } else { self.coerce(v, elem) };
        let ty = v.ty;
        let new = self.bind_temp(NEW, v, &mut stmts);
        // The element is given `<new>` itself, or the number an `any` `<new>` converts to,
        // which leaves `<new>` to be the value.
        let stored = match any {
            true => {
                let read = self.mk(H::Local(new, hir::UseMode::Borrow), ty, span);
                let converted = self.any_element_value(read, elem);
                self.bind_temp(CONV, converted, &mut stmts);
                CONV
            }
            false => NEW,
        };
        let write = self.assign(None, target, &name_ref(stored, span), span);
        stmts.push(stmt(hir::StmtKind::Expr(write), span));
        let result = self.read_temp(new, ty, span);
        let then = self.mk(H::Block(block(stmts, Some(result), span)), ty, span);
        let old = self.expr(target, None, Want::Borrow);
        let old = self.coerce(old, ty);
        let els = self.mk(H::Block(block(vec![], Some(old), span)), ty, span);
        self.pop_scope();
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        let choice = self.mk(kind, ty, span);
        self.mk(H::Block(block(outer, Some(choice), span)), ty, span)
    }

    /// `target` (`b[i]`, its indexes free of effects) with its array `b` bound to `<object>`
    /// (appended to `stmts`) when `value` may have effects, else as it is.
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

    /// Is `t` one of std's typed arrays (a class that extends `TypedArray`)?
    pub(crate) fn typed_array_receiver(&self, t: TyId) -> bool {
        let Some((d, _)) = self.cx.class_of(t) else {
            return false;
        };
        match self.cx.std_def("std/lang/typed_array/base::TypedArray") {
            Some(Item::Def(sup)) => self.cx.class_extends(d, sup),
            _ => false,
        }
    }

    /// The value `v` a typed array's index setter is given: an `any` as JavaScript's store
    /// converts it (module docs), anything else as it is.
    pub(crate) fn typed_array_value(&mut self, v: hir::Expr) -> hir::Expr {
        if self.cx.dynamic(v.ty) != Some(Dynamic::Any) {
            return v;
        }
        let f64_ = self.cx.ty.f64;
        self.any_element_value(v, f64_)
    }

    /// The `any` `v` as the numeric type `elem` an element has (module docs).
    pub(crate) fn any_element_value(&mut self, v: hir::Expr, elem: TyId) -> hir::Expr {
        let span = v.span;
        let any = v.ty;
        let mut stmts = Vec::new();
        let Some(side @ Side::Dyn(t, d)) = self.op_side(v, &mut stmts) else {
            return self.error_expr(span);
        };
        let is_number = self.tag_test(t, d, BinOp::Eq, TAG_NUMBER, span);
        let held = self.side_any(t, d, span);
        let as_it_is = self.coerce(held, elem);
        let number = self.op_number(side, span);
        let converted = match elem == self.cx.ty.f64 {
            true => number,
            false => {
                let boxed = self.coerce(number, any);
                self.coerce(boxed, elem)
            }
        };
        let value = self.op_choose(is_number, as_it_is, converted, elem, span);
        self.op_block(stmts, value, span)
    }
}
