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
use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, BinOp, ExprKind as H, TyId};

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
}
