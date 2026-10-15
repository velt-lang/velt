//! `===`, `==` and key comparison of `any` / `unknown` values (issue #924; module docs of
//! any.rs for the descriptor). Two values of one type compare with the type's own `===`
//! (descriptor slot `AnySame`); an object seen through two static types is itself; numbers of
//! two widths compare by value. Generic code over `T = any` (`indexOf`, `===` in a generic
//! function) compares the same way (`same_values` / `eq_values`), and `Set` / `Map` keys
//! compare by SameValueZero (`NaN` is itself, `-0` is `0`) and hash with descriptor slot
//! `AnyHash`, which agrees: numbers hash their value as a float, strings their text, objects
//! their identity.

use velt_sema::hir::{self, TyId, TyKind};

use super::any::{
    SLOT_HASH, SLOT_NUMBER, SLOT_SAME, TAG_BOOLEAN, TAG_NULL, TAG_NUMBER, TAG_OBJECT, TAG_STRING,
};
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// How two `any`s compare.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnyCmp {
    /// `===`.
    Strict,
    /// `==`.
    Loose,
    /// SameValueZero (`Set` / `Map` keys, `includes`-style lookups).
    Key,
}

impl FnLower<'_, '_> {
    /// `a === b` (`loose`: `a == b`) of two `any`s.
    pub(super) fn any_eq(&mut self, a: &hir::Expr, b: &hir::Expr, loose: bool) -> Operand {
        let (ta, tb) = (self.sub(a.ty), self.sub(b.ty));
        let va = self.expr(a);
        let vb = self.expr(b);
        let (da, xa) = self.any_parts(va, ta);
        let (db, xb) = self.any_parts(vb, tb);
        let cmp = if loose { AnyCmp::Loose } else { AnyCmp::Strict };
        self.any_compare(&da, &xa, &db, &xb, cmp)
    }

    /// `===` (`key`: SameValueZero) of the `any`s at two places: generic code over `any`.
    pub(super) fn any_same_places(&mut self, a: &Place, b: &Place, key: bool) -> Operand {
        let da = Operand::Copy(proj(a, Proj::Field(0)));
        let xa = Operand::Copy(proj(a, Proj::Field(1)));
        let db = Operand::Copy(proj(b, Proj::Field(0)));
        let xb = Operand::Copy(proj(b, Proj::Field(1)));
        let cmp = if key { AnyCmp::Key } else { AnyCmp::Strict };
        self.any_compare(&da, &xa, &db, &xb, cmp)
    }

    /// Hash of the `any` at `place`, agreeing with SameValueZero (U64 operand).
    pub(super) fn any_hash(&mut self, place: &Place) -> Operand {
        let data = Operand::Copy(proj(place, Proj::Field(0)));
        let desc = Operand::Copy(proj(place, Proj::Field(1)));
        self.any_slot_call(desc, SLOT_HASH, vec![data], Ty::U64)
    }

    /// `AnyHash` of a value of concrete type `ty` at the data pointer `data`.
    pub(super) fn any_hash_body(&mut self, data: vir::Local, ty: TyId) {
        let zero = cint(0, Ty::U64);
        let h = match self.cx.kind(ty) {
            // `null` and `undefined` are each one value.
            TyKind::Option(..) | TyKind::Unit => zero,
            TyKind::Bool => {
                let p = self.deref_param(data, ty);
                let x = self.cast_to(Operand::Copy(p), Ty::Bool, Ty::U64);
                self.fx_combine(zero, x)
            }
            // Numbers of every width hash their value as a float (`1` as `i32` is `1`).
            TyKind::Int(_) | TyKind::Float(_) => {
                let p = self.deref_param(data, ty);
                let vt = self.cx.ty(ty);
                let f = self.cast_to(Operand::Copy(p), vt, Ty::F64);
                let x = self.key_float_bits(f, Ty::F64);
                self.fx_combine(zero, x)
            }
            TyKind::Str => {
                let r = self.temp(Ty::U64);
                let a = Operand::Copy(Place::local(data));
                self.call_rt(Rt::StrHash, vec![a], Some(Place::local(r)));
                Operand::Copy(Place::local(r))
            }
            // A function or interface value: its identity.
            _ if self.cx.is_fn_or_dyn(ty) => {
                let p = self.deref_param(data, ty);
                self.hash_value(&p, ty)
            }
            // An object: its pointer (a class object, a counted box, or the `any`'s own box).
            _ if self.cx.is_class(ty) || self.cx.boxed(ty) || self.cx.is_object(ty) => {
                let x = self.cast_to(Operand::Copy(Place::local(data)), Ty::Ptr, Ty::U64);
                self.fx_combine(zero, x)
            }
            _ => {
                let p = self.deref_param(data, ty);
                self.hash_value(&p, ty)
            }
        };
        self.terminate(Terminator::Return(h));
    }

    fn any_compare(
        &mut self,
        da: &Operand,
        xa: &Operand,
        db: &Operand,
        xb: &Operand,
        cmp: AnyCmp,
    ) -> Operand {
        let loose = cmp == AnyCmp::Loose;
        let (da, xa, db, xb) = (da.clone(), xa.clone(), db.clone(), xb.clone());
        let out = Place::local(self.temp(Ty::Bool));
        let done = self.new_block();
        let set = |lw: &mut Self, v: Operand| {
            lw.assign(out.clone(), Rvalue::Use(v));
            lw.goto(done);
        };
        if cmp == AnyCmp::Key {
            // Two numbers (of any widths): SameValueZero.
            let ta = self.any_tag_of(xa.clone());
            let tb = self.any_tag_of(xb.clone());
            let num = cint(TAG_NUMBER as i128, Ty::U64);
            let a_num = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ta, num.clone()));
            let b_num = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tb, num));
            let both = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a_num, b_num));
            let (nums_bb, rest_bb) = (self.new_block(), self.new_block());
            self.branch(both, nums_bb, rest_bb);
            self.switch_to(nums_bb);
            let x = self.any_slot_call(xa.clone(), SLOT_NUMBER, vec![da.clone()], Ty::F64);
            let y = self.any_slot_call(xb.clone(), SLOT_NUMBER, vec![db.clone()], Ty::F64);
            let r = self.same_value_zero(x, y);
            set(self, r);
            self.switch_to(rest_bb);
        }
        // The same type: the type's own `===`.
        let same_ty = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, xa.clone(), xb.clone()));
        let (same_bb, other_bb) = (self.new_block(), self.new_block());
        self.branch(same_ty, same_bb, other_bb);
        self.switch_to(same_bb);
        let r = self.any_slot_call(
            xa.clone(),
            SLOT_SAME,
            vec![da.clone(), db.clone()],
            Ty::Bool,
        );
        set(self, r);
        // Different types: one object seen through two static types (a class and its base,
        // an interface value) is itself; numbers of two widths compare by value; `==` also
        // converts.
        self.switch_to(other_bb);
        let tag_a = self.any_tag_of(xa.clone());
        let tag_b = self.any_tag_of(xb.clone());
        let obj = cint(TAG_OBJECT as i128, Ty::U64);
        let a_obj = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, tag_a.clone(), obj.clone()),
        );
        let b_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag_b.clone(), obj));
        let objs = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::BitAnd, a_obj.clone(), b_obj.clone()),
        );
        let (objs_bb, not_objs_bb) = (self.new_block(), self.new_block());
        self.branch(objs, objs_bb, not_objs_bb);
        self.switch_to(objs_bb);
        let r = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, da.clone(), db.clone()));
        set(self, r);
        self.switch_to(not_objs_bb);
        let num = cint(TAG_NUMBER as i128, Ty::U64);
        let a_num = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, tag_a.clone(), num.clone()),
        );
        let b_num = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag_b.clone(), num));
        let both = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a_num, b_num));
        let (nums_bb, rest_bb) = (self.new_block(), self.new_block());
        self.branch(both, nums_bb, rest_bb);
        self.switch_to(nums_bb);
        let r = self.any_numbers_equal(&da, &xa, &db, &xb);
        set(self, r);
        self.switch_to(rest_bb);
        if !loose {
            set(self, Operand::Const(Const::Bool(false), Ty::Bool));
        } else {
            // `null == undefined`; a nothing equals nothing else.
            let one = cint(TAG_NULL as i128, Ty::U64);
            let a_nil = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Le, tag_a.clone(), one.clone()),
            );
            let b_nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag_b.clone(), one));
            let any_nil = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::BitOr, a_nil.clone(), b_nil.clone()),
            );
            let (nil_bb, prim_bb) = (self.new_block(), self.new_block());
            self.branch(any_nil, nil_bb, prim_bb);
            self.switch_to(nil_bb);
            let r = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a_nil, b_nil));
            set(self, r);
            // Booleans, numbers and strings of different types: compared as numbers.
            self.switch_to(prim_bb);
            let a_p = self.any_tag_primitive(&tag_a);
            let b_p = self.any_tag_primitive(&tag_b);
            let prims = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::BitAnd, a_p.clone(), b_p.clone()),
            );
            let (conv_bb, no_bb) = (self.new_block(), self.new_block());
            self.branch(prims, conv_bb, no_bb);
            self.switch_to(conv_bb);
            let r = self.any_numbers_equal(&da, &xa, &db, &xb);
            set(self, r);
            // An object and a boolean, number or string: JavaScript converts the object with
            // `ToPrimitive` (`[] == 0` is `true`), which is not there yet: stop rather than
            // answer `false`.
            self.switch_to(no_bb);
            let a_case = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a_obj, b_p));
            let b_case = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, b_obj, a_p));
            let mixed = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitOr, a_case, b_case));
            let (mixed_bb, false_bb) = (self.new_block(), self.new_block());
            self.branch(mixed, mixed_bb, false_bb);
            self.switch_to(mixed_bb);
            self.panic_msg(
                "TypeError: `==` between an object and a boolean, number or string is not supported yet (#924); compare with `===`, or convert the object first",
            );
            self.terminate(Terminator::Unreachable);
            self.switch_to(false_bb);
            set(self, Operand::Const(Const::Bool(false), Ty::Bool));
        }
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// Is `tag` a boolean, number or string?
    fn any_tag_primitive(&mut self, tag: &Operand) -> Operand {
        let lo = cint(TAG_BOOLEAN as i128, Ty::U64);
        let hi = cint(TAG_STRING as i128, Ty::U64);
        let ge = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ge, tag.clone(), lo));
        let le = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag.clone(), hi));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, ge, le))
    }

    fn any_numbers_equal(
        &mut self,
        da: &Operand,
        xa: &Operand,
        db: &Operand,
        xb: &Operand,
    ) -> Operand {
        let x = self.any_slot_call(xa.clone(), SLOT_NUMBER, vec![da.clone()], Ty::F64);
        let y = self.any_slot_call(xb.clone(), SLOT_NUMBER, vec![db.clone()], Ty::F64);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }
}
