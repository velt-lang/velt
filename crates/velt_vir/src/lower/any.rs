//! `any` and `unknown` (issue #924; hir_encodings.md "any and unknown"): interface values of
//! the prelude's `any` / `unknown`, built by `ToDyn` with `Program::any_impl`. Their table is
//! the type descriptor of the value they hold: besides the hidden slots of every interface
//! table (drop, clone, format, share, transfer, class id) it has
//!
//! | slot | entry |
//! |---|---|
//! | 0 | the `typeof` tag (`TAG_*`, an integer, not an address) |
//! | 1 | `AnyTruthy`: `(data: ptr) -> bool` |
//! | 2 | `AnySame`: `(a: ptr, b: ptr) -> bool`, `===` of two data pointers of this type |
//! | 3 | `AnyNumber`: `(data: ptr) -> f64`, JavaScript's `ToNumber` (booleans, numbers and strings only) |
//!
//! Tables are built per concrete type and memoized, so two values of one type have the same
//! table: a checked conversion compares one pointer.

use velt_sema::hir::{self, TyId, TyKind};

use super::glue::{Glue, VtableKey};
use super::rt::Rt;
use super::{cint, Cx, FnLower, Work};
use crate::vir::{
    self, BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG,
};

pub(super) const SLOT_TAG: i128 = 0;
const SLOT_TRUTHY: i128 = 1;
const SLOT_SAME: i128 = 2;
const SLOT_NUMBER: i128 = 3;

pub(super) const TAG_UNDEFINED: u64 = 0;
const TAG_NULL: u64 = 1;
const TAG_BOOLEAN: u64 = 2;
const TAG_NUMBER: u64 = 3;
const TAG_STRING: u64 = 4;
const TAG_SYMBOL: u64 = 5;
const TAG_FUNCTION: u64 = 6;
const TAG_OBJECT: u64 = 7;

/// `typeof` text of each tag.
const TYPEOF: [&str; 8] = [
    "undefined",
    "object",
    "boolean",
    "number",
    "string",
    "symbol",
    "function",
    "object",
];

/// What a value of each tag is called in a `TypeError`.
const FOUND: [&str; 8] = [
    "undefined",
    "null",
    "a boolean",
    "a number",
    "a string",
    "a symbol",
    "a function",
    "an object",
];

impl Cx<'_> {
    /// Is `t` `any` or `unknown`?
    pub(super) fn is_dynamic(&self, t: TyId) -> bool {
        let Some(i) = self.hir.any_impl else {
            return false;
        };
        let any = self.hir.impls[i as usize].iface;
        match self.types.kind(t) {
            TyKind::Dyn(d, args) if args.is_empty() => *d == any || Some(*d) == self.hir.unknown,
            _ => false,
        }
    }

    /// Is `index` the implementation of `any` for every type?
    pub(super) fn is_any_impl(&self, index: u32) -> bool {
        self.hir.any_impl == Some(index)
    }

    /// The `typeof` tag of a value of the concrete type `t` held by an `any`.
    pub(super) fn any_tag(&self, t: TyId) -> u64 {
        match self.types.kind(t) {
            // Options are unpacked when boxed: an option is the dynamic `null` / `undefined`.
            TyKind::Option(_, n) if n.has_null() => TAG_NULL,
            TyKind::Option(..) | TyKind::Unit => TAG_UNDEFINED,
            TyKind::Bool => TAG_BOOLEAN,
            TyKind::Int(_) | TyKind::Float(_) => TAG_NUMBER,
            TyKind::Str => TAG_STRING,
            TyKind::Symbol => TAG_SYMBOL,
            TyKind::FnPtr { .. } | TyKind::Closure(_) => TAG_FUNCTION,
            TyKind::Literal(hir::LitValue::Str(_)) => TAG_STRING,
            TyKind::Literal(hir::LitValue::Bool(_)) => TAG_BOOLEAN,
            TyKind::Literal(_) => TAG_NUMBER,
            _ => TAG_OBJECT,
        }
    }

    /// The entry for the name slot of the table of implementation `index` for `ty` when values
    /// of its interface are converted to `any`: the `any` table of `ty`, so the conversion
    /// keeps the data pointer and swaps the table (`make_dyn_any`).
    pub(super) fn any_table_entry(&mut self, index: u32, ty: TyId) -> Option<Const> {
        let any = self.hir.any_impl?;
        let iface = self.hir.impls[index as usize].iface;
        if !self.boxing.dyn_to_any.contains(&iface) || self.is_dynamic(ty) {
            return None;
        }
        Some(Const::Static(self.vtable(VtableKey::Impl(any, ty))))
    }

    /// The descriptor entries of the `any` table of `ty` (module docs).
    pub(super) fn any_entries(&mut self, ty: TyId) -> Vec<(i128, Const)> {
        let mut e = vec![
            (
                SLOT_TRUTHY,
                Const::Func(self.func(Work::Glue(Glue::AnyTruthy, ty))),
            ),
            (
                SLOT_SAME,
                Const::Func(self.func(Work::Glue(Glue::AnySame, ty))),
            ),
        ];
        if matches!(
            self.types.kind(ty),
            TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Str
        ) {
            e.push((
                SLOT_NUMBER,
                Const::Func(self.func(Work::Glue(Glue::AnyNumber, ty))),
            ));
        }
        e
    }
}

impl FnLower<'_, '_> {
    /// The value of concrete type `ty` an `any`'s data pointer `data` stands for.
    fn any_value_place(&mut self, data: vir::Local, ty: TyId) -> Place {
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            Place::local(data)
        } else {
            self.deref_param(data, ty)
        }
    }

    pub(super) fn any_truthy_body(&mut self, data: vir::Local, ty: TyId) {
        let p = self.any_value_place(data, ty);
        let out = Place::local(self.temp(Ty::Bool));
        self.truthy_into(&out, &p, ty);
        self.terminate(Terminator::Return(Operand::Copy(out)));
    }

    pub(super) fn any_same_body(&mut self, a: vir::Local, b: vir::Local, ty: TyId) {
        let r = if self.cx.is_class(ty) || self.cx.boxed(ty) {
            let (x, y) = (
                Operand::Copy(Place::local(a)),
                Operand::Copy(Place::local(b)),
            );
            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        } else if matches!(self.cx.kind(ty), TyKind::Option(..) | TyKind::Unit) {
            Operand::Const(Const::Bool(true), Ty::Bool)
        } else if matches!(
            self.cx.kind(ty),
            TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Symbol
        ) {
            let (pa, pb) = (self.deref_param(a, ty), self.deref_param(b, ty));
            let (x, y) = (Operand::Copy(pa), Operand::Copy(pb));
            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        } else if matches!(self.cx.kind(ty), TyKind::Str) {
            let out = Place::local(self.temp(Ty::U8));
            let args = vec![
                Operand::Copy(Place::local(a)),
                Operand::Copy(Place::local(b)),
            ];
            self.call_rt(Rt::StrEq, args, Some(out.clone()));
            let zero = cint(0, Ty::U8);
            self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Ne, Operand::Copy(out), zero),
            )
        } else if self.cx.is_fn_or_dyn(ty) {
            // A function value boxed twice is the same function: its code and environment.
            let (pa, pb) = (self.deref_param(a, ty), self.deref_param(b, ty));
            self.ref_identity(&pa, &pb, ty)
        } else if self.cx.is_object(ty) {
            // An object stored in the box itself: the same box.
            let (x, y) = (
                Operand::Copy(Place::local(a)),
                Operand::Copy(Place::local(b)),
            );
            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        } else {
            let f = self.cx.func(Work::Glue(Glue::Same, ty));
            let out = Place::local(self.temp(Ty::Bool));
            let args = vec![
                Operand::Copy(Place::local(a)),
                Operand::Copy(Place::local(b)),
            ];
            self.call(Callee::Func(f), args, Some(out.clone()), false);
            Operand::Copy(out)
        };
        self.terminate(Terminator::Return(r));
    }

    pub(super) fn any_number_body(&mut self, data: vir::Local, ty: TyId) {
        let p = self.any_value_place(data, ty);
        let v = Operand::Copy(p.clone());
        let r = match self.cx.kind(ty) {
            TyKind::Str => {
                let out = Place::local(self.temp(Ty::F64));
                let a = self.addr(p);
                self.call_rt(Rt::StrToNumber, vec![a], Some(out.clone()));
                Operand::Copy(out)
            }
            TyKind::Bool => {
                let n = self.rvalue_temp(Ty::U8, Rvalue::Cast(v, Ty::U8));
                self.rvalue_temp(Ty::F64, Rvalue::Cast(n, Ty::F64))
            }
            _ => {
                let from = self.cx.ty(ty);
                self.cast_to(v, from, Ty::F64)
            }
        };
        self.terminate(Terminator::Return(r));
    }

    /// `ToDyn` of the interface value `v` (type `cty`, not dynamic) to `any` (`ty`): the value
    /// it holds with the `any` table of its concrete type, which the interface table keeps in
    /// its name slot (`Cx::any_table_entry`), so identity, `instanceof` and checked
    /// conversions see the object itself. A table built before the conversion was known has
    /// none: the interface value is boxed as it is.
    pub(super) fn make_dyn_any(&mut self, v: Operand, cty: TyId, ty: TyId) -> Operand {
        let TyKind::Dyn(iface, _) = self.cx.kind(cty) else {
            super::ice("interface conversion of a non-interface value")
        };
        self.cx.note_dyn_to_any(iface);
        let p = self.place_of(v, cty);
        let data = Operand::Copy(proj(&p, Proj::Field(0)));
        let table = Operand::Copy(proj(&p, Proj::Field(1)));
        let at = self.dispatch(table, super::glue::SLOT_NAME);
        let (d_out, t_out) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        let has = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, at.clone(), cint(0, Ty::Ptr)),
        );
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(has, yes, no);
        self.switch_to(yes);
        self.assign(Place::local(d_out), Rvalue::Use(data));
        self.assign(Place::local(t_out), Rvalue::Use(at));
        self.goto(done);
        self.switch_to(no);
        let vt = self.cx.ty(cty);
        let b = self.alloc(vt);
        let bp = self.operand_place(b.clone(), Ty::Ptr);
        self.store(proj(&bp, Proj::Deref(vt)), Operand::Copy(p));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let own = self.vtable_addr(VtableKey::Impl(imp, cty));
        self.assign(Place::local(d_out), Rvalue::Use(b));
        self.assign(Place::local(t_out), Rvalue::Use(own));
        self.goto(done);
        self.switch_to(done);
        let ops = vec![
            Operand::Copy(Place::local(d_out)),
            Operand::Copy(Place::local(t_out)),
        ];
        self.build_agg(ty, ops)
    }

    /// The `(data, descriptor)` of the `any` operand `v`.
    fn any_parts(&mut self, v: Operand, ty: TyId) -> (Operand, Operand) {
        let p = self.place_of(v, ty);
        (
            Operand::Copy(proj(&p, Proj::Field(0))),
            Operand::Copy(proj(&p, Proj::Field(1))),
        )
    }

    /// The tag word of descriptor `desc`.
    fn any_tag_of(&mut self, desc: Operand) -> Operand {
        let off = cint(8 * (SLOT_TAG + 7), Ty::U64);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, desc, off));
        let pp = self.operand_place(p, Ty::Ptr);
        self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&pp, Proj::Deref(Ty::U64)))),
        )
    }

    /// Call descriptor slot `slot` of `desc` with `args`.
    fn any_slot_call(&mut self, desc: Operand, slot: i128, args: Vec<Operand>, ret: Ty) -> Operand {
        let f = self.dispatch(desc, slot);
        let params = vec![Ty::Ptr; args.len()];
        let out = Place::local(self.temp(ret));
        let callee = Callee::Ptr {
            target: f,
            params,
            ret,
        };
        self.call(callee, args, Some(out.clone()), false);
        Operand::Copy(out)
    }

    /// Does the `any` at `p` hold a string?
    pub(super) fn any_holds_str(&mut self, p: &Place) -> Operand {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let str_ty = self.cx.str_ty();
        let want = self.vtable_addr(VtableKey::Impl(imp, str_ty));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, want))
    }

    /// `typeof x`.
    pub(super) fn any_typeof(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let (_, desc) = self.any_parts(v, t);
        let tag = self.any_tag_of(desc);
        let out = Place::local(self.temp(Ty::Agg(STR_AGG)));
        let done = self.new_block();
        let blocks: Vec<_> = (0..TYPEOF.len()).map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(k, b)| (k as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: blocks[TAG_OBJECT as usize],
        });
        for (k, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            let s = self.str_lit(TYPEOF[k]);
            self.assign(out.clone(), Rvalue::Use(s));
            self.goto(done);
        }
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// Truthiness of an `any`.
    pub(super) fn any_truthy(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let (data, desc) = self.any_parts(v, t);
        self.any_slot_call(desc, SLOT_TRUTHY, vec![data], Ty::Bool)
    }

    /// `a === b` (`loose`: `a == b`) of two `any`s.
    pub(super) fn any_eq(&mut self, a: &hir::Expr, b: &hir::Expr, loose: bool) -> Operand {
        let (ta, tb) = (self.sub(a.ty), self.sub(b.ty));
        let va = self.expr(a);
        let vb = self.expr(b);
        let (da, xa) = self.any_parts(va, ta);
        let (db, xb) = self.any_parts(vb, tb);
        let out = Place::local(self.temp(Ty::Bool));
        let done = self.new_block();
        let set = |lw: &mut Self, v: Operand| {
            lw.assign(out.clone(), Rvalue::Use(v));
            lw.goto(done);
        };
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

    /// `AnyTo`: the value of type `ty` the owned `any` `a` holds, or a `TypeError`.
    pub(super) fn any_to(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let from = self.sub(a.ty);
        let ty = self.sub(ty);
        let v = self.consume(a);
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        if self.cx.is_dynamic(ty) {
            return self.own_value(v, ty);
        }
        let fvt = self.cx.ty(from);
        let p = Place::local(self.copy_to_temp(v, fvt));
        let r = self.any_unbox(&p, from, ty);
        self.own_value(r, ty)
    }

    /// The value of type `ty` the `any` at `p` (owned, consumed here) holds.
    fn any_unbox(&mut self, p: &Place, from: TyId, ty: TyId) -> Operand {
        if self.cx.is_dynamic(ty) {
            // The payload of an `any | null`: the value itself.
            return Operand::Copy(p.clone());
        }
        let vt = self.cx.ty(ty);
        let out = Place::local(self.temp(vt));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        if let TyKind::Option(inner, _) = self.cx.kind(ty) {
            // `null` / `undefined` give the nothing (their box is dropped), anything else is
            // checked as the payload.
            let done = self.new_block();
            let tag = self.any_tag_of(desc.clone());
            let one = cint(TAG_NULL as i128, Ty::U64);
            let nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag, one));
            let (nil_bb, some_bb) = (self.new_block(), self.new_block());
            self.branch(nil, nil_bb, some_bb);
            self.switch_to(nil_bb);
            self.drop_glue(p.clone(), from);
            let none = self.none_value(ty);
            self.assign(out.clone(), Rvalue::Use(none));
            self.goto(done);
            self.switch_to(some_bb);
            let v = self.any_unbox(p, from, inner);
            // Moved straight into `out`: a temporary registered for a drop here would be
            // dropped on the `null` path too (where it was never written) and drop the value
            // a second time on this one.
            let some = match vt {
                Ty::Ptr => v,
                Ty::Bool => Operand::Const(Const::Bool(true), Ty::Bool),
                Ty::Agg(a) => self.rvalue_temp(
                    vt,
                    Rvalue::Aggregate(a, vec![Operand::Const(Const::Bool(true), Ty::Bool), v]),
                ),
                t => super::ice(format_args!("option of type {t:?}")),
            };
            self.assign(out.clone(), Rvalue::Use(some));
            self.goto(done);
            self.switch_to(done);
            return Operand::Copy(out);
        }
        let (ok_bb, bad_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        // A class without type arguments: an instance of it or of a subclass. A generic class
        // (`Box<number>`, `Map<string, string>`): the table of that very instantiation, since
        // its class id does not say what its type arguments are.
        let class = match self.cx.kind(ty) {
            TyKind::Adt(d, args) if self.cx.is_class(ty) && args.is_empty() => Some(d),
            _ => None,
        };
        let ok = match class {
            Some(class) => {
                // An instance of the class or of a subclass.
                let is_obj = self.any_tag_of(desc.clone());
                let obj = cint(TAG_OBJECT as i128, Ty::U64);
                let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, is_obj, obj));
                let (test_bb, no_bb, join) = (self.new_block(), self.new_block(), self.new_block());
                let r = Place::local(self.temp(Ty::Bool));
                self.branch(is_obj, test_bb, no_bb);
                self.switch_to(test_bb);
                let t = self.instance_test(p, from, class);
                self.assign(r.clone(), Rvalue::Use(t));
                self.goto(join);
                self.switch_to(no_bb);
                self.assign(
                    r.clone(),
                    Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
                );
                self.goto(join);
                self.switch_to(join);
                Operand::Copy(r)
            }
            None => {
                let imp = self.cx.hir.any_impl.unwrap_or(0);
                let want = self.vtable_addr(VtableKey::Impl(imp, ty));
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want))
            }
        };
        self.branch(ok, ok_bb, bad_bb);
        self.switch_to(ok_bb);
        let v = if self.cx.is_class(ty) || self.cx.boxed(ty) {
            data.clone()
        } else {
            let dp = self.operand_place(data.clone(), Ty::Ptr);
            let v = Operand::Copy(proj(&dp, Proj::Deref(vt)));
            let v = self.copy_to_temp(v, vt);
            self.free(data, vt);
            Operand::Copy(Place::local(v))
        };
        self.assign(out.clone(), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(bad_bb);
        self.any_type_error(desc, ty);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// The name of `ty` in a `TypeError`, with its type arguments (`Box<number>`).
    fn any_target_name(&self, ty: TyId) -> String {
        let name = self.cx.type_name(ty);
        match self.cx.kind(ty) {
            TyKind::Adt(_, args) if !args.is_empty() => {
                let args: Vec<String> = args.iter().map(|a| self.any_target_name(*a)).collect();
                format!("{name}<{}>", args.join(", "))
            }
            _ => name,
        }
    }

    /// Stop with `TypeError: the value is not <ty> (it is <found>)`.
    fn any_type_error(&mut self, desc: Operand, ty: TyId) {
        let want = self.any_target_name(ty);
        let article = match want.chars().next() {
            Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        };
        let tag = self.any_tag_of(desc);
        let blocks: Vec<_> = (0..FOUND.len()).map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(k, b)| (k as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: blocks[TAG_OBJECT as usize],
        });
        for (k, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            let msg = format!(
                "TypeError: the value is not {article} {want} (it is {})",
                FOUND[k]
            );
            self.panic_msg(&msg);
            self.terminate(Terminator::Unreachable);
        }
    }
}

fn proj(p: &Place, pr: Proj) -> Place {
    let mut p = p.clone();
    p.proj.push(pr);
    p
}
