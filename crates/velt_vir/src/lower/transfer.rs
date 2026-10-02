//! Thread transfer (semantics stage 2, docs/design/semantics-stage2.md §6). Counts are not
//! atomic, so a counted object must never be reachable from two threads. Values entering a
//! spawned task are *transferred*: a value whose type holds no counted part moves (it is unique
//! by construction); any other is deep-copied — like JS's structured clone at a worker
//! boundary — and the original released. A spawned call through a function value, vtable or
//! interface passes its arguments with the borrow ABI and the callee shares the ones it keeps,
//! so the caller passes a copy instead and releases it before the task starts
//! ([`transfer_copy`](FnLower::transfer_copy), `async_fn/tasks.rs` `spawn`).

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::{Cx, FnLower};
use crate::vir::{Operand, Place};

impl Cx<'_> {
    /// Can a value of `t` reach a counted object (itself, or any part stored in it)?
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
            // The captures behind it may be any closure's (boxing/: `fn_values`). A program
            // whose closures capture only unique values moves function values, so a captured
            // object with a `[Symbol.dispose]()` is not copied (and disposed twice).
            TyKind::Closure(_) | TyKind::FnPtr { .. } => return self.boxing.fn_values,
            TyKind::Array(e) | TyKind::Shared(e) => vec![e],
            TyKind::Adt(..) if self.is_class(t) => self.adt_field_tys(t),
            _ => self.part_types(t),
        };
        parts.into_iter().any(|p| self.holds_counted_in(p, seen))
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
    /// a deep copy in a temporary of the enclosing scope when it can reach a counted object.
    pub(super) fn transfer_copy(&mut self, v: Operand, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        if self.dead() || !self.cx.holds_counted(ty) {
            return v;
        }
        let copy = self.clone_value(v, ty);
        self.own_value(copy, ty)
    }

    /// The owned value `v` of type `ty`, made safe to hand to another thread (module docs).
    pub(super) fn transfer_value(&mut self, v: Operand, ty: TyId) -> Operand {
        if self.dead() || !self.cx.holds_counted(ty) {
            return v;
        }
        let copy = self.clone_value(v.clone(), ty);
        let vt = self.cx.ty(ty);
        let src = self.operand_place(v, vt);
        self.drop_glue(src, ty);
        let t = self.copy_to_temp(copy, vt);
        Operand::Copy(Place::local(t))
    }
}
