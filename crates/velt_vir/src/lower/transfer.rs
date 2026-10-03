//! Thread transfer (semantics stage 2, docs/design/semantics-stage2.md §6). Counts are not
//! atomic, so a counted object must never be reachable from two threads. A value entering
//! another task (a `spawn` argument or capture, a channel send, the HTTP handler environment, a
//! value settled on a promise from another task, a promise's result) is *transferred*, in
//! place, by the transfer glue (glue/transfer.rs): what the sender holds the only reference to
//! moves as it is (count 1, checked at run time), and only what is still shared is deep-copied
//! for the task — like JS's structured clone at a worker boundary — and the sender's reference
//! released. So a uniquely owned disposable crosses without a copy, and
//! its `[Symbol.dispose]()` runs once (#122, #263).
//!
//! A deep copy of a value owning a `[Symbol.dispose]` resource calls the type's own `clone()`
//! (glue/clone.rs); one that has no `clone()` cannot be copied: sema rejects the visible cases
//! (a value still used after the boundary, velt_sema `ownership/boundary.rs`) and the transfer
//! glue panics on the rest instead of releasing the resource twice.
//!
//! A spawned call through a function value, vtable or interface passes its arguments with the
//! borrow ABI and the callee shares the ones it keeps, so the caller passes a copy instead and
//! releases it before the task starts ([`transfer_copy`](FnLower::transfer_copy),
//! `async_fn/tasks.rs` `spawn`).

use std::collections::{HashMap, HashSet};

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::rt::Rt;
use super::{Cx, FnLower, Glue, Work};
use crate::vir::{self, Operand, Place, Ty};

impl Cx<'_> {
    /// Can a value of `t` reach a counted object (itself, or any part stored in it), or a
    /// promise whose result can? Only such values have anything to transfer.
    pub(super) fn holds_counted(&mut self, t: TyId) -> bool {
        self.holds_counted_in(t, &mut HashSet::new())
    }

    fn holds_counted_in(&mut self, t: TyId, seen: &mut HashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        if self.counted(t) {
            return true;
        }
        let parts = match self.kind(t) {
            // The implementor behind it may be any type.
            TyKind::Dyn(..) => return true,
            // Its env is counted (the transfer glue moves a unique one).
            TyKind::Closure(_) | TyKind::FnPtr { .. } => return true,
            TyKind::Array(e) | TyKind::Shared(e) => vec![e],
            // Its result reaches the awaiter on another task (`velt_rt_fut_transfer`, #160).
            TyKind::Promise(r, e) => vec![r, e],
            TyKind::Adt(..) if self.is_class(t) => self.adt_field_tys(t),
            _ => self.part_types(t),
        };
        parts.into_iter().any(|p| self.holds_counted_in(p, seen))
    }

    /// Does a value of `t` own a resource that cannot be deep-copied: a value with a
    /// `[Symbol.dispose]()` hook and no `clone()` of its own, or a promise (directly, or in a
    /// field, element or payload)? Function and interface values are checked when they are
    /// copied (their environment's or implementor's own glue).
    pub(super) fn uncopyable(&mut self, t: TyId) -> bool {
        self.uncopyable_in(t, &mut HashSet::new())
    }

    fn uncopyable_in(&mut self, t: TyId, seen: &mut HashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        let parts = match self.kind(t) {
            TyKind::Promise(..) => return true,
            TyKind::Shared(_) | TyKind::Dyn(..) | TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                return false
            }
            TyKind::Adt(..) if self.own_clone(t).is_some() => return false,
            TyKind::Adt(d, _) if self.dispose_of(d).is_some() => return true,
            TyKind::Array(e) => vec![e],
            TyKind::Adt(..) if self.is_class(t) => self.adt_field_tys(t),
            _ => self.part_types(t),
        };
        parts.into_iter().any(|p| self.uncopyable_in(p, seen))
    }

    /// The class's own `clone()` method (declared on the class itself, no parameters, not
    /// async, cannot throw, returns the class): what a deep copy of an instance calls instead
    /// of copying it field by field.
    pub(super) fn own_clone(&mut self, t: TyId) -> Option<DefId> {
        let TyKind::Adt(d, _) = self.kind(t) else {
            return None;
        };
        if !self.is_class(t) {
            return None;
        }
        if self.own_clones.is_none() {
            self.own_clones = Some(self.find_own_clones());
        }
        self.own_clones.as_ref().and_then(|m| m.get(&d).copied())
    }

    fn find_own_clones(&self) -> HashMap<DefId, DefId> {
        let mut out = HashMap::new();
        for (i, def) in self.hir.defs.iter().enumerate() {
            let hir::Def::Fn(f) = def else { continue };
            let Some(TyKind::Adt(owner, _)) = f.self_ty.map(|t| self.types.kind(t).clone()) else {
                continue;
            };
            let returns_self = matches!(self.types.kind(f.ret), TyKind::Adt(r, _) if *r == owner);
            let throws = f
                .throws
                .is_some_and(|e| !matches!(self.types.kind(e), TyKind::Never));
            if f.name.ends_with(".clone")
                && f.params.len() == 1
                && !f.is_async
                && !throws
                && returns_self
            {
                out.insert(owner, DefId(i as u32));
            }
        }
        out
    }
}

impl FnLower<'_, '_> {
    /// An owned argument of an async call: transferred when the call starts a spawned task.
    pub(super) fn maybe_transfer(&mut self, v: Operand, ty: TyId) -> Operand {
        match self.transfer_args {
            true => {
                let ty = self.sub(ty);
                self.transfer_value(v, ty)
            }
            false => v,
        }
    }

    /// A borrow-ABI argument `v` (of type `ty`, still owned by the caller) for a spawned call:
    /// a copy for the task in a temporary of the enclosing scope when it can reach a counted
    /// object. The caller keeps its reference, so everything reachable is copied.
    pub(super) fn transfer_copy(&mut self, v: Operand, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        if self.dead() || !self.cx.holds_counted(ty) {
            return v;
        }
        let copy = self.thread_copy(v, ty);
        self.own_value(copy, ty)
    }

    /// A deep copy of `v` (concrete type `ty`) for another thread, in a fresh temporary; a
    /// panic when `ty` owns a resource that cannot be copied (module docs).
    pub(super) fn thread_copy(&mut self, v: Operand, ty: TyId) -> Operand {
        if self.cx.uncopyable(ty) {
            self.panic_uncopyable(ty);
            return v;
        }
        let copy = match self.cx.kind(ty) {
            // Another reference to the env (always counted), which the transfer then copies
            // knowing its captures' types: a resource without `clone()` among them panics.
            TyKind::Closure(_) | TyKind::FnPtr { .. } => self.share_value(v, ty),
            _ => self.clone_value(v, ty),
        };
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            return copy;
        }
        // A copied closure still shares its captured variables' cells: transferring the fresh
        // copy gives it cells of its own.
        let t = Place::local(self.copy_to_temp(copy, vt));
        self.transfer_in_place(t.clone(), ty);
        Operand::Copy(t)
    }

    /// Panic: a value of `ty` that the program still shares would have to be copied for
    /// another task, but it owns a resource without `clone()` (module docs).
    pub(super) fn panic_uncopyable(&mut self, ty: TyId) {
        let name = self.cx.type_name(ty);
        let msg = self.str_lit(&format!(
            "cannot copy a `{name}` for another task: other references to it are still in use, and it owns a resource ([Symbol.dispose]) without a clone() method"
        ));
        let at = self.operand_addr(msg, Ty::Agg(vir::STR_AGG));
        self.call_rt(Rt::Panic, vec![at], None);
    }

    /// The owned value `v` of type `ty`, made safe to hand to another thread (module docs).
    pub(super) fn transfer_value(&mut self, v: Operand, ty: TyId) -> Operand {
        if self.dead() || !self.cx.holds_counted(ty) {
            return v;
        }
        let vt = self.cx.ty(ty);
        let t = Place::local(self.copy_to_temp(v, vt));
        self.transfer_in_place(t.clone(), ty);
        Operand::Copy(t)
    }

    /// Transfer the owned value at `p` (concrete type `ty`) in place (glue/transfer.rs).
    pub(super) fn transfer_in_place(&mut self, p: Place, ty: TyId) {
        if !self.cx.holds_counted(ty) {
            return;
        }
        let a = self.addr(p);
        let f = self.cx.func(Work::Glue(Glue::Transfer, ty));
        self.call(vir::Callee::Func(f), vec![a], None, false);
    }
}
