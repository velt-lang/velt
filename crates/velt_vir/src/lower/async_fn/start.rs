//! Eager start of promise values (hybrid promises, docs/reference/async.md). A call of an async
//! function — or of a function value returning a promise — whose promise is kept as a value
//! (stored, put in an array, passed on, returned) is started at once with `velt_rt_fut_start`:
//! its body runs until its first suspension, like calling an async function in JS, and the
//! current task drives it from then on. So is a rejecting `Promise.all` / `race` kept as a
//! value (kept.rs), and the promise inside one widened to a wider error type (widen.rs).
//!
//! The zero-cost forms never get here: `await f()` embeds `f`'s state in the caller's state and
//! `spawn(f())` gives `f` its own task, both from `f`'s initial state; `await` / `spawn` of a
//! call that has to be boxed (recursion, function values) take the lazy box (`take_promise`).
//!
//! A started promise nobody awaits still finishes; the runtime then disposes of its result slot
//! with the function built here (`Work::Unclaimed`): a rejection (`Err` in a `Result<T, E>`
//! slot) is reported like an unhandled rejection (`Uncaught <Type>: message`, exit 1).

use velt_sema::hir::{self, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, Cx, FnLower, Work};
use crate::vir::{BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// A call used as a value (`ExprKind::Call`); a compiled promise it returns is started unless
    /// it is awaited or spawned right away (`lazy_call`, set by `take_promise`). Async functions
    /// and function values returning promises (async closures) return lazy boxed promises, and so
    /// do the combinator intrinsics (kept.rs); `PromiseWiden` starts its inner promise (widen.rs);
    /// other intrinsics, externs (runtime leaves) and
    /// synchronous functions (which started any promise they return) don't.
    pub(in crate::lower) fn call_value(
        &mut self,
        callee: &hir::Callee,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        let lazy = std::mem::take(&mut self.lazy_call);
        if let (hir::Callee::Intrinsic(hir::Intrinsic::PromiseWiden), [p]) = (callee, args) {
            return self.promise_widen(p, ty, !lazy);
        }
        let v = self.call_expr(callee, args, ty);
        if lazy || self.dead() {
            return v;
        }
        let t = self.sub(ty);
        let v = match callee {
            hir::Callee::Intrinsic(i) => match self.kept_combinator(*i, v.clone(), t) {
                Some(started) => started,
                None => return v,
            },
            hir::Callee::Def(d, _) if self.cx.is_async_fn(*d) => v,
            hir::Callee::Def(..) => return v,
            _ if matches!(self.kind(ty), TyKind::Promise(..)) => v,
            _ => return v,
        };
        let drop = self.unclaimed_drop_fn(t);
        self.call_rt(Rt::FutStart, vec![v.clone(), drop], None);
        v
    }

    /// `(slot: ptr)` disposing of the unclaimed result of a started promise of type `t`, or null
    /// when there is nothing to do.
    pub(super) fn unclaimed_drop_fn(&mut self, t: TyId) -> Operand {
        if self.cx.promise_error(t).is_some() {
            return cfunc(self.cx.func(Work::Unclaimed(t)));
        }
        let result = self.cx.promise_result(t);
        self.result_drop_fn(result)
            .unwrap_or_else(|| cint(0, Ty::Ptr))
    }

    /// `(slot: ptr)` for a rejecting promise type `t`: drop an `Ok` value, report an `Err` as
    /// uncaught and exit 1.
    pub(in crate::lower) fn build_unclaimed(cx: &mut Cx<'_>, t: TyId) -> Function {
        let slot = cx.promise_slot(t);
        let TyKind::Result(_, err) = cx.kind(slot) else {
            crate::lower::ice("unclaimed result of a promise that cannot reject")
        };
        let sv = cx.ty(slot);
        let mut lw = FnLower::bare(cx, vec![]);
        let sp = lw.new_local(Ty::Ptr, Some("slot".into()));
        let rp = proj(&Place::local(sp), crate::vir::Proj::Deref(sv));
        let tag = Operand::Copy(proj(&rp, Proj::Field(0)));
        let is_err = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb) = (lw.new_block(), lw.new_block());
        lw.branch(is_err, err_bb, ok_bb);
        lw.switch_to(err_bb);
        let ev = lw.cx.view(slot, 1);
        let ep = proj(&proj(&rp, Proj::Cast(ev)), Proj::Field(1));
        lw.report_uncaught(&ep, err);
        lw.drop_glue(ep, err);
        lw.call_rt(Rt::Exit, vec![cint(1, Ty::I32)], None);
        lw.switch_to(ok_bb);
        lw.drop_glue(rp, slot);
        lw.terminate(Terminator::Return(unit()));
        lw.finish(format!("_Gunclaimed_{}", t.0), vec![Ty::Ptr], Ty::Unit)
    }
}
