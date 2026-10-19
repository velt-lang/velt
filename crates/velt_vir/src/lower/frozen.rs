//! Frozen values: a value other threads read at the same time, which this code must neither
//! change nor count (#36, #708). It is the value inside a `shared<T>` (`Intrinsic::SharedRead`),
//! a param of a `Work::Frozen` instance in its mask, a binding by reference into either, or a
//! part of one of those (a *frozen root*). Sema rejects every write to one, also through the
//! functions it is passed to. Lowering must not change a reference count in one either, since
//! those are not atomic (semantics stage 2 §6):
//! - a stabilized borrow (`stable_borrow`) and a held operand (`hold`) of one take no share and
//!   retain nothing on the way: nothing replaces or frees the value while it is frozen;
//! - one consumed (moved into a variable, an owned param, a field, returned) is a deep copy
//!   (`FnLower::consume`);
//! - a counted one passed to a borrowed param of a direct call goes to the callee's
//!   `Work::Frozen` instance with that param in its mask; through a function value, a vtable or
//!   an interface (whose code cannot be specialized) it is a copy;
//! - a binding by reference into one, a `for…of` element of one and a borrowed capture of one
//!   are frozen too (the capture through a `Work::Frozen` instance of the closure).
//!
//! What holds no counted part (`Cx::holds_counted`) has no count to change: it is borrowed as
//! usual and goes to ordinary instances.

use velt_sema::hir::{self, Callee, ExprKind as K, Intrinsic, PassMode, Pat, PatKind, UseMode};

use super::FnLower;
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// Is `e` a frozen root (module docs), or a part of one?
    pub(super) fn frozen_root(&self, e: &hir::Expr) -> bool {
        match &e.kind {
            K::Call {
                callee: Callee::Intrinsic(Intrinsic::SharedRead),
                ..
            } => true,
            K::Local(l, _) => self.frozen.contains(l),
            K::Field { base, .. }
            | K::Index { base, .. }
            | K::UnwrapSome(base, _)
            | K::UnwrapVariant { expr: base, .. }
            | K::Downcast(base)
            | K::Upcast(base) => self.frozen_root(base),
            _ => false,
        }
    }

    /// Is `e` frozen and does it hold counted parts, which no code here may count?
    pub(super) fn frozen_counted(&mut self, e: &hir::Expr) -> bool {
        if !self.frozen_root(e) {
            return false;
        }
        let ty = self.sub(e.ty);
        self.cx.holds_counted(ty)
    }

    /// The mask of a direct call's params that get a frozen counted argument (`args` follow
    /// `skip` already lowered leading params): such a call goes to a `Work::Frozen` instance.
    pub(super) fn frozen_mask(
        &mut self,
        modes: &[PassMode],
        args: &[hir::Expr],
        skip: usize,
    ) -> u64 {
        let mut mask = 0u64;
        for (i, (a, m)) in args.iter().zip(modes).enumerate() {
            let at = i + skip;
            if *m == PassMode::Borrow && at < 64 && self.frozen_counted(a) {
                mask |= 1 << at;
            }
        }
        mask
    }

    /// A stabilized borrow of the frozen argument `a` (`stable_borrow`): the value itself for a
    /// frozen param of the callee (`frozen_args`) or one that holds no counted part, else a copy
    /// the call borrows (dropped with the statement).
    pub(super) fn frozen_borrow(&mut self, a: &hir::Expr) -> Operand {
        let ty = self.sub(a.ty);
        let v = self.frozen_read(a);
        if !self.frozen_args && self.cx.holds_counted(ty) {
            let v = self.clone_value(v, ty);
            return self.own_value(v, ty);
        }
        v
    }

    /// The frozen value `e` read in place: its own use borrowed whatever its HIR mode, since
    /// moving out of one would leave the all-zero value where other threads read it, and moving
    /// out of a counted part shares it.
    pub(super) fn frozen_read(&mut self, e: &hir::Expr) -> Operand {
        let mut read = e.clone();
        match &mut read.kind {
            K::Local(_, mode)
            | K::Field { mode, .. }
            | K::Index { mode, .. }
            | K::UnwrapSome(_, mode)
            | K::UnwrapVariant { mode, .. } => *mode = UseMode::Borrow,
            _ => {}
        }
        self.expr(&read)
    }

    /// The locals `pat` binds by reference become frozen (bound into a frozen value).
    pub(super) fn freeze_bindings(&mut self, pat: &Pat) {
        match &pat.kind {
            PatKind::Binding(id, UseMode::Borrow | UseMode::BorrowMut) => {
                self.frozen.insert(*id);
            }
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|p| self.freeze_bindings(p))
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| self.freeze_bindings(p)),
            PatKind::Array { elems, .. } => elems.iter().for_each(|p| self.freeze_bindings(p)),
            PatKind::Some(p) => self.freeze_bindings(p),
            _ => {}
        }
    }
}
