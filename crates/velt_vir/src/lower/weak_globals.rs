//! Module-level `let`s declared `weak` (`hir::GlobalDef::weak`; weak_fields.rs,
//! docs/internals/design/cycles.md "`weak T`"): the module state's slot holds the target's slot
//! in the weak core, as a `weak` local's storage does. Every read is `Intrinsic::WeakRead` of the
//! bare `Global(d)`; an assignment swaps slots; the initializer stores 0, then its value's slot;
//! and the state's drop at exit gives the slot up ([`build_module_drop`](FnLower::build_module_drop),
//! taking the place of the state tuple's drop glue when a slot is weak). Module state is used by
//! the tasks that reach it one at a time, under the module lock, so the slot stays on one task at
//! a time.

use velt_sema::hir::{self, TyId};

use super::{FnLower, Work};
use crate::vir::{Function, Place, Terminator, Ty};

impl super::Cx<'_> {
    /// The read type of global `d` when it is a `weak` module `let`.
    pub(super) fn weak_global(&self, d: hir::DefId) -> Option<TyId> {
        match self.hir.def(d) {
            hir::Def::Global(g) if g.weak => Some(g.ty),
            _ => None,
        }
    }

    /// The drop of the module state: the state tuple's drop glue, or, when a slot is a `weak`
    /// module `let`, [`FnLower::build_module_drop`].
    pub(super) fn module_drop_func(&mut self, ty: TyId) -> crate::vir::FuncId {
        let slots = self.hir.module_state.as_ref().map(|m| m.slots.clone());
        let weak = slots
            .unwrap_or_default()
            .iter()
            .any(|&d| self.weak_global(d).is_some());
        match weak {
            true => self.func(Work::ModuleDrop),
            false => self.func(Work::Glue(super::Glue::Drop, ty)),
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The storage of the `weak` variable `e` names (a `weak` local, or a `weak` module `let`'s
    /// slot) and its read type; `None` for anything else.
    pub(super) fn weak_place(&mut self, e: &hir::Expr) -> Option<(Place, TyId)> {
        match e.kind {
            hir::ExprKind::Local(id, _) => {
                let read = self.info[id.0 as usize].weak?;
                Some((self.local_place(id), read))
            }
            hir::ExprKind::Global(d) => {
                let read = self.cx.weak_global(d)?;
                let slot = self
                    .slot_of(d)
                    .unwrap_or_else(|| super::ice("weak global without a slot"));
                Some((self.module_slot(slot), read))
            }
            _ => None,
        }
    }

    /// `(state: ptr)`: drop the module state at exit, slot by slot as the tuple's drop glue
    /// would, a `weak` module `let`'s slot giving its target's slot up.
    pub(super) fn build_module_drop(cx: &'c mut super::Cx<'h>) -> Function {
        let ms = cx.hir.module_state.as_ref();
        let (slots, ty) = ms
            .map(|m| (m.slots.clone(), m.ty))
            .unwrap_or_else(|| super::ice("a module drop without module state"));
        let mut lw = FnLower::bare(cx, vec![]);
        let state = lw.new_local(Ty::Ptr, Some("state".into()));
        let vt = lw.cx.ty(ty);
        let base = super::operand::proj(&Place::local(state), crate::vir::Proj::Deref(vt));
        let tys = match lw.cx.kind(ty) {
            hir::TyKind::Tuple(ts) => ts,
            _ => super::ice("module state is not a tuple"),
        };
        for (i, (&d, &t)) in slots.iter().zip(&tys).enumerate() {
            let p = lw.field_place(&base, ty, i as u32);
            match lw.cx.weak_global(d) {
                Some(read) => lw.weak_field_drop(p, read),
                None => lw.drop_glue(p, t),
            }
        }
        lw.terminate(Terminator::Return(super::unit()));
        lw.finish("_Gmodule_drop".to_string(), vec![Ty::Ptr], Ty::Unit)
    }
}
