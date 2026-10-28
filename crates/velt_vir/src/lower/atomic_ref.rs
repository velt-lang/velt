//! `Atomic<shared<T>>` and `Atomic<shared<T> | null>` (docs/internals/design/atomics.md
//! "References"): the word is the pointer of one counted reference the `Atomic` owns (its field,
//! which its drop glue releases), and the operations move references in and out of it.
//! - `load` gives a counted copy of the reference.
//! - `store` and `exchange` move `v` in; `exchange` gives a counted copy of the old reference.
//! - `compareExchange(expected, v)` compares identity, and gives whether it swapped; `v` is
//!   released when it did not. `expected` is only read (its owner keeps it alive, so its
//!   address cannot be reused meanwhile).
//!
//! The reference `store`, `exchange` or a successful compare-and-swap takes out of the word is
//! *retired*: another thread may have read the word and not counted it yet, so releasing it at
//! once could free what that thread is about to count. Until reclamation runs
//! (`velt:sync/epoch`), a retired reference is kept: never released, so never freed.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, TyId, TyKind};

use super::FnLower;
use crate::atomic::{AtomicCall, Order, Rmw};
use crate::vir::{BinOp, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The `T` of the `Atomic<T>` receiver `a` when the word holds a reference.
    pub(super) fn atomic_ref_value(&mut self, a: &hir::Expr) -> Option<TyId> {
        let aty = self.sub(a.ty);
        let t = match self.cx.kind(aty) {
            TyKind::Adt(_, args) if args.len() == 1 => args[0],
            _ => return None,
        };
        let inner = match self.cx.kind(t) {
            TyKind::Option(inner, _) => inner,
            _ => t,
        };
        matches!(self.cx.kind(inner), TyKind::Shared(_)).then_some(t)
    }

    /// An `Atomic*` intrinsic on a reference word of type `t` (`atomic_ref_value`).
    pub(super) fn atomic_ref_intrinsic(
        &mut self,
        i: Intrinsic,
        args: &[hir::Expr],
        t: TyId,
    ) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::AtomicLoad, [a, o]) => {
                let (p, _, bits) = self.atomic_word(a);
                let order = super::atomic::order(o);
                let w = self.atomic_call(AtomicCall::Load { bits, order }, vec![p]);
                let r = self.cast_to(w, Ty::U64, Ty::Ptr);
                let counted = self.clone_value(r, t);
                self.owned_ref(counted, t)
            }
            (I::AtomicStore, [a, v, o]) => {
                let order = super::atomic::order(o);
                let old = self.ref_exchange(a, v, order);
                self.retire(old, t);
                super::unit()
            }
            // The caller gets a counted copy of the old reference, and the word's own one is
            // retired: a reader may have read the word and not counted it yet.
            (I::AtomicRmw, [a, v, _, o]) => {
                let old = self.ref_exchange(a, v, super::atomic::order(o));
                let old = self.rvalue_temp(Ty::Ptr, Rvalue::Use(old));
                let counted = self.clone_value(old.clone(), t);
                let counted = self.owned_ref(counted, t);
                self.retire(old, t);
                counted
            }
            (I::AtomicCas | I::AtomicCasWeak, [a, e, v, o, f]) => {
                let (p, _, bits) = self.atomic_word(a);
                // Only read: sema's share of a place argument is not needed.
                let e = match &e.kind {
                    K::Call {
                        callee: hir::Callee::Intrinsic(I::Share),
                        args,
                    } if args.len() == 1 => &args[0],
                    _ => e,
                };
                let e = self.expr(e);
                let e = self.cast_to(e, Ty::Ptr, Ty::U64);
                let e = self.rvalue_temp(Ty::U64, Rvalue::Use(e));
                let v = self.consume(v);
                let v = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
                let vw = self.cast_to(v.clone(), Ty::Ptr, Ty::U64);
                let weak = i == I::AtomicCasWeak;
                let call = AtomicCall::Cas {
                    bits,
                    order: super::atomic::order(o),
                    failure: super::atomic::order(f),
                    weak,
                };
                let r = self.atomic_call(call, vec![p, e.clone(), vw]);
                let swapped = match weak {
                    true => self.rvalue_temp(Ty::Bool, Rvalue::Use(r)),
                    false => self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, r, e.clone())),
                };
                let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
                self.branch(swapped.clone(), yes, no);
                // The reference it replaced is the one `expected` holds.
                self.switch_to(yes);
                let old = self.cast_to(e, Ty::U64, Ty::Ptr);
                self.retire(old, t);
                self.goto(done);
                self.switch_to(no);
                let tmp = self.copy_to_temp(v, Ty::Ptr);
                self.drop_glue(Place::local(tmp), t);
                self.goto(done);
                self.switch_to(done);
                swapped
            }
            _ => super::ice(format_args!(
                "intrinsic {i:?} on a reference Atomic with {} arguments",
                args.len()
            )),
        }
    }

    /// Swap the reference `v` (moved in) into the word of `a`; the old reference, now the
    /// caller's.
    fn ref_exchange(&mut self, a: &hir::Expr, v: &hir::Expr, order: Order) -> Operand {
        let (p, _, bits) = self.atomic_word(a);
        let v = self.consume(v);
        let vw = self.cast_to(v, Ty::Ptr, Ty::U64);
        let call = AtomicCall::Rmw {
            op: Rmw::Xchg,
            bits,
            order,
        };
        let w = self.atomic_call(call, vec![p, vw]);
        self.cast_to(w, Ty::U64, Ty::Ptr)
    }

    /// The reference `r`, owned by the expression's temporary.
    fn owned_ref(&mut self, r: Operand, t: TyId) -> Operand {
        let tmp = self.copy_to_temp(r, Ty::Ptr);
        self.own_temp(tmp, t);
        Operand::Copy(Place::local(tmp))
    }

    /// The reference `old` of type `t`, which a store replaced: kept until reclamation frees
    /// it (module docs).
    fn retire(&mut self, old: Operand, _t: TyId) {
        let _ = old;
    }
}
