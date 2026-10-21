//! `object` as a `WeakMap` key, a `WeakSet` member and a `WeakRef` target (issue #926; owner
//! decision 3 on #1176), on the weak core of weak.rs.
//!
//! An `object` is a dynamic value `{ data, descriptor }`. Once a program uses `object` that way
//! (`Boxing::weak_object`), every concrete type a dynamic value holds is counted and
//! weak-capable (boxing/weak.rs), and every `any` table has one more word, [`SLOT_WEAK`]:
//!
//! | word | the value |
//! |---|---|
//! | 0 | cannot be weakly held: a primitive, `null` / `undefined`, or an object the conversion boxed (a tuple, a function value) |
//! | 1 | its data pointer is a counted, weak-capable object without trace glue |
//! | other | the same, and the word is the object's trace glue |
//!
//! The weak key is the data pointer, so a key keeps its identity through `any`, `unknown` and
//! `object`. A map whose key type is `object` has no key trace of its own: each `set` passes
//! the key's (`velt_rt_weakmap_set_traced`). The trace glue looks into dynamic values through
//! the same word, so a `WeakMap<object, object>` whose values refer back to their keys (sigx's
//! raw → proxy cache) frees both once nothing else refers to either. A `WeakRef<object>` keeps
//! the target's descriptor with its slot (`velt_rt_weakref_new_with`), from which `deref()`
//! rebuilds the `object`. Programs that never make `object` weak compile as before.
//!
//! An interface value as a key (`WeakMap<Instance, V>`, sigx's component instances) is keyed the
//! same way: its interface table keeps the `any` table of the concrete type in its name slot
//! (`Cx::any_table_entry`, as for a conversion to `any`), whose weak word says what the data
//! pointer is. A `WeakRef` of an interface keeps the interface table, from which `deref()`
//! rebuilds the interface value.

use velt_sema::hir::{self, DefId, Nothing, TyId, TyKind};

use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, Cx, FnLower, Work};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The weak slot of an `any` table (module docs), after the member slots (any_has.rs's 11).
pub(super) const SLOT_WEAK: i128 = 12;

/// What the weak slot of a type's `any` table holds.
pub(in crate::lower) enum WeakSlot {
    No,
    Opaque,
    Trace(Const),
}

/// Which operation takes the key: the `TypeError` JavaScript gives for a value that cannot be
/// weakly held.
#[derive(Clone, Copy)]
pub(super) enum WeakUse {
    MapSet,
    SetAdd,
    Ref,
}

impl WeakUse {
    fn error(self) -> &'static str {
        match self {
            WeakUse::MapSet => "TypeError: Invalid value used as weak map key",
            WeakUse::SetAdd => "TypeError: Invalid value used in weak set",
            WeakUse::Ref => "TypeError: WeakRef: invalid target",
        }
    }
}

impl Cx<'_> {
    /// The weak slot of the `any` table of `t` (module docs).
    pub(in crate::lower) fn weak_slot(&mut self, t: TyId) -> WeakSlot {
        if !self.weak_keyable(t) {
            return WeakSlot::No;
        }
        if self.traceable(t) && !self.weak_refs_of(None, t).is_empty() {
            WeakSlot::Trace(Const::Func(self.func(Work::Glue(Glue::Trace, t))))
        } else {
            WeakSlot::Opaque
        }
    }
}

/// A dynamic weak key, evaluated: its data pointer, the `any` table of what it holds and its
/// weak word; for an interface value, `table` is its interface table.
pub(super) struct DynKey {
    pub(super) data: Operand,
    pub(super) desc: Operand,
    pub(super) word: Operand,
    pub(super) table: Option<Operand>,
}

impl FnLower<'_, '_> {
    /// Is `kt` (substituted) `object` or an interface, a weak key that is a dynamic value
    /// (`{ data, table }`)?
    pub(super) fn dyn_weak_key(&self, kt: TyId) -> bool {
        matches!(self.cx.kind(kt), TyKind::Dyn(..))
    }

    /// The interface of the interface type `kt` (not a dynamic type).
    fn weak_iface(&self, kt: TyId) -> Option<DefId> {
        match self.cx.kind(kt) {
            TyKind::Dyn(d, _) if !self.cx.is_dynamic(kt) => Some(d),
            _ => None,
        }
    }

    /// The `object` or interface key (borrowed): `None` in a pass whose tables lack the weak
    /// slot (the next pass has it).
    pub(super) fn dyn_key(&mut self, key: &hir::Expr, kt: TyId) -> Option<DynKey> {
        self.cx.note_weak_object();
        self.cx.note_identity(kt);
        self.cx.note_weak_seed(kt);
        let iface = self.weak_iface(kt);
        if let Some(d) = iface {
            // Its tables keep the `any` tables (as for a conversion to `object`).
            self.cx.note_dyn_to_any(d);
            self.cx.note_to_object(None);
        }
        let k = self.expr(key);
        let tables = iface.is_none_or(|d| self.cx.boxing.dyn_to_any.contains(&d));
        if !self.cx.weak_object() || !tables {
            self.cx.facts.unmet = true;
            return None;
        }
        let (data, desc) = self.any_parts(k, kt);
        let (desc, table) = match iface {
            Some(_) => (self.iface_any_table(desc.clone()), Some(desc)),
            None => (desc, None),
        };
        let word = self.dispatch(desc.clone(), SLOT_WEAK);
        Some(DynKey {
            data,
            desc,
            word,
            table,
        })
    }

    /// The `any` table an interface table keeps in its name slot (every table of an interface
    /// in `Boxing::dyn_to_any` has one).
    fn iface_any_table(&mut self, table: Operand) -> Operand {
        let at = self.dispatch(table, super::glue::SLOT_NAME);
        let has = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, at.clone(), cint(0, Ty::Ptr)),
        );
        let (bad, good) = (self.new_block(), self.new_block());
        self.branch(has, good, bad);
        self.switch_to(bad);
        self.panic_msg("ICE: an interface table without the `any` table of its type");
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
        at
    }

    /// The address a probe (`get`, `has`, `delete`) looks up: the data pointer, or null for a
    /// value that cannot be a key (never in a map, as in JavaScript, where they find nothing).
    pub(super) fn dyn_probe_addr(&mut self, k: &DynKey) -> Operand {
        let ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, k.word.clone(), cint(0, Ty::Ptr)),
        );
        let out = self.temp(Ty::Ptr);
        self.assign(Place::local(out), Rvalue::Use(cint(0, Ty::Ptr)));
        let done = self.new_block();
        self.when(ok, done);
        self.assign(Place::local(out), Rvalue::Use(k.data.clone()));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }

    /// Stop with JavaScript's `TypeError` unless the key can be weakly held (`set`, `add`,
    /// `new WeakRef`). An object Velt cannot hold weakly yet gets a message of its own.
    pub(super) fn dyn_key_check(&mut self, k: &DynKey, what: WeakUse) {
        let ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, k.word.clone(), cint(0, Ty::Ptr)),
        );
        let (bad, good) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        let tag = self.any_tag_of(k.desc.clone());
        let (prim, obj) = (self.new_block(), self.new_block());
        let lo = cint(super::any::TAG_FUNCTION as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ge, tag, lo));
        self.branch(is_obj, obj, prim);
        self.switch_to(prim);
        self.panic_msg(what.error());
        self.terminate(Terminator::Unreachable);
        self.switch_to(obj);
        self.panic_msg(&format!(
            "{} (this kind of object cannot be weakly held yet)",
            what.error()
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
    }

    /// The key's trace glue from its weak word (1: none).
    pub(super) fn dyn_key_trace(&mut self, k: &DynKey) -> Operand {
        self.weak_word_trace(k.word.clone())
    }

    fn weak_word_trace(&mut self, word: Operand) -> Operand {
        let out = self.temp(Ty::Ptr);
        self.assign(Place::local(out), Rvalue::Use(word.clone()));
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, word, cint(1, Ty::Ptr)));
        let done = self.new_block();
        self.when(one, done);
        self.assign(Place::local(out), Rvalue::Use(cint(0, Ty::Ptr)));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }

    /// `new WeakRef(target)` of an `object` or interface value: the slot keeps the descriptor
    /// (the interface table).
    pub(super) fn weakref_new_dyn(&mut self, target: &hir::Expr, kt: TyId) -> Operand {
        let Some(k) = self.dyn_key(target, kt) else {
            return cint(0, Ty::U32);
        };
        self.dyn_key_check(&k, WeakUse::Ref);
        let kept = k.table.clone().unwrap_or_else(|| k.desc.clone());
        let desc = self.cast_to(kept, Ty::Ptr, Ty::U64);
        let d = self.temp(Ty::U32);
        self.call_rt(
            Rt::WeakrefNewWith,
            vec![k.data.clone(), desc],
            Some(Place::local(d)),
        );
        Operand::Copy(Place::local(d))
    }

    /// `deref()` of a `WeakRef<object>` (`t`: `object` or `object | null`): the target with its
    /// descriptor, or `undefined` once it was freed (as JavaScript's `deref()`). An interface
    /// target is rebuilt with its interface table.
    pub(super) fn weakref_deref_dyn(&mut self, r: Operand, t: TyId, obj: TyId) -> Operand {
        let ovt = self.cx.ty(obj);
        let out = Place::local(self.temp(ovt));
        let d = self.temp(Ty::Ptr);
        self.call_rt(Rt::WeakrefDeref, vec![r.clone()], Some(Place::local(d)));
        let live = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(d)), cint(0, Ty::Ptr)),
        );
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(live.clone(), yes, no);
        self.switch_to(yes);
        let w = self.temp(Ty::U64);
        self.call_rt(Rt::WeakrefWord, vec![r], Some(Place::local(w)));
        let desc = self.cast_to(Operand::Copy(Place::local(w)), Ty::U64, Ty::Ptr);
        self.assign(
            proj(&out, Proj::Field(0)),
            Rvalue::Use(Operand::Copy(Place::local(d))),
        );
        self.assign(proj(&out, Proj::Field(1)), Rvalue::Use(desc));
        self.goto(done);
        self.switch_to(no);
        // An option (`Inst | undefined`) is absent once the target is gone, with a payload of
        // nothing to drop; an `object` holds JavaScript's `undefined`.
        let option = t != obj && matches!(self.cx.ty(t), Ty::Agg(_));
        if option {
            let zero = self.zero_value(ovt);
            self.assign(out.clone(), Rvalue::Use(zero));
        } else {
            self.any_nothing_into(Nothing::Undefined, &out);
        }
        self.goto(done);
        self.switch_to(done);
        let v = Operand::Copy(out);
        if !option {
            return self.own_value(v, t);
        }
        // `build_agg` owns the option it builds.
        self.build_agg(t, vec![live, v])
    }

    /// The trace glue's step into a dynamic value at `place`: visit its data pointer when its
    /// weak word says it is a counted object. An interface value (`iface`) finds the word in
    /// the `any` table its interface table keeps.
    pub(super) fn trace_dyn(
        &mut self,
        place: Place,
        tv: (crate::vir::Local, crate::vir::Local),
        iface: bool,
    ) {
        let mut desc = Operand::Copy(proj(&place, Proj::Field(1)));
        let data = Operand::Copy(proj(&place, Proj::Field(0)));
        let done = self.new_block();
        let has_desc = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, desc.clone(), cint(0, Ty::Ptr)),
        );
        self.when(has_desc, done);
        if iface {
            desc = self.dispatch(desc, super::glue::SLOT_NAME);
            let has = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Ne, desc.clone(), cint(0, Ty::Ptr)),
            );
            self.when(has, done);
        }
        let word = self.dispatch(desc, SLOT_WEAK);
        let held = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, word.clone(), cint(0, Ty::Ptr)),
        );
        self.when(held, done);
        let tf = self.weak_word_trace(word);
        let (visit, cx) = (
            Operand::Copy(Place::local(tv.0)),
            Operand::Copy(Place::local(tv.1)),
        );
        self.call_entry(visit, vec![cx, data, tf], vec![Ty::Ptr; 3], Ty::Unit);
        self.goto(done);
        self.switch_to(done);
    }
}
