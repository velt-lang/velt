//! Thread transfer (semantics stage 2, docs/design/semantics-stage2.md §6). Counts are not
//! atomic, so a counted object must never be reachable from two threads. Values entering a
//! spawned task are *transferred*: a value whose type holds no counted part moves (it is unique
//! by construction); any other is deep-copied — like JS's structured clone at a worker
//! boundary — and the original released. A closure or function value moves when it is the
//! only reference to its environment (count 1, checked at run time) and no capture can reach
//! a counted object (the env's `reach` word, closure.rs), so a uniquely owned capture is not
//! copied (and disposed twice, #122). A spawned call through a function value, vtable or
//! interface passes its arguments with the borrow ABI and the callee shares the ones it keeps,
//! so the caller passes a copy instead and releases it before the task starts
//! ([`transfer_copy`](FnLower::transfer_copy), `async_fn/tasks.rs` `spawn`).

use std::collections::HashSet;

use velt_sema::hir::{TyId, TyKind};

use super::operand::proj;
use super::{cint, Cx, FnLower};
use crate::vir::{BinOp, BlockId, Operand, Place, Proj, Rvalue, Ty};

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
            // Its env is counted ([`transfer_value`](FnLower::transfer_value) moves a unique one).
            TyKind::Closure(_) | TyKind::FnPtr { .. } => return true,
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
        let vt = self.cx.ty(ty);
        let t = Place::local(self.copy_to_temp(v, vt));
        let done = self.new_block();
        if matches!(self.cx.kind(ty), TyKind::Closure(_) | TyKind::FnPtr { .. }) {
            self.unless_movable_closure(&t, done);
        }
        let copy = self.clone_value(Operand::Copy(t.clone()), ty);
        self.drop_glue(t.clone(), ty);
        self.assign(t.clone(), Rvalue::Use(copy));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(t)
    }

    /// Continue when the closure value at `c` must be copied to cross threads; jump to `moved`
    /// when it may move: its env is null or in a frame, or a heap env only `c` references
    /// (count 1) whose captures reach no counted object (`reach` is 0).
    fn unless_movable_closure(&mut self, c: &Place, moved: BlockId) {
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(c, Proj::Field(1)))));
        let nn = self.non_null(env.clone());
        self.when(nn, moved);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let drop_fn = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&ep, Proj::Deref(Ty::Ptr)))),
        );
        let heap = self.non_null(drop_fn);
        self.when(heap, moved);
        let count = self.count_place(env.clone());
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(count)));
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, n, cint(1, Ty::U64)));
        let (unique, copy) = (self.new_block(), self.new_block());
        self.branch(one, unique, copy);
        self.switch_to(unique);
        // `reach` follows the drop and clone entries (closure.rs `ENV_HEADER`).
        let (word, _) = self.cx.size_align(Ty::Ptr);
        let rp = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, env, cint(2 * i128::from(word), Ty::I64)),
        );
        let rpp = self.operand_place(rp, Ty::Ptr);
        let reach = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&rpp, Proj::Deref(Ty::U64)))),
        );
        let none = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, reach, cint(0, Ty::U64)));
        self.branch(none, moved, copy);
        self.switch_to(copy);
    }
}
