//! JS `===` on non-primitive values (`Intrinsic::Same`, semantics stage 2): objects — class
//! instances, arrays, object types, interface and function values — compare by identity, the
//! rest part by part. A counted value's identity is its pointer; an uncounted object has exactly
//! one home (it has a single owner), so its identity is the address of the place holding it.
//! An object type that is copied when shared (never changed in place) would lose its identity
//! that way, so comparing one records a fact that makes it counted once it is shared (boxing/).
//! Interface and function values: see `ref_identity`. A promise is the promise its holder holds
//! (`promise_identity`). As `Map` and `Set` keys (`KeyEq`), a type declared with `struct` compares
//! by content instead, field by field; a C struct (`declare c struct`) always does.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower, Glue};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `a === b` for the values of concrete type `ty` at two places (Bool operand).
    pub(super) fn same_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        // `any` / `unknown` in generic code: `===` of the values they hold (any_eq.rs).
        if self.cx.is_dynamic(ty) {
            return self.any_same_places(a, b, self.any_cmp());
        }
        // A C struct has no identity: every comparison compares its content (its fields as
        // the mode compares numbers). Inside a `struct`, by the struct's rule: its bits.
        if self.cx.c_struct(ty) && !self.bits_mode {
            return self.same_glue(a, b, ty);
        }
        // A `struct` is a value (#641): `==`, `===` and keys compare its content, floats by bits.
        if self.cx.value_struct(ty) {
            let (pa, pb) = (self.addr(a.clone()), self.addr(b.clone()));
            return self.call_glue(Glue::BitsEq, ty, vec![pa, pb]);
        }
        if let TyKind::Promise(..) = self.cx.kind(ty) {
            return self.promise_identity(a, b);
        }
        if self.cx.is_object(ty) {
            // Function and interface values keep their identity (a code / data pointer) when
            // copied: no fact (one would box them).
            if !self.cx.is_fn_or_dyn(ty) {
                self.cx.note_identity(ty);
            }
            let eq = self.identity(a, b, ty);
            if self.holds_view(ty) {
                return self.same_nothing(a, b, eq);
            }
            return eq;
        }
        match self.cx.kind(ty) {
            TyKind::Option(..) | TyKind::Tuple(_) | TyKind::Result(..) => self.same_glue(a, b, ty),
            TyKind::Adt(d, _) if !self.cx.is_c_like_enum(d) => self.same_glue(a, b, ty),
            _ => self.eq_values(a, b, ty),
        }
    }

    /// `Intrinsic::KeyEq` (JS SameValueZero, `Map` and `Set` keys): `same_values` with floats
    /// compared so that `NaN` equals itself and `0` equals `-0` (Bool operand).
    pub(super) fn key_same_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let outer = std::mem::replace(&mut self.key_mode, true);
        let r = self.same_values(a, b, ty);
        self.key_mode = outer;
        r
    }

    /// `Intrinsic::IncludesEq` (JS `includes`): `same_values`, with a number (also one an `any`
    /// or an option holds) compared by SameValueZero, so `NaN` finds `NaN`; so is a C struct's
    /// content (also an option's), which is the struct's value; tuples and the other types
    /// compare their parts as `===` does; so does a union, whose `NaN`s are equal (Bool operand).
    pub(super) fn includes_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        // An `f16`/`bf16` (a `u16` in VIR) is a number too: its NaN finds NaN.
        let keyed = match self.cx.kind(ty) {
            TyKind::Option(inner, _) => {
                self.cx.ty(inner).is_float()
                    || self.half_of(inner).is_some()
                    || self.cx.c_struct(inner)
            }
            _ => {
                self.cx.ty(ty).is_float()
                    || self.half_of(ty).is_some()
                    || self.cx.is_dynamic(ty)
                    || self.cx.c_struct(ty)
            }
        };
        if keyed {
            return self.key_same_values(a, b, ty);
        }
        let same = self.same_values(a, b, ty);
        let inner = match self.cx.kind(ty) {
            TyKind::Option(inner, _) => inner,
            _ => ty,
        };
        if !self.is_enum(inner) {
            return same;
        }
        // A union (`f64 | u8`, `f64 | string`): two values that each hold a `NaN` in a float
        // member are equal. Its other members compare as `===` does (an object by identity,
        // where the key comparison would compare a `struct` by content; a tuple holding a `NaN`
        // is not `===` itself, yet holds no `NaN` number).
        let a_nan = self.holds_nan(a, ty);
        let b_nan = self.holds_nan(b, ty);
        let nans = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a_nan, b_nan));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitOr, same, nans))
    }

    /// Does the union value at `p` (of type `ty`, or an option of a union) hold a `NaN` in one
    /// of its float members (Bool operand)?
    fn holds_nan(&mut self, p: &Place, ty: TyId) -> Operand {
        let r = self.temp(Ty::Bool);
        let no = Operand::Const(Const::Bool(false), Ty::Bool);
        self.assign(Place::local(r), Rvalue::Use(no));
        let done = self.new_block();
        let (p, union) = match self.cx.kind(ty) {
            TyKind::Option(inner, _) => {
                let some = self.option_is_some(p, ty);
                self.when(some, done);
                (self.some_payload(p, ty), inner)
            }
            _ => (p.clone(), ty),
        };
        self.for_each_variant(&p, union, |lw, _, parts| {
            let [(x, t)] = &parts[..] else {
                return;
            };
            let vt = lw.cx.ty(*t);
            if !vt.is_float() {
                return;
            }
            let (x, y) = (Operand::Copy(x.clone()), Operand::Copy(x.clone()));
            let nan = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, x, y));
            lw.assign(Place::local(r), Rvalue::Use(nan));
        });
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(r))
    }

    /// `Object.is(a, b)` (`Intrinsic::SameValue`) for the values of concrete type `ty` at two
    /// places: `same_values` with floats compared by SameValue (Bool operand).
    pub(super) fn same_value_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let outer = std::mem::replace(&mut self.value_mode, true);
        let r = self.same_values(a, b, ty);
        self.value_mode = outer;
        r
    }

    /// How the current mode compares two `any`s (any_eq.rs).
    pub(super) fn any_cmp(&self) -> super::any_eq::AnyCmp {
        use super::any_eq::AnyCmp;
        match (self.value_mode, self.key_mode) {
            (true, _) => AnyCmp::Value,
            (false, true) => AnyCmp::Key,
            (false, false) => AnyCmp::Strict,
        }
    }

    fn same_glue(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let (pa, pb) = (self.addr(a.clone()), self.addr(b.clone()));
        let g = if self.value_mode {
            Glue::SameValue
        } else if self.key_mode {
            Glue::KeySame
        } else {
            Glue::Same
        };
        self.call_glue(g, ty, vec![pa, pb])
    }

    /// Identity of two objects of type `ty`.
    fn identity(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let (x, y) = match self.cx.ty(ty) {
            // Class objects and counted boxes: the pointer is the object.
            Ty::Ptr => (Operand::Copy(a.clone()), Operand::Copy(b.clone())),
            // Interface and function values: compared here, not through `eq_values`: inside
            // `same` glue (`same_mode`) that comes back to `same_values` for objects, without end.
            _ if self.cx.is_fn_or_dyn(ty) => return self.ref_identity(a, b, ty),
            // A uniquely owned inline object: the address of its one home.
            _ => (self.addr(a.clone()), self.addr(b.clone())),
        };
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }

    /// Identity of two function or interface values: an interface value is its data pointer
    /// (a class object, or a counted box once interface values are compared: `make_dyn`); a
    /// function value is its code and its environment (each evaluation of a closure has its
    /// own once function values are compared: closure.rs).
    pub(super) fn ref_identity(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        // A generic function value (glue/generic_slots.rs): copies box the same closure, whose
        // env is its identity (every closure has one in a program that compares functions).
        if self.cx.is_generic_fn_value(ty) {
            self.note_fn_compared(ty);
            let (x, y) = (self.boxed_env(a), self.boxed_env(b));
            return self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y));
        }
        if matches!(self.cx.kind(ty), TyKind::Dyn(..)) {
            self.cx.note_identity(ty);
            let (a, b) = (self.through_adapters(a, ty), self.through_adapters(b, ty));
            let same = self.word_eq(&a, &b, 0);
            return self.dyn_values_same(&a, &b, ty, same);
        }
        self.note_fn_compared(ty);
        let code = self.word_eq(a, b, 0);
        let env = self.word_eq(a, b, 1);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, code, env))
    }

    /// `===` of two interface values of type `ty` that hold `struct` values (#641): the same
    /// object (`same`), or two values of one struct type equal by content. A struct value has
    /// no identity: each conversion boxes a copy, so its table's class id word names its type
    /// (class_test.rs `VALUE_IDS`) and its `SLOT_VALUE_EQ` compares the content as `BitsEq`
    /// does, for every instance of a generic struct too.
    fn dyn_values_same(&mut self, a: &Place, b: &Place, ty: TyId, same: Operand) -> Operand {
        if !self.cx.holds_values(ty) {
            return same;
        }
        let r = self.temp(Ty::Bool);
        self.assign(Place::local(r), Rvalue::Use(same.clone()));
        let (done, differ) = (self.new_block(), self.new_block());
        self.branch(same, done, differ);
        self.switch_to(differ);
        let ida = self.vtable_class_id(Operand::Copy(proj(a, Proj::Field(1))));
        let idb = self.vtable_class_id(Operand::Copy(proj(b, Proj::Field(1))));
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ida.clone(), idb));
        self.when(one, done);
        let (da, db) = (
            Operand::Copy(proj(a, Proj::Field(0))),
            Operand::Copy(proj(b, Proj::Field(0))),
        );
        // A class id at or above `VALUE_IDS` and below the marker words names a struct type.
        let lo = cint(super::class_test::VALUE_IDS as i128, Ty::U64);
        let hi = cint((super::class_test::VALUE_IDS << 1) as i128, Ty::U64);
        let ge = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ge, ida.clone(), lo));
        let lt = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, ida, hi));
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, ge, lt));
        let then = self.new_block();
        self.branch(is, then, done);
        self.switch_to(then);
        let vt = Operand::Copy(proj(a, Proj::Field(1)));
        let slot = super::glue::vtable::SLOT_VALUE_EQ;
        let eq = self.any_slot_call(vt, slot, vec![da, db], Ty::Bool);
        self.assign(Place::local(r), Rvalue::Use(eq));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(r))
    }

    /// Identity of the promises at `a` and `b`: each holder of a shared promise is a pointer
    /// of its own, so the runtime names the promise they hold (`velt_rt_fut_id`, #1682).
    pub(super) fn promise_identity(&mut self, a: &Place, b: &Place) -> Operand {
        let (x, y) = (self.promise_id(a), self.promise_id(b));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }

    /// The identity of the promise at `p` (U64 operand).
    pub(super) fn promise_id(&mut self, p: &Place) -> Operand {
        let r = self.temp(Ty::U64);
        let f = Operand::Copy(p.clone());
        self.call_rt(Rt::FutId, vec![f], Some(Place::local(r)));
        Operand::Copy(Place::local(r))
    }

    /// The env of the closure the generic function value at `p` boxes.
    pub(super) fn boxed_env(&mut self, p: &Place) -> Operand {
        let agg = self.cx.closure_agg();
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))));
        let dp = self.operand_place(data, Ty::Ptr);
        Operand::Copy(Place {
            local: dp.local,
            proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(1)],
        })
    }

    /// Do the words `field` of the values at `a` and `b` agree (Bool operand)?
    fn word_eq(&mut self, a: &Place, b: &Place, field: u32) -> Operand {
        let f = Proj::Field(field);
        let (x, y) = (Operand::Copy(proj(a, f.clone())), Operand::Copy(proj(b, f)));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }

    /// `Glue::Same` of an option / union / tuple: the equality glue comparing objects inside it
    /// by identity.
    pub(super) fn same_body(&mut self, pa: crate::vir::Local, pb: crate::vir::Local, ty: TyId) {
        self.same_mode = true;
        self.eq_body(pa, pb, ty);
    }

    /// `Glue::BitsEq`: a `struct` value's fields by content (strings by text, objects inside by
    /// identity), floats by their bits.
    pub(super) fn bits_eq_body(&mut self, pa: crate::vir::Local, pb: crate::vir::Local, ty: TyId) {
        self.bits_mode = true;
        self.same_body(pa, pb, ty);
    }

    /// `Glue::BitsHash`: the hash agreeing with `Glue::BitsEq`.
    pub(super) fn bits_hash_body(&mut self, p: crate::vir::Local, ty: TyId) {
        self.bits_mode = true;
        self.hash_body(p, ty);
    }

    /// `Glue::KeySame`: `same_body` with floats compared by SameValueZero.
    pub(super) fn key_same_body(&mut self, pa: crate::vir::Local, pb: crate::vir::Local, ty: TyId) {
        self.key_mode = true;
        self.same_body(pa, pb, ty);
    }

    /// `Glue::SameValue`: `Glue::Same` with floats compared by SameValue.
    pub(super) fn same_value_body(
        &mut self,
        pa: crate::vir::Local,
        pb: crate::vir::Local,
        ty: TyId,
    ) {
        self.value_mode = true;
        self.same_body(pa, pb, ty);
    }
}

impl super::Cx<'_> {
    /// Is `t` a generic function value: an interface value of an interface whose one method is
    /// sema's `#call` (`velt_sema` `generic_fns`), which only generic closures implement?
    pub(super) fn is_generic_fn_value(&self, t: TyId) -> bool {
        let TyKind::Dyn(d, _) = self.kind(t) else {
            return false;
        };
        matches!(self.hir.def(d), hir::Def::Interface(i)
            if i.methods.len() == 1 && i.methods[0].name == "#call")
    }

    /// Is `t` an object type (compared by identity)?
    pub(super) fn is_object(&self, t: TyId) -> bool {
        match self.raw_kind(t) {
            TyKind::Array(_) | TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Adt(_)),
            _ => false,
        }
    }

    /// May a value of the interface value type `t` be a `struct` value (#641), compared and
    /// hashed by content? True when a `struct` type, generic or not, implements its interface.
    pub(in crate::lower) fn holds_values(&self, t: TyId) -> bool {
        let TyKind::Dyn(iface, _) = self.raw_kind(t) else {
            return false;
        };
        self.hir
            .impls
            .iter()
            .any(|imp| imp.iface == *iface && self.value_struct(imp.ty))
    }

    /// Is `t` a value type declared with `struct` (#641): no identity, compared and hashed by
    /// content (`hir::AdtDef::declared_struct`)?
    pub(in crate::lower) fn value_struct(&self, t: TyId) -> bool {
        self.content_key(t)
    }

    /// Is `t` a type declared with `struct`, whose `Map` and `Set` keys compare and hash by
    /// content (`hir::AdtDef::declared_struct`)?
    pub(super) fn content_key(&self, t: TyId) -> bool {
        match self.raw_kind(t) {
            TyKind::Adt(d, _) => {
                matches!(self.hir.def(*d), hir::Def::Adt(a) if a.declared_struct)
            }
            _ => false,
        }
    }

    /// Is `t` a C struct (`hir::AdtDef::c_struct`): a value with no identity, never counted or
    /// boxed, whose `===`, `Object.is`, `includes` and keys compare its content?
    pub(super) fn c_struct(&self, t: TyId) -> bool {
        match self.raw_kind(t) {
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Adt(a) if a.c_struct),
            _ => false,
        }
    }

    /// Is every value of `t` an object, never a primitive (a struct, class, object type,
    /// array, tuple, map or function type)? `Intrinsic::MayHoldPrimitive` is its negation.
    pub(super) fn object_only(&self, t: TyId) -> bool {
        match self.raw_kind(t) {
            TyKind::Array(_)
            | TyKind::Map(..)
            | TyKind::Tuple(_)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_) => true,
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Adt(_)),
            _ => false,
        }
    }

    /// Is `t` an object type whose values a share copies unless it is counted (an array or an
    /// object type, not a class, interface or function value)?
    pub(super) fn copied_object(&self, t: TyId) -> bool {
        self.is_object(t) && !self.is_class(t) && !self.is_fn_or_dyn(t)
    }

    pub(super) fn is_fn_or_dyn(&self, t: TyId) -> bool {
        matches!(
            self.raw_kind(t),
            TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_)
        )
    }
}
