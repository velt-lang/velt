//! A promise held as `any` / `unknown` (#924, the design on its issue): boxed as it is, in the
//! `any` table of its own type `Promise<T, E>`. That table's class id word is `PROMISE_ID` (as
//! an array's is `ARRAY_ID`), and only such a table has the slot `SLOT_AWAIT`:
//! `(data, out: *VeltFut*) -> u8`, which puts a promise of type `Promise<unknown, unknown>` that
//! waits for the boxed one in `*out`. That promise is a widening wrapper (`async_fn/widen.rs`)
//! around a share of it, which boxes the value into `unknown` when it settles. It polls the boxed
//! promise itself, so awaiting it takes no turn more than awaiting the promise does.
//!
//! `Intrinsic::AnyPromise(x)` (std/lang/hooks/any_ops.velt `awaitAny`, which an `await` of an
//! `any` or `unknown` calls) is that promise, or `undefined` when `x` holds no promise.

use velt_sema::hir::{self, TyId, TyKind};

use super::any_member::ST_OK;
use super::glue::Glue;
use super::operand::proj;
use super::{cint, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The await slot of a promise's `any` table (module docs).
pub(super) const SLOT_AWAIT: i128 = 14;

/// The class id word of a promise's `any` table (module docs).
pub(super) const PROMISE_ID: u64 = u64::MAX - 4;

// The reserved class id words are distinct (class ids count up from 1; a function value's is
// `u64::MAX - 3`).
const _: () = assert!(
    PROMISE_ID != super::class_test::ASK_OBJECT
        && PROMISE_ID != super::any_set_site::ARRAY_ID
        && PROMISE_ID != super::adapters::ASK_INNER
        && PROMISE_ID != u64::MAX - 3
);

impl Cx<'_> {
    /// The await slot of the `any` table of `ty`, when `ty` is a promise.
    pub(super) fn any_await_entry(&mut self, ty: TyId) -> Option<(i128, Const)> {
        if !matches!(self.raw_kind(ty), TyKind::Promise(..)) {
            return None;
        }
        // The wrapper the slot makes is the executor's, which a program running no async code
        // lacks (it holds no promise then either).
        if !self.executor_loaded() {
            return None;
        }
        let f = self.func(Work::Glue(Glue::AnyAwait, ty));
        Some((SLOT_AWAIT, Const::Func(f)))
    }

    /// Does the program run the executor in Velt (std's `schedStart` is loaded)? Without it no
    /// promise exists, and a promise's table, whose glue is the executor's, can't be built.
    pub(super) fn executor_loaded(&self) -> bool {
        self.prelude_def(velt_common::std_hook!(sched_start))
            .is_some()
    }

    /// `Promise<unknown, unknown>`, what a promise held as `any` is awaited as.
    fn awaited_any_promise(&mut self) -> TyId {
        let unknown = match self.hir.unknown {
            Some(d) => self.intern(TyKind::Dyn(d, vec![])),
            None => self.any_ty(),
        };
        self.intern(TyKind::Promise(unknown, unknown))
    }
}

impl FnLower<'_, '_> {
    /// `Glue::AnyAwait` of the promise type `ty`: a share of the promise at `data` in a wrapper
    /// of type `Promise<unknown, unknown>`, written to `*out`.
    pub(super) fn any_await_glue(&mut self, data: vir::Local, out: vir::Local, ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        let p = self.any_value_place(data, ty);
        let held = self.share_value(Operand::Copy(p), ty);
        let to = self.cx.awaited_any_promise();
        let w = self.widen_fut(held, ty, to, false);
        let at = proj(&Place::local(out), Proj::Deref(Ty::Ptr));
        if let Operand::Copy(pl) = &w {
            self.take_temp(pl);
        }
        self.assign(at, Rvalue::Use(w));
        self.scopes.pop();
        self.terminate(Terminator::Return(cint(ST_OK, Ty::U8)));
    }

    /// `Intrinsic::AnyIsPromise(x)`: does `x` hold a promise (its table's class id word)?
    pub(super) fn any_is_promise(&mut self, x: &hir::Expr) -> Operand {
        let t = self.sub(x.ty);
        let v = self.expr(x);
        if self.dead() {
            return Operand::Const(Const::Bool(false), Ty::Bool);
        }
        let (_, desc) = self.any_parts(v, t);
        let id = self.vtable_class_id(desc);
        let want = cint(PROMISE_ID as i128, Ty::U64);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want))
    }

    /// `Intrinsic::AnyPromise(x)` of result type `ty` (`Promise<unknown, unknown> | undefined`):
    /// see the module docs.
    pub(super) fn any_promise(&mut self, x: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let (data, desc) = self.any_receiver(x);
        if self.dead() {
            return super::unit();
        }
        // The option, `undefined` until the table says a promise.
        let ovt = self.cx.ty(ty);
        let out = self.temp(ovt);
        let none = self.none_value(ty);
        self.assign(Place::local(out), Rvalue::Use(none));
        let fut = self.temp(Ty::Ptr);
        let desc_l = self.temp(Ty::Ptr);
        self.assign(Place::local(desc_l), Rvalue::Use(desc));
        let id = self.vtable_class_id(Operand::Copy(Place::local(desc_l)));
        let want = cint(PROMISE_ID as i128, Ty::U64);
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want));
        let (yes, done) = (self.new_block(), self.new_block());
        self.branch(is, yes, done);
        self.switch_to(yes);
        let fa = self.addr(Place::local(fut));
        // Only a promise's table has the slot (module docs).
        let desc = Operand::Copy(Place::local(desc_l));
        let _ = self.any_slot_call(desc, SLOT_AWAIT, vec![data, fa], Ty::U8);
        let f = Operand::Copy(Place::local(fut));
        let some = match ovt {
            Ty::Agg(a) => {
                Rvalue::Aggregate(a, vec![Operand::Const(Const::Bool(true), Ty::Bool), f])
            }
            _ => Rvalue::Use(f),
        };
        self.assign(Place::local(out), some);
        self.goto(done);
        self.switch_to(done);
        self.owned_result(Some(out), ty)
    }
}
