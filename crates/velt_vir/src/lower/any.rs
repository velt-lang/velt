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
//! | 4 | `AnyHash`: `(data: ptr) -> u64`, a hash agreeing with SameValueZero (`Set` / `Map` keys: any_eq.rs) |
//! | 5..8 | member reads and calls (any_member.rs), when the program has them |
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
pub(super) const SLOT_SAME: i128 = 2;
pub(super) const SLOT_NUMBER: i128 = 3;
pub(super) const SLOT_HASH: i128 = 4;

pub(super) const TAG_UNDEFINED: u64 = 0;
pub(super) const TAG_NULL: u64 = 1;
pub(super) const TAG_BOOLEAN: u64 = 2;
pub(super) const TAG_NUMBER: u64 = 3;
pub(super) const TAG_STRING: u64 = 4;
const TAG_SYMBOL: u64 = 5;
const TAG_FUNCTION: u64 = 6;
pub(super) const TAG_OBJECT: u64 = 7;

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
        if matches!(
            self.types.kind(ty),
            TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Str
        ) {
            e.push((
                SLOT_NUMBER,
                Const::Func(self.func(Work::Glue(Glue::AnyNumber, ty))),
            ));
        }
        e.extend(self.any_member_entries(ty));
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
        let ty = self.sub(ty);
        let v = self.consume(a);
        if self.dead() {
            let vt = self.cx.ty(ty);
            return self.zero_value(vt);
        }
        if self.cx.is_dynamic(ty) {
            if self.is_object_ty(ty) && !self.cx.is_narrow_dynamic(from) {
                // `any` / `unknown` to `object` (issue #926): a primitive is a `TypeError`;
                // `null` and `undefined` are what `object | null` holds.
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

    /// Is `t` TypeScript's `object` (issue #926)?
    fn is_object_ty(&self, t: TyId) -> bool {
        matches!(self.cx.kind(t), TyKind::Dyn(d, args) if args.is_empty() && Some(d) == self.cx.hir.object)
    }

    /// Stop with a `TypeError` when the dynamic value at `p` holds a boolean, number, string
    /// or symbol (converted to `object`, `ty`).
    fn check_not_primitive(&mut self, p: &Place, ty: TyId) {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let tag = self.any_tag_of(desc.clone());
        let lo = cint(TAG_BOOLEAN as i128, Ty::U64);
        let k = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, tag, lo));
        let n = cint((TAG_SYMBOL - TAG_BOOLEAN) as i128, Ty::U64);
        let bad = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, k, n));
        let (bad_bb, ok_bb) = (self.new_block(), self.new_block());
        self.branch(bad, bad_bb, ok_bb);
        self.switch_to(bad_bb);
        self.any_type_error(desc, ty);
        self.switch_to(ok_bb);
    }

    /// The value of type `ty` the `any` at `p` (owned, consumed here) holds.
    pub(super) fn any_unbox(&mut self, p: &Place, from: TyId, ty: TyId) -> Operand {
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
    pub(super) fn any_target_name(&self, ty: TyId) -> String {
        if let TyKind::Array(e) = self.cx.kind(ty) {
            let e = self.any_target_name(e);
            return match e.contains(' ') {
                true => format!("({e})[]"),
                false => format!("{e}[]"),
            };
        }
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
        let an = article(&want);
        // A value of the expected generic class (or an array) with other type arguments is
        // named as one: "it is a Map", "it is an array".
        let (same_id, same_name) = match self.cx.kind(ty) {
            TyKind::Array(_) => (
                Some(super::any_set_site::ARRAY_ID),
                "an array".to_string(),
            ),
            TyKind::Adt(d, args)
                if self.cx.is_class(ty) && !args.is_empty() && !self.cx.has_header(d) =>
            {
                let name = self.cx.type_name(ty);
                (Some(self.cx.class_range(d).0), format!("{} {name}", article(&name)))
            }
            _ => (None, String::new()),
        };
        let tag = self.any_tag_of(desc.clone());
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
            if let (Some(id), true) = (same_id, k == TAG_OBJECT as usize) {
                let (yes, no) = (self.new_block(), self.new_block());
                let have = self.vtable_class_id(desc.clone());
                let want_id = cint(id as i128, Ty::U64);
                let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, have, want_id));
                self.branch(same, yes, no);
                self.switch_to(yes);
                self.panic_msg(&format!(
                    "TypeError: the value is not {an} {want} (it is {same_name})"
                ));
                self.terminate(Terminator::Unreachable);
                self.switch_to(no);
            }
            let msg = format!(
                "TypeError: the value is not {an} {want} (it is {})",
                FOUND[k]
            );
            self.panic_msg(&msg);
            self.terminate(Terminator::Unreachable);
        }
    }
}

/// The indefinite article before `word` ("an Animal", "a Map").
fn article(word: &str) -> &'static str {
    match word.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

fn proj(p: &Place, pr: Proj) -> Place {
    let mut p = p.clone();
    p.proj.push(pr);
    p
}
