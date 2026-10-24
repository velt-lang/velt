//! Weak collections keyed by an object type that may hold a view (any_view.rs): a `WeakMap`
//! key, a `WeakSet` member or a `WeakRef` target of such a type is the object a view views, as
//! in Node, where the view is that object. A view is often a temporary of a conversion, so an
//! entry keyed by the view itself would go with it: keyed by the object, an entry lives as long
//! as the object, and `deref()` of a `WeakRef` gives the object's live view (or a new one).
//!
//! Such a key goes the way of an `object` key (weak_object.rs): the value converted to `any`
//! (a value of the type itself and a typed proxy keep their own pointer, so their identity is
//! the one they had; a view gives its object) and keyed by its data pointer. `deref()` converts
//! the `any` it rebuilds back to the type, which gives the view of an object of another type.
//! Only programs with views get here (`Cx::view_fn`).

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::weak_object::{DynKey, WeakUse, SLOT_WEAK};
use super::{cint, FnLower};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// May a weak key of the (substituted) type `kt` be a view?
    pub(super) fn view_weak_key(&mut self, kt: TyId) -> bool {
        matches!(self.cx.kind(kt), TyKind::Adt(..))
            && !self.cx.is_class(kt)
            && self.cx.proxiable(kt)
            && self.cx.view_fn().is_some()
    }

    /// The key `key` (borrowed) of the view-capable type `kt` as an `object` key (module docs),
    /// its `any` owned by a temporary of the statement; `None` in a pass whose tables lack the
    /// weak slot (the next pass has it).
    pub(super) fn view_key(&mut self, key: &hir::Expr, kt: TyId) -> Option<DynKey> {
        self.cx.note_weak_object();
        // The key reaches `object` as itself or, for a view, as any dynamic value.
        self.cx.note_to_object(Some(kt));
        self.cx.note_to_object(None);
        self.cx.note_identity(kt);
        self.cx.note_weak_seed(kt);
        let k = self.expr(key);
        if !self.cx.weak_object() {
            self.cx.facts.unmet = true;
            return None;
        }
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let s = self.share_value(k, kt);
        let held = self.temp(avt);
        self.any_box_into(s, kt, &Place::local(held));
        self.own_temp(held, any);
        let held = Place::local(held);
        let desc = Operand::Copy(proj(&held, Proj::Field(1)));
        let word = self.dispatch(desc.clone(), SLOT_WEAK);
        Some(DynKey {
            data: Operand::Copy(proj(&held, Proj::Field(0))),
            desc,
            word,
            table: None,
        })
    }

    /// `new WeakRef(target)` of the view-capable `kt`: the slot keeps the `any` table.
    pub(super) fn weakref_new_view(&mut self, target: &hir::Expr, kt: TyId) -> Operand {
        let Some(k) = self.view_key(target, kt) else {
            return cint(0, Ty::U32);
        };
        self.dyn_key_check(&k, WeakUse::Ref);
        self.weakref_new_key(&k)
    }

    /// `deref()` of a `WeakRef` of the view-capable `obj` (`t`: `obj` or `obj | undefined`):
    /// the `any` the slot rebuilds (`undefined` once the object was freed), converted to `t`.
    pub(super) fn weakref_deref_view(&mut self, r: Operand, t: TyId) -> Operand {
        self.cx.note_weak_object();
        if !self.cx.weak_object() {
            self.cx.facts.unmet = true;
            let vt = self.cx.ty(t);
            return self.zero_value(vt);
        }
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let held = self.weakref_deref_dyn(r, any, any);
        let held = self.place_of(held, any);
        // A reference of its own for the conversion, which consumes it.
        let s = self.share_value(Operand::Copy(held), any);
        let p = Place::local(self.temp(avt));
        self.assign(p.clone(), Rvalue::Use(s));
        let v = self.any_unbox(&p, any, t);
        self.own_value(v, t)
    }
}
