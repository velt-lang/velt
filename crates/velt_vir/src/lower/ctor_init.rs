//! Field initializers run in JavaScript's order: a class's own initializers run once its base
//! class is constructed, before the rest of its constructor body. For `new D()` with
//! `class D extends B`: B's initializers, B's constructor body, D's initializers, D's
//! constructor body.
//!
//! Each constructor runs the initializers of its class and of the classes between it and the
//! class declaring the base constructor it calls: right after `super(...)` returns, or on entry
//! when no ancestor has a constructor. `new C()` calls C's constructor (possibly inherited)
//! and then runs the initializers of the classes below the one declaring it.

use velt_sema::hir::{self, DefId, LocalId, TyId, TyKind};

use super::{ice, FnLower};
use crate::vir::Place;

impl FnLower<'_, '_> {
    /// The concrete class type when `def` is that class's own constructor.
    pub(super) fn ctor_class(&mut self, def: DefId, f: &hir::FnDef) -> Option<TyId> {
        let ty = self.sub(f.self_ty?);
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        match self.cx.hir.def(d) {
            hir::Def::Adt(a) if a.ctor == Some(def) => Some(ty),
            _ => None,
        }
    }

    /// The constructor `super(...)` calls in a constructor of class `ty`, if any.
    fn base_ctor(&mut self, ty: TyId) -> Option<DefId> {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return None;
        };
        let base = self.cx.adt_def(d).base?;
        let base = self.cx.subst(base, &args);
        match self.cx.kind(base) {
            TyKind::Adt(b, _) => self.cx.adt_def(b).ctor,
            _ => None,
        }
    }

    /// How many fields the class declaring constructor `ctor` has (its own and inherited
    /// ones): the initializers of those run inside `ctor`.
    pub(super) fn ctor_fields(&mut self, ctor: DefId) -> usize {
        match self.cx.fn_def(ctor).self_ty.map(|t| self.cx.kind(t)) {
            Some(TyKind::Adt(d, _)) => self.cx.adt_def(d).fields.len(),
            _ => ice("constructor without a class `this`"),
        }
    }

    /// On entry to a constructor whose class has no ancestor with a constructor: every field
    /// initializer of the class.
    pub(super) fn ctor_entry_inits(&mut self) {
        let Some(ty) = self.ctor_self else { return };
        if self.base_ctor(ty).is_none() {
            let this = self.local_place(LocalId(0));
            self.init_fields(&this, ty, 0);
        }
    }

    /// After a call of `def`: when it is the `super(...)` call of the constructor being
    /// lowered, the initializers of the fields the base constructor did not initialize.
    pub(super) fn after_super_inits(&mut self, def: DefId) {
        let Some(ty) = self.ctor_self else { return };
        if self.base_ctor(ty) != Some(def) || self.dead() {
            return;
        }
        let from = self.ctor_fields(def);
        let this = self.local_place(LocalId(0));
        self.init_fields(&this, ty, from);
    }

    /// Store the initializer of every field of class `ty` (concrete) from index `from` on
    /// into the object `obj` points to, evaluated in the class's type context.
    pub(super) fn init_fields(&mut self, obj: &Place, ty: TyId, from: usize) {
        let TyKind::Adt(d, cargs) = self.cx.kind(ty) else {
            ice("field initializers of a non-class type")
        };
        let adt = self.cx.adt_def(d);
        let saved = std::mem::replace(&mut self.targs, cargs);
        for (i, f) in adt.fields.iter().enumerate().skip(from) {
            if let Some(def) = &f.default {
                let v = self.consume(def);
                let p = self.field_place(obj, ty, i as u32);
                self.store(p, v);
            }
        }
        self.targs = saved;
    }
}
