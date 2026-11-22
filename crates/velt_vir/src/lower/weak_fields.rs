//! `weak T` fields (`hir::FieldDef::weak`; docs/internals/design/cycles.md "`weak T`"): a class's
//! field declared `weak T | null`, with `T` a class. Its HIR type is the read type, `T | null`,
//! one pointer; the field holds the slot of its target in the weak core instead
//! (std/lang/hooks/weak_slots.velt: a block `[count][target]` shared by every weak reference to the
//! object), or 0 for `null`:
//!
//! - a read is `s ? s.target : 0`, inline and without the lock, then a share (a retain): the
//!   reader gets a counted reference of its own, or `null` once the target was freed;
//! - a write takes the new target's slot (`weakSlot`, which marks the target weakly held), gives
//!   up the old one (`weakSlotDrop`) and releases the value written, which the field does not keep;
//! - the drop glue gives up the slot; trace glue, the cycle analysis and the weak-capable analysis
//!   see no edge (`Cx::adt_edge_tys`).
//!
//! The target's type is counted and weak-capable (its release takes the signed compare), as a weak
//! map key's is. A typed proxy's word carries bit 0 (proxy_typed.rs): the field keeps that bit in
//! its slot pointer's bit 0 (a slot is 16-byte aligned), and the slot is the `Proxy` object's.

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, TyId, TyKind};

use super::async_fn::BoxHold;
use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::{cint, unit, Cx, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// The slot block's target word (std/lang/hooks/weak_slots.velt `S_TARGET`).
const S_TARGET: i128 = 8;

impl Cx<'_> {
    /// For field `index` of class `ty` declared `weak T | null`: its read type, `T | null`.
    pub(super) fn weak_field(&mut self, ty: TyId, index: u32) -> Option<TyId> {
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return None;
        };
        if !self.adt_def(d).fields.get(index as usize)?.weak {
            return None;
        }
        self.adt_field_tys(ty).get(index as usize).copied()
    }

    /// The name of class `t` when it has a `weak` field (own or inherited): its slots belong to
    /// the weak core of the task that has their targets, so it cannot go to another task.
    pub(super) fn weak_holder(&self, t: TyId) -> Option<String> {
        let TyKind::Adt(d, _) = self.raw_kind(t) else {
            return None;
        };
        if !matches!(self.hir.def(*d), hir::Def::Adt(_)) {
            return None;
        }
        let a = self.adt_def(*d);
        let name = a.name.rsplit("::").next().unwrap_or(&a.name);
        a.fields.iter().any(|f| f.weak).then(|| name.to_string())
    }

    /// The types of class `ty`'s fields that are edges of the ownership graph: all but its
    /// `weak` ones (trace glue, the cycle and weak-capable analyses).
    pub(super) fn adt_edge_tys(&mut self, ty: TyId) -> Vec<TyId> {
        let tys = self.adt_field_tys(ty);
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return tys;
        };
        let fields = &self.adt_def(d).fields;
        (tys.into_iter().zip(fields))
            .filter(|(_, f)| !f.weak)
            .map(|(t, _)| t)
            .collect()
    }

    /// May an object of class `t` be weakly held by `weak` fields alone, without a release gate:
    /// is it in the hierarchy of a `weak` field's target? Its release then decrements it inline
    /// while its count stays above 0 (rc.rs `release_weak`); any other weak-capable type's goes
    /// to `weakRelease` at once, which is right for every object.
    pub(super) fn slot_target(&self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = self.raw_kind(t) else {
            return false;
        };
        let d = *d;
        let targets = self.weak_targets.get_or_init(|| {
            let mut out = FxHashSet::default();
            for def in &self.hir.defs {
                let hir::Def::Adt(a) = def else { continue };
                for f in a.fields.iter().filter(|f| f.weak) {
                    if let TyKind::Option(t, _) = self.raw_kind(f.ty) {
                        if let TyKind::Adt(td, _) = self.raw_kind(*t) {
                            if matches!(self.hir.def(*td), hir::Def::Adt(_)) {
                                out.insert(self.class_root(*td));
                            }
                        }
                    }
                }
            }
            out
        });
        targets.contains(&self.class_root(d))
    }

    /// The target type `T` of a weak field of read type `read` (`T | null`), made counted and
    /// weak-capable; `None` in a pass where it is not counted yet (the next pass counts it).
    fn weak_target(&mut self, read: TyId) -> Option<TyId> {
        let TyKind::Option(t, _) = self.kind(read) else {
            return None;
        };
        self.note_identity(t);
        self.note_share(t);
        self.note_weak_seed(t);
        if self.proxiable(t) {
            let pty = self.proxy_class_ty();
            self.note_weak_seed(pty);
        }
        if self.counted(t) && self.ty(t) == Ty::Ptr {
            return Some(t);
        }
        self.facts.unmet = true;
        None
    }
}

impl FnLower<'_, '_> {
    /// Is `e` a read of a weak field, whose value is a new reference (an `Intrinsic::Share` of it
    /// needs no other)?
    pub(super) fn weak_read_expr(&mut self, e: &hir::Expr) -> bool {
        let hir::ExprKind::Field { base, index, .. } = &e.kind else {
            return false;
        };
        let bty = self.sub(base.ty);
        !self.cx.proxiable(bty) && self.cx.weak_field(bty, *index).is_some()
    }

    /// `base.f` for a weak field `f` of `bty` (`None` for another field): its target, retained,
    /// whatever the use mode, as the field keeps its slot.
    pub(super) fn weak_field_expr(
        &mut self,
        base: &hir::Expr,
        bty: TyId,
        index: u32,
    ) -> Option<Operand> {
        let read = self.cx.weak_field(bty, index)?;
        let v = self.projection_base(base, BoxHold::No);
        let bp = self.place_of(v, bty);
        let fp = self.field_place(&bp, bty, index);
        Some(self.weak_read(fp, read))
    }

    /// The place `fp` of field `index` of `bty` on the way to a place (`n.parent!.x = v`): for a
    /// weak field, its target, held for the statement (an assignment to the field itself is
    /// [`weak_assign`](Self::weak_assign)'s).
    pub(super) fn weak_on_the_way(&mut self, fp: Place, bty: TyId, index: u32) -> Place {
        match self.cx.weak_field(bty, index) {
            Some(read) => {
                let v = self.weak_read(fp, read);
                self.place_of(v, read)
            }
            None => fp,
        }
    }

    /// `base.f = value` for a weak field `f` of `bty` (`None` for another field): the object,
    /// held while the value is evaluated (JavaScript's order), then the value's slot in the field.
    pub(super) fn weak_assign(
        &mut self,
        (base, bty, index): (&hir::Expr, TyId, u32),
        value: &hir::Expr,
    ) -> Option<Operand> {
        let read = self.cx.weak_field(bty, index)?;
        let later = self.later_of(value);
        let b = self.expr_held(base, later);
        let v = self.consume(value);
        if self.dead() {
            return Some(unit());
        }
        let bp = self.place_of(b, bty);
        let fp = self.field_place(&bp, bty, index);
        self.weak_store(fp, v, read);
        Some(unit())
    }

    /// The value of the weak field whose slot is at `fp`, of read type `read`: its target with a
    /// reference of its own, or `null`, in an owned temporary (dropped with the scope).
    pub(super) fn weak_read(&mut self, fp: Place, read: TyId) -> Operand {
        let v = self.weak_value(fp, read);
        self.own_value(v, read)
    }

    /// [`weak_read`](Self::weak_read) into a temporary the caller drops (glue, which has no
    /// scopes).
    pub(super) fn weak_value(&mut self, fp: Place, read: TyId) -> Operand {
        let Some(t) = self.cx.weak_target(read) else {
            return self.none_value(read);
        };
        let tagged = self.cx.proxiable(t);
        let field = self.as_usize(Operand::Copy(fp));
        let s = match tagged {
            true => self.word_and(field.clone(), -2),
            false => field.clone(),
        };
        let word = self.temp(Ty::Ptr);
        self.assign(Place::local(word), Rvalue::Use(cint(0, Ty::Ptr)));
        let (live, join) = (self.new_block(), self.new_block());
        let some = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, s.clone(), cint(0, Ty::U64)),
        );
        self.branch(some, live, join);
        self.switch_to(live);
        let sp = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(s, Ty::Ptr));
        let at = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, sp, cint(S_TARGET, Ty::I64)),
        );
        let ap = self.operand_place(at, Ty::Ptr);
        let target = Place {
            local: ap.local,
            proj: vec![Proj::Deref(Ty::Ptr)],
        };
        let target = Operand::Copy(target);
        let target = match tagged {
            // The proxy's bit, kept in the slot pointer's (null stays null: the target is 0).
            true => {
                let tw = self.as_usize(target);
                let bit = self.word_and(field, 1);
                let ne = Rvalue::Binary(BinOp::Ne, tw.clone(), cint(0, Ty::U64));
                let alive = self.rvalue_temp(Ty::Bool, ne);
                let alive = self.rvalue_temp(Ty::U64, Rvalue::Cast(alive, Ty::U64));
                let bit = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitAnd, bit, alive));
                let w = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitOr, tw, bit));
                self.rvalue_temp(Ty::Ptr, Rvalue::Cast(w, Ty::Ptr))
            }
            false => target,
        };
        self.assign(Place::local(word), Rvalue::Use(target));
        self.goto(join);
        self.switch_to(join);
        let v = self.share_value(Operand::Copy(Place::local(word)), read);
        let vt = self.cx.ty(read);
        Operand::Copy(Place::local(self.copy_to_temp(v, vt)))
    }

    /// Store the owned value `v` (of read type `read`) into the weak field whose slot is at `fp`:
    /// the field takes `v`'s slot and gives up its old one, and `v` itself is released.
    pub(super) fn weak_store(&mut self, fp: Place, v: Operand, read: TyId) {
        let vt = self.cx.ty(read);
        let held = self.copy_to_temp(v, vt);
        let Some(t) = self.cx.weak_target(read) else {
            self.drop_glue(Place::local(held), read);
            return;
        };
        let word = self.as_usize(Operand::Copy(Place::local(held)));
        let s = match self.cx.proxiable(t) {
            true => {
                let obj = self.word_and(word.clone(), -2);
                let s = hooks::weak_slot(self, Construct::Weak, ty::USize::arg(obj))
                    .emit(self)
                    .op();
                let bit = self.word_and(word, 1);
                self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitOr, s, bit))
            }
            false => hooks::weak_slot(self, Construct::Weak, ty::USize::arg(word))
                .emit(self)
                .op(),
        };
        self.drop_glue(Place::local(held), read);
        let old = self.as_usize(Operand::Copy(fp.clone()));
        let sp = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(s, Ty::Ptr));
        self.assign(fp, Rvalue::Use(sp));
        self.weak_slot_drop(old, t);
    }

    /// Give up the slot of the weak field at `fp` (of read type `read`): its drop.
    pub(super) fn weak_field_drop(&mut self, fp: Place, read: TyId) {
        let Some(t) = self.cx.weak_target(read) else {
            return;
        };
        let old = self.as_usize(Operand::Copy(fp));
        self.weak_slot_drop(old, t);
    }

    /// `weakSlotDrop` of the field word `w` (a slot pointer, 0, or with a proxy's bit when the
    /// target `t` is proxiable).
    fn weak_slot_drop(&mut self, w: Operand, t: TyId) {
        let s = match self.cx.proxiable(t) {
            true => self.word_and(w, -2),
            false => w,
        };
        hooks::weak_slot_drop(self, Construct::Weak, ty::USize::arg(s)).emit(self);
    }

    /// `w & mask` on a word.
    fn word_and(&mut self, w: Operand, mask: i128) -> Operand {
        self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, w, cint(mask, Ty::U64)),
        )
    }
}
