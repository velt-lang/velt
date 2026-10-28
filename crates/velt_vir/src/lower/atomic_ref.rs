//! `Atomic<shared<T>>` and `Atomic<shared<T> | null>` (docs/internals/design/atomics.md
//! "References"): the word is the pointer of one counted reference the `Atomic` owns (its field,
//! which its drop glue releases), and the operations move references in and out of it.
//! - `load` gives the reference borrowed, uncounted: read in place (`a.load().name`, a borrowing
//!   argument, `const c = a.load()`, which sema binds by reference) it costs the load alone, and
//!   what keeps it (a store, a return, an owning argument) counts it, as for any borrowed place.
//!   The function runs in an epoch section (epoch_sections.rs), so the borrow stays valid until
//!   it returns. In an async function or a generator, the load is counted at once instead.
//! - `store` and `exchange` move `v` in; `exchange` gives a counted copy of the old reference.
//! - `compareExchange(expected, v)` compares identity, and gives whether it swapped; `v` is
//!   released when it did not. `expected` is only read (its owner keeps it alive, so its
//!   address cannot be reused meanwhile).
//!
//! The reference `store`, `exchange` or a successful compare-and-swap takes out of the word is
//! *retired*: another thread may have read the word and not counted it yet, so releasing it at
//! once could free what that thread is about to count. It goes to `velt:sync/epoch`'s `retire`,
//! which drops it once no thread in a section can still read it.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, TyId, TyKind};

use super::FnLower;
use crate::atomic::{AtomicCall, Order, Rmw};
use crate::vir::{BinOp, Callee, Operand, Place, Rvalue, Ty};

/// `velt:sync/epoch`'s `retire`, which every retired reference goes to.
const RETIRE: &str = "std/sync/epoch::retire";

impl FnLower<'_, '_> {
    /// The `T` of the `Atomic<T>` receiver `a` when the word holds a reference.
    pub(super) fn atomic_ref_value(&mut self, a: &hir::Expr) -> Option<TyId> {
        let aty = self.sub(a.ty);
        let t = match self.cx.kind(aty) {
            TyKind::Adt(_, args) if args.len() == 1 => args[0],
            _ => return None,
        };
        let reference = match self.cx.kind(t) {
            TyKind::Shared(_) | TyKind::Str => true,
            TyKind::Option(inner, _) => matches!(self.cx.kind(inner), TyKind::Shared(_)),
            _ => false,
        };
        reference.then_some(t)
    }

    /// Is `e` the `load` of a reference word (a borrowed, uncounted reference)?
    pub(super) fn is_ref_load(&mut self, e: &hir::Expr) -> bool {
        if self.asyncx.is_some() {
            return false;
        }
        match &e.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::AtomicLoad),
                args,
            } => args
                .first()
                .is_some_and(|a| self.atomic_ref_value(a).is_some()),
            _ => false,
        }
    }

    /// An `Atomic*` intrinsic on a reference word of type `t` (`atomic_ref_value`).
    pub(super) fn atomic_ref_intrinsic(
        &mut self,
        i: Intrinsic,
        args: &[hir::Expr],
        t: TyId,
    ) -> Operand {
        use Intrinsic as I;
        if matches!(self.cx.kind(t), TyKind::Str) {
            return self.atomic_str_intrinsic(i, args);
        }
        match (i, args) {
            (I::AtomicLoad, [a, o]) => {
                let (p, _, bits) = self.atomic_word(a);
                let order = super::atomic::order(o);
                // In an async function or generator, whose code a suspension splits, the load
                // is counted at once, inside a section of its own (epoch_sections.rs).
                if self.asyncx.is_some() && !self.epoch_exempt {
                    self.epoch_call(super::epoch_sections::PIN);
                    let w = self.atomic_call(AtomicCall::Load { bits, order }, vec![p]);
                    let r = self.cast_to(w, Ty::U64, Ty::Ptr);
                    let counted = self.clone_value(r, t);
                    let counted = self.owned_ref(counted, t);
                    self.epoch_call(super::epoch_sections::UNPIN);
                    return counted;
                }
                let w = self.atomic_call(AtomicCall::Load { bits, order }, vec![p]);
                self.ref_loads = true;
                // Borrowed: a consumer that keeps it counts it (`consume`).
                self.cast_to(w, Ty::U64, Ty::Ptr)
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

    /// Hands the reference `old` of type `t`, taken out of a word, to `velt:sync/epoch`'s
    /// `retire`, which drops it once no thread in a section can still read it (none for null).
    pub(super) fn retire(&mut self, old: Operand, t: TyId) {
        let (shared_ty, nullable) = match self.cx.kind(t) {
            TyKind::Option(inner, _) => (inner, true),
            _ => (t, false),
        };
        let TyKind::Shared(value_ty) = self.cx.kind(shared_ty) else {
            super::ice("a retired reference that is not `shared`");
        };
        let d = self
            .cx
            .prelude_def(RETIRE)
            .unwrap_or_else(|| super::ice(format_args!("no `{RETIRE}`")));
        let owned = match self.cx.hir.def(d) {
            hir::Def::Fn(f) => f
                .params
                .first()
                .is_some_and(|p| p.mode == hir::PassMode::Owned),
            _ => false,
        };
        let f = self.cx.func_for_direct(d, vec![value_ty], self.mode);
        let old = self.rvalue_temp(Ty::Ptr, Rvalue::Use(old));
        let done = self.new_block();
        if nullable {
            let some = self.new_block();
            let null = Operand::Const(crate::vir::Const::Int(0), Ty::Ptr);
            let is_some = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, old.clone(), null));
            self.branch(is_some, some, done);
            self.switch_to(some);
        }
        self.call(Callee::Func(f), vec![old.clone()], None, false);
        // A borrowing `retire` keeps a reference of its own: the word's is released.
        if !owned {
            let tmp = self.copy_to_temp(old, Ty::Ptr);
            self.drop_glue(Place::local(tmp), shared_ty);
        }
        self.goto(done);
        self.switch_to(done);
    }
}
