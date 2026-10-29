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
//! | 3 | `AnyNumber`: `(data: ptr) -> f64`, JavaScript's `ToNumber` (booleans, numbers and strings; every type in a program with operators on `any`, any_number.rs) |
//! | 4 | `AnyHash`: `(data: ptr) -> u64`, a hash agreeing with SameValueZero (`Set` / `Map` keys: any_eq.rs) |
//! | 5..8 | member reads and calls (any_member.rs), when the program has them |
//! | 9..11 | member writes and `in` (any_set.rs, any_has.rs), when the program has them |
//! | 12 | the weak word (weak_object.rs), when `object` is a weak key |
//! | 13 | `AnyKeys` (any_keys.rs), when the program lists the keys of an `any` |
//! | 14 | `AnyJson` (json/any.rs), when the program writes a dynamic value as JSON |
//! | 15 | `AnyDelete` (any_delete.rs), when the prelude deletes a property through `any` |
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
pub(super) const SLOT_TRUTHY: i128 = 1;
pub(super) const SLOT_SAME: i128 = 2;
pub(super) const SLOT_NUMBER: i128 = 3;
pub(super) const SLOT_HASH: i128 = 4;

pub(super) const TAG_UNDEFINED: u64 = 0;
pub(super) const TAG_NULL: u64 = 1;
pub(super) const TAG_BOOLEAN: u64 = 2;
pub(super) const TAG_NUMBER: u64 = 3;
pub(super) const TAG_STRING: u64 = 4;
pub(super) const TAG_SYMBOL: u64 = 5;
pub(super) const TAG_FUNCTION: u64 = 6;
pub(super) const TAG_OBJECT: u64 = 7;

/// `typeof` text of each tag.
pub(super) const TYPEOF: [&str; 8] = [
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
pub(super) const FOUND: [&str; 8] = [
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
    /// Is `t` `any`, `unknown`, `object` or `Function`?
    pub(super) fn is_dynamic(&self, t: TyId) -> bool {
        let Some(i) = self.hir.any_impl else {
            return false;
        };
        let any = self.hir.impls[i as usize].iface;
        match self.types.kind(t) {
            TyKind::Dyn(d, args) if args.is_empty() => {
                *d == any
                    || [self.hir.unknown, self.hir.object, self.hir.function].contains(&Some(*d))
            }
            _ => false,
        }
    }

    /// Is `t` `any`, `unknown` or `object`, or an array, tuple, union or nullable type holding
    /// one (`Intrinsic::HoldsDynamic`): a type whose JavaScript text is the value's, not its
    /// static `ToString` (a `Function` is not written as a dynamic value's text)?
    pub(super) fn holds_dynamic(&mut self, t: TyId) -> bool {
        match self.types.kind(t).clone() {
            TyKind::Dyn(d, _) if self.hir.function == Some(d) => false,
            TyKind::Array(e) | TyKind::Option(e, _) => self.holds_dynamic(e),
            TyKind::Tuple(es) => es.iter().any(|e| self.holds_dynamic(*e)),
            TyKind::Adt(d, _) if matches!(self.hir.def(d), hir::Def::Enum(e) if e.is_union) => {
                let n = self.enum_def(d).variants.len() as u32;
                (0..n).any(|v| {
                    let member = self.variant_tys(t, v);
                    member.iter().any(|m| self.holds_dynamic(*m))
                })
            }
            _ => self.is_dynamic(t),
        }
    }

    /// [`Self::holds_dynamic`], or a class instance (`Intrinsic::HoldsDynamicOrClass`).
    pub(super) fn holds_dynamic_or_class(&mut self, t: TyId) -> bool {
        self.holds_dynamic(t)
            || matches!(self.types.kind(t), TyKind::Adt(d, _) if self.adt_def(*d).kind == hir::AdtKind::Class)
    }

    /// Is `t` `object` or `Function` (issue #926)?
    pub(super) fn is_narrow_dynamic(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Dyn(d, args) if args.is_empty() => {
                self.hir.any_impl.is_some()
                    && [self.hir.object, self.hir.function].contains(&Some(*d))
            }
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
            TyKind::Adt(..) if self.is_function_object(t) => TAG_FUNCTION,
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
            (
                SLOT_HASH,
                Const::Func(self.func(Work::Glue(Glue::AnyHash, ty))),
            ),
        ];
        if self.boxing.any_number
            || matches!(
                self.types.kind(ty),
                TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Str
            )
        {
            e.push((
                SLOT_NUMBER,
                Const::Func(self.func(Work::Glue(Glue::AnyNumber, ty))),
            ));
        }
        e.extend(self.any_member_entries(ty));
        if self.boxing.any_keys {
            let f = Const::Func(self.func(Work::Glue(Glue::AnyKeys, ty)));
            e.push((super::any_keys::SLOT_KEYS, f));
        }
        if self.boxing.any_json {
            let f = Const::Func(self.func(Work::Glue(Glue::AnyJson, ty)));
            e.push((super::json::any::SLOT_JSON, f));
        }
        e
    }
}

impl FnLower<'_, '_> {
    /// The value of concrete type `ty` an `any`'s data pointer `data` stands for.
    pub(super) fn any_value_place(&mut self, data: vir::Local, ty: TyId) -> Place {
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
            // The other types have the slot only in a program with operators on `any`
            // (`Boxing::any_number`, any_number.rs).
            k if !matches!(k, TyKind::Int(_) | TyKind::Float(_)) => {
                return self.any_number_other(ty)
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
        // The object an adapter wraps, not the adapter (adapters.rs): `any` reads and writes
        // its members and compares its identity, as Node sees the one object.
        let p = self.owned_unwrapped(p, cty);
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
    pub(super) fn any_parts(&mut self, v: Operand, ty: TyId) -> (Operand, Operand) {
        let p = self.place_of(v, ty);
        (
            Operand::Copy(proj(&p, Proj::Field(0))),
            Operand::Copy(proj(&p, Proj::Field(1))),
        )
    }

    /// The tag word of descriptor `desc`.
    pub(super) fn any_tag_of(&mut self, desc: Operand) -> Operand {
        let off = cint(8 * (SLOT_TAG + 7), Ty::U64);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, desc, off));
        let pp = self.operand_place(p, Ty::Ptr);
        self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&pp, Proj::Deref(Ty::U64)))),
        )
    }

    /// Call descriptor slot `slot` of `desc` with `args`.
    pub(super) fn any_slot_call(
        &mut self,
        desc: Operand,
        slot: i128,
        args: Vec<Operand>,
        ret: Ty,
    ) -> Operand {
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

    /// Does the `any` at `p` hold anything but `undefined`?
    pub(super) fn any_defined(&mut self, p: &Place) -> Operand {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let tag = self.any_tag_of(desc);
        let undef = cint(TAG_UNDEFINED as i128, Ty::U64);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, undef))
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
        if let Some(tag) = self.typeof_folded(a) {
            return tag;
        }
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

    /// `AnyTo`: the value of type `ty` the owned `any` `a` holds, or a `TypeError`.
    pub(super) fn any_to(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let from = self.sub(a.ty);
        let written = ty;
        let ty = self.sub(ty);
        if let Some(v) = self.unboxed_own(a, ty) {
            return v;
        }
        let v = self.consume(a);
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        if matches!(self.cx.kind(ty), TyKind::Never) {
            return self.any_to_never(v, from, ty, written);
        }
        if matches!(self.cx.kind(ty), TyKind::Unit) {
            return self.any_to_void(v, from);
        }
        if self.cx.is_dynamic(ty) {
            if self.is_object_ty(ty) && !self.cx.is_narrow_dynamic(from) {
                // `any` / `unknown` to `object` (issue #926): a primitive is a `TypeError`;
                // `null` and `undefined` are what `object | null` holds.
                self.cx.note_to_object(None);
                let fvt = self.cx.ty(from);
                let p = Place::local(self.copy_to_temp(v, fvt));
                self.check_not_primitive(&p, ty);
                return self.own_value(Operand::Copy(p), ty);
            }
            return self.own_value(v, ty);
        }
        let fvt = self.cx.ty(from);
        let p = Place::local(self.copy_to_temp(v, fvt));
        let r = self.any_unbox(&p, from, ty);
        self.own_value(r, ty)
    }

    /// Does a value of the concrete type `t` need unpacking or checking on its way into
    /// `object` (issue #926: `x as object` of an unbounded `T`)? Options, unions, primitives,
    /// literal and enum types, `void`, and `any` / `unknown`, which may hold a primitive.
    pub(super) fn object_unpacks(&self, t: TyId) -> bool {
        match self.cx.kind(t) {
            TyKind::Option(..)
            | TyKind::Unit
            | TyKind::Bool
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Str
            | TyKind::Symbol
            | TyKind::Literal(_) => true,
            TyKind::Dyn(..) => self.cx.is_dynamic(t) && !self.cx.is_narrow_dynamic(t),
            _ => self.is_enum(t),
        }
    }

    /// The owned value `v` of the concrete type `t` as the `object` `ty`: an option holds its
    /// payload or the dynamic `null` / `undefined`, a union the member it holds; a primitive
    /// (in an `any`, or a member) stops with a `TypeError`, as `x as object` of an `unknown`.
    pub(super) fn object_unpack(&mut self, v: Operand, t: TyId, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        let out = Place::local(self.temp(vt));
        // Assigned on every path: a union's switch has a default edge.
        let zero = self.zero_value(vt);
        self.assign(out.clone(), Rvalue::Use(zero));
        self.object_into(v, t, ty, &out);
        self.own_value(Operand::Copy(out), ty)
    }

    fn object_into(&mut self, v: Operand, t: TyId, ty: TyId, out: &Place) {
        if self.dead() {
            return;
        }
        if matches!(self.cx.kind(t), TyKind::Dyn(..)) {
            self.cx.note_to_object(None);
        }
        match self.cx.kind(t) {
            TyKind::Dyn(..) if self.cx.is_dynamic(t) => {
                self.assign(out.clone(), Rvalue::Use(v));
                if !self.cx.is_narrow_dynamic(t) {
                    self.check_not_primitive(out, ty);
                }
            }
            TyKind::Dyn(..) => {
                let o = self.make_dyn_any(v, t, ty);
                self.move_into(o, out);
            }
            TyKind::Unit => self.any_nothing_into(hir::Nothing::Undefined, out),
            TyKind::Never => self.terminate(Terminator::Unreachable),
            TyKind::Option(inner, n) => {
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                let some = self.option_is_some(&p, t);
                let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
                self.branch(some, yes, no);
                self.switch_to(yes);
                let pp = self.some_payload(&p, t);
                self.object_into(Operand::Copy(pp), inner, ty, out);
                self.goto(done);
                self.switch_to(no);
                self.any_nothing_into(n, out);
                self.goto(done);
                self.switch_to(done);
            }
            _ if self.is_enum(t) && self.union_payloads(t) => {
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                self.for_each_variant(&p, t, |lw, k, parts| match parts.as_slice() {
                    [(pp, pt)] => lw.object_into(Operand::Copy(pp.clone()), *pt, ty, out),
                    _ => match lw.variant_literal(t, k) {
                        Some(l) => {
                            let lt = lw.cx.intern(TyKind::Literal(l));
                            lw.not_object_error(lt, ty);
                        }
                        None => lw.any_nothing_into(hir::Nothing::Undefined, out),
                    },
                });
            }
            _ if self.object_unpacks(t) => self.not_object_error(t, ty),
            _ => {
                let o = self.object_leaf(v, t, ty);
                self.move_into(o, out);
            }
        }
    }

    /// Store the owned temporary `o` into `out`, which takes over its ownership.
    fn move_into(&mut self, o: Operand, out: &Place) {
        if let Operand::Copy(p) = &o {
            self.take_temp(p);
        }
        self.assign(out.clone(), Rvalue::Use(o));
    }

    /// Does the enum type `t` hold payloads (a union, not a C-like or string enum)?
    fn union_payloads(&mut self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(t) else {
            return false;
        };
        let n = self.cx.enum_def(d).variants.len() as u32;
        (0..n).any(|k| !self.cx.variant_tys(t, k).is_empty())
    }

    /// Stop with the `TypeError` of a value of the primitive type `t` converted to `object`.
    fn not_object_error(&mut self, t: TyId, ty: TyId) {
        let tag = match self.enum_strings(t) {
            Some(_) => TAG_STRING,
            None if self.is_enum(t) => TAG_NUMBER,
            None => self.cx.any_tag(t),
        };
        let msg = format!(
            "TypeError: {}the value is not an {} (it is {})",
            self.any_context_words(),
            self.any_target_name(ty),
            FOUND[tag as usize]
        );
        self.panic_msg(&msg);
        self.terminate(Terminator::Unreachable);
        let next = self.new_block();
        self.switch_to(next);
    }

    /// An object value of the concrete type `t` as the `object` `ty`, as `make_dyn` boxes it.
    fn object_leaf(&mut self, v: Operand, t: TyId, ty: TyId) -> Operand {
        self.cx.note_dyn_identity(ty, t);
        self.cx.note_to_object(Some(t));
        let data = if self.cx.is_class(t) || self.cx.boxed(t) {
            v
        } else {
            let vt = self.cx.ty(t);
            let b = self.alloc(vt);
            let bp = self.operand_place(b.clone(), Ty::Ptr);
            self.store(proj(&bp, Proj::Deref(vt)), v);
            b
        };
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let table = self.vtable_addr(VtableKey::Impl(imp, t));
        self.build_agg(ty, vec![data, table])
    }

    pub(super) fn any_unbox(&mut self, p: &Place, from: TyId, ty: TyId) -> Operand {
        if self.cx.is_dynamic(ty) {
            // The payload of an `any | null`: the value itself.
            return Operand::Copy(p.clone());
        }
        if self.cx.is_union(ty) {
            return self.any_unbox_union(p, from, ty);
        }
        if let TyKind::Literal(hir::LitValue::Str(lit)) = self.cx.kind(ty) {
            return self.any_unbox_str_literal(p, from, ty, &lit);
        }
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            // A `void` (a field of `{ value: T }` at `T = void`): nothing to keep.
            self.drop_glue(p.clone(), from);
            return super::unit();
        }
        let out = Place::local(self.temp(vt));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        if let TyKind::Option(inner, n) = self.cx.kind(ty) {
            // `null` / `undefined` give the nothing (their box is dropped), anything else is
            // checked as the payload. On `T | null | undefined` (an option of a `T | null`),
            // only the outer option's own nothing gives its nothing: the other one is the
            // payload's (`null` is `Some(None)`).
            let done = self.new_block();
            let tag = self.any_tag_of(desc.clone());
            // A two-state option takes only its own nothing too: the other one is checked as
            // the payload, which rejects it with a `TypeError` (`undefined` is not a
            // `number | null`), as Node keeps it.
            let nil = if n != hir::Nothing::Both {
                let own = if n.has_null() {
                    TAG_NULL
                } else {
                    TAG_UNDEFINED
                };
                let own = cint(own as i128, Ty::U64);
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, own))
            } else {
                let one = cint(TAG_NULL as i128, Ty::U64);
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag, one))
            };
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
        match self.cx.kind(ty) {
            TyKind::Adt(..) => self.cx.note_proxy_target(ty),
            TyKind::Array(_) => self.cx.note_array_view(ty),
            _ => {}
        }
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
        // A record built by `{} as D` (sema's `empty_as_deferred`) where `D` is an object type.
        self.any_record_fill(p, from, ty, &out, done);
        // A generic function value held as `unknown` (glue/vtable.rs `ClosureAny`).
        self.any_to_generic_fn(p, ty, desc.clone(), &out, done);
        // An object implementing the interface, or the interface at other type arguments.
        self.any_to_iface(p, ty, &out, done);
        if self.cx.proxiable(ty) {
            // A proxy converted to a type that may hold one: the tagged `Proxy` object.
            let err = self.new_block();
            let data = Operand::Copy(proj(p, Proj::Field(0)));
            self.proxy_unbox(data, desc.clone(), &out, done, err);
            self.switch_to(err);
            // An object of another type: a view of it (any_view.rs).
            self.any_view(p, ty, &out, done);
        }
        if self.cx.array_view(ty) {
            // An array of another element type: a view of it (array_view.rs).
            self.array_view_make(p, desc.clone(), ty, &out, done);
        }
        if !self.cx.proxiable(ty) {
            // An object of another object type, where no view can be made: a copy of its
            // members (any_object_fill.rs).
            self.any_object_fill(p, from, ty, &out, done);
        }
        // A number of another numeric type (any_number.rs).
        if !self.any_number_convert(p, ty, desc.clone(), &out, done) {
            self.any_type_error(desc, ty);
        }
        self.switch_to(done);
        Operand::Copy(out)
    }
}

fn proj(p: &Place, pr: Proj) -> Place {
    let mut p = p.clone();
    p.proj.push(pr);
    p
}
