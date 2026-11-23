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

use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, BinOp, ExprKind as H, IntTy, TyId, TyKind};
use velt_common::Span;
use velt_syntax::ast;

use super::dynamic_arith::{Side, TAG_NUMBER};

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

    /// Is `place` an element of an array of `u8`, `i8`, `u16`, `i16`, `u32` or `i32`, and `value`
    /// a `number` (#3182)? A JavaScript typed array (`Buffer`/`Uint8Array`, `Int8Array`, …)
    /// converts a number it stores ([`Self::number_store_value`]); a value already of the
    /// element's type is stored as it is, and every other conversion of a `number` to an integer
    /// type is still `as` (which saturates). A 64-bit element is not one: `BigInt64Array` takes
    /// a `bigint`.
    pub(crate) fn number_store(&self, place: &hir::Expr, value: &hir::Expr) -> bool {
        let H::Index { base, .. } = &place.kind else {
            return false;
        };
        value.ty == self.cx.ty.f64
            && self.cx.ty.array_elem(base.ty) == Some(place.ty)
            && store_hook(self.cx.ty.kind(place.ty)).is_some()
    }

    /// The value `v` of a store into an integer array's element of type `elem`: an `any`
    /// ([`Self::converting_store`]) or a `number` ([`Self::number_store`]), converted as
    /// JavaScript's store converts it.
    pub(crate) fn element_value(&mut self, v: hir::Expr, elem: TyId) -> hir::Expr {
        match v.ty == self.cx.ty.f64 {
            true => self.number_store_value(v, elem),
            false => self.any_element_value(v, elem),
        }
    }

    /// `place op= v` on an integer array's element with a `number` `v` ([`Self::number_store`]):
    /// computed as a number, as JavaScript does (the element read as one), and stored as a typed
    /// array stores it (#3182). The index is evaluated once, before `v`.
    pub(crate) fn number_element_rmw(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        v: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let (mut place, mut stmts) = (place, Vec::new());
        self.hoist_indices(&mut place, &mut stmts);
        let (f64_, elem, unit) = (self.cx.ty.f64, place.ty, self.cx.ty.unit);
        let cur = self.place_read(&place, Want::Borrow);
        let cur = self.mk(H::Cast(Box::new(cur)), f64_, span);
        let Some(wide) = self.storage_op(cur, op, v, span) else {
            return self.error_expr(span);
        };
        let wide = self.coerce(wide, f64_);
        let stored = self.number_store_value(wide, elem);
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(stored),
        };
        let assign = self.mk(kind, unit, span);
        self.with_temps(stmts, assign)
    }

    /// The `number` `v` as the element type `elem` (one of [`Self::number_store`]'s): a call of
    /// std's conversion of that typed array (std/lang/hooks/int_store.velt, demanded here), or
    /// `v` coerced, which reports the mismatch, without the prelude.
    pub(crate) fn number_store_value(&mut self, v: hir::Expr, elem: TyId) -> hir::Expr {
        let Some(name) = store_hook(self.cx.ty.kind(elem)) else {
            return self.coerce(v, elem);
        };
        if let Some((module, _)) = name.split_once("::") {
            self.cx.demand_std(module);
        }
        let Some(Item::Def(d)) = self.cx.std_def(name) else {
            return self.coerce(v, elem);
        };
        let span = v.span;
        let kind = H::Call {
            callee: hir::Callee::Def(d, vec![]),
            args: vec![v],
        };
        self.mk(kind, elem, span)
    }

    /// The `number` `v` (what `-`, `*`, `|`… give on an `any`) as the integer type `elem` of an
    /// integer array's element, as JavaScript's store converts it: as a number held in an `any`
    /// converts (truncated, modulo 2^N, NaN as 0; `b[0] -= "1"` on 0 stores 255).
    pub(crate) fn number_element_value(&mut self, v: hir::Expr, elem: TyId) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(v.span);
        };
        let boxed = self.coerce(v, any);
        self.coerce(boxed, elem)
    }
}

/// The hook that converts a `number` for an element of type `k` (std/lang/hooks/int_store.velt),
/// for the integer types a JavaScript typed array has but the 64-bit ones.
fn store_hook(k: &TyKind) -> Option<&'static str> {
    use velt_common::std_hook as h;
    Some(match k {
        TyKind::Int(IntTy::U8) => h!(number_to_u8),
        TyKind::Int(IntTy::I8) => h!(number_to_i8),
        TyKind::Int(IntTy::U16) => h!(number_to_u16),
        TyKind::Int(IntTy::I16) => h!(number_to_i16),
        TyKind::Int(IntTy::U32) => h!(number_to_u32),
        TyKind::Int(IntTy::I32) => h!(number_to_i32),
        _ => return None,
    })
}
