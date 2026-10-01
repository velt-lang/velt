//! `await Promise.all([a, b, …])` of an array literal, compiled inline in the poll function
//! (rt_abi_async.md §1): no `velt_rt_all`, and no boxing of direct calls.
//!
//! - The elements are evaluated first, in order. When the promises cannot reject, a direct call
//!   of a compiled async function becomes a child state embedded in this state; any other
//!   promise is taken as a heap future. When they can reject, every child is a heap future:
//!   on an early rejection the others must keep running (JS), which an embedded state can't.
//! - One suspension: its resume block polls every child whose `done` flag is still clear (a
//!   child is never polled after READY) and suspends while any is pending.
//! - Promises that can reject: the first pass runs every child up to its first suspension (the
//!   calls of the literal, in JS), then the first `Err` in array order rejects the join. After
//!   that, a child that finishes with an `Err` rejects right after its poll, before the rest of
//!   the pass polls its siblings (in JS the rejection handler runs before the next woken
//!   promise). The other finished results are dropped, and the pending children are handed to
//!   the task (`velt_rt_fut_detach`), handled, so they run to completion quietly after the
//!   handler (rt_abi_async.md §1.1).
//! - Once all are done, the results move into a fresh `T[]` (heap futures are freed).
//! - Its cancel block drops the pending children, the results of the finished ones, and
//!   everything the scopes own.

use velt_sema::hir::{self, DefId, PassMode, TyId};

use super::AsyncInfo;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower, Work};
use crate::vir::{self, BinOp, Const, Local, Operand, Place, Proj, Rvalue, Ty};

/// One child of an inline `Promise.all`.
#[derive(Clone)]
enum Child {
    /// Embedded state of a compiled async call (its `$poll`/`$drop`).
    State {
        def: DefId,
        targs: Vec<TyId>,
        info: AsyncInfo,
        local: Local,
    },
    /// Owned heap future (`VeltFut*`).
    Heap(Local),
}

impl FnLower<'_, '_> {
    /// `await Promise.all(elems)`; `pty` is the joined promise type `Promise<T[], E>`.
    pub(super) fn await_all_inline(&mut self, elems: &[hir::Expr], pty: TyId) -> Operand {
        let arr_ty = self.cx.promise_result(pty);
        let err = self.cx.promise_error(pty);
        let elem = self.elem_ty(arr_ty);
        // What a heap child leaves in its slot: `Result<T, E>` when it can reject.
        let slot = match err {
            Some(e) => self.cx.intern(velt_sema::hir::TyKind::Result(elem, e)),
            None => elem,
        };
        let embed = err.is_none();
        let children: Vec<Child> = elems.iter().map(|e| self.all_child(e, embed)).collect();
        if self.dead() {
            return unit();
        }
        let done: Vec<Local> = children
            .iter()
            .map(|_| {
                let d = self.temp(Ty::Bool);
                self.assign(Place::local(d), Rvalue::Use(Self::cbool(false)));
                d
            })
            .collect();
        // Set during the first pass, which runs every child up to its first suspension (the
        // calls of the array literal, in JS) before a rejection is taken.
        let first = err.map(|_| {
            let f = self.temp(Ty::Bool);
            self.assign(Place::local(f), Rvalue::Use(Self::cbool(true)));
            f
        });
        let (k, resume) = self.suspension();
        self.goto(resume);
        self.switch_to(resume);
        let mut all_done = Self::cbool(true);
        for (i, (c, &d)) in children.iter().zip(&done).enumerate() {
            self.poll_unless_done(c, d);
            if let (Some(e), Some(f)) = (err, first) {
                let (check, next) = (self.new_block(), self.new_block());
                self.branch(Operand::Copy(Place::local(f)), next, check);
                self.switch_to(check);
                self.reject_if_err(i, &children, &done, elem, e);
                self.goto(next);
                self.switch_to(next);
            }
            let both = Rvalue::Binary(BinOp::BitAnd, all_done, Operand::Copy(Place::local(d)));
            all_done = self.rvalue_temp(Ty::Bool, both);
        }
        if let (Some(e), Some(f)) = (err, first) {
            let (check, next) = (self.new_block(), self.new_block());
            self.branch(Operand::Copy(Place::local(f)), check, next);
            self.switch_to(check);
            self.assign(Place::local(f), Rvalue::Use(Self::cbool(false)));
            for i in 0..children.len() {
                self.reject_if_err(i, &children, &done, elem, e);
            }
            self.goto(next);
            self.switch_to(next);
        }
        let ready = self.rvalue_temp(Ty::U32, Rvalue::Cast(all_done, Ty::U32));
        let (cs, ds) = (children.clone(), done.clone());
        self.after_poll(k, ready, move |lw| lw.cancel_children(&cs, &ds, elem, slot));
        self.collect_results(&children, arr_ty, elem, err)
    }

    /// If child `i` finished with an `Err`, reject now (see the module docs). Continues where it
    /// didn't.
    fn reject_if_err(
        &mut self,
        i: usize,
        children: &[Child],
        done: &[Local],
        elem: TyId,
        err: TyId,
    ) {
        let c = &children[i];
        let Child::Heap(f) = c else { return };
        let rty = self.cx.intern(velt_sema::hir::TyKind::Result(elem, err));
        let rv = self.cx.ty(rty);
        let (check, next, err_bb) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(Operand::Copy(Place::local(done[i])), check, next);
        self.switch_to(check);
        let res = self.child_result(c, rv);
        let tag = Operand::Copy(proj(&res, Proj::Field(0)));
        let is_err = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        self.branch(is_err, err_bb, next);
        self.switch_to(err_bb);
        let own = self.copy_to_temp(Operand::Copy(res), rv);
        self.call_rt(Rt::FutDrop, vec![Operand::Copy(Place::local(*f))], None);
        for (j, (other, &od)) in children.iter().zip(done).enumerate() {
            if j != i {
                self.release_sibling(other, od, elem, rty);
            }
        }
        let ev = self.cx.view(rty, 1);
        let e = Operand::Copy(proj(
            &proj(&Place::local(own), Proj::Cast(ev)),
            Proj::Field(1),
        ));
        self.record_throw_loc();
        self.route_error(e, err);
        self.switch_to(next);
    }

    /// A sibling of the rejected child: a finished one's result is dropped; a pending one keeps
    /// running (started on this task if it was lazy) with its outcome handled quietly.
    fn release_sibling(&mut self, c: &Child, done: Local, elem: TyId, rty: TyId) {
        let Child::Heap(f) = c else {
            crate::lower::ice("embedded child of a Promise.all that can reject")
        };
        let fp = Operand::Copy(Place::local(*f));
        let (fin, pend, next) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(Operand::Copy(Place::local(done)), fin, pend);
        self.switch_to(fin);
        self.drop_child_result(c, elem, rty);
        self.call_rt(Rt::FutDrop, vec![fp.clone()], None);
        self.goto(next);
        self.switch_to(pend);
        let quiet = self.result_drop_fn(rty).unwrap_or_else(|| cint(0, Ty::Ptr));
        // A lazy child joins the task's started promises and runs after this continuation; a
        // started one (a stored `p` in the literal) keeps running; both are handled.
        self.call_rt(Rt::FutDetach, vec![fp, quiet], None);
        self.goto(next);
        self.switch_to(next);
    }

    /// Drop the (finished) result of child `c`: an embedded state's `T`, a heap child's slot.
    fn drop_child_result(&mut self, c: &Child, elem: TyId, rty: TyId) {
        let t = match c {
            Child::State { .. } => elem,
            Child::Heap(_) => rty,
        };
        let vt = self.cx.ty(t);
        if vt != Ty::Unit {
            let p = self.child_result(c, vt);
            self.drop_glue(p, t);
        }
    }

    fn cbool(b: bool) -> Operand {
        Operand::Const(Const::Bool(b), Ty::Bool)
    }

    /// Evaluate one element into a child (embedded state, if `embed` allows it, or heap future).
    fn all_child(&mut self, e: &hir::Expr, embed: bool) -> Child {
        if let hir::ExprKind::Call {
            callee: hir::Callee::Def(d, targs),
            args,
        } = &e.kind
        {
            let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
            let embeddable = embed && self.cx.is_async_fn(*d);
            if let Some(info) = embeddable.then(|| self.cx.async_info(*d, &targs)).flatten() {
                let modes: Vec<PassMode> =
                    self.cx.fn_def(*d).params.iter().map(|p| p.mode).collect();
                let vals = self.async_args(args, &modes);
                let local = self.temp(Ty::Agg(info.state));
                if !self.dead() {
                    self.init_state(&info, &Place::local(local), vals);
                }
                return Child::State {
                    def: *d,
                    targs,
                    info,
                    local,
                };
            }
        }
        let fut = self.take_promise(e);
        let l = self.temp(Ty::Ptr);
        if !self.dead() {
            self.assign(Place::local(l), Rvalue::Use(fut));
        }
        Child::Heap(l)
    }

    /// `if (!done) { done = poll(child) != 0 }`.
    fn poll_unless_done(&mut self, c: &Child, done: Local) {
        let (poll_bb, next) = (self.new_block(), self.new_block());
        self.branch(Operand::Copy(Place::local(done)), next, poll_bb);
        self.switch_to(poll_bb);
        let r = self.temp(Ty::U32);
        let cx = self.poll_cx();
        match c {
            Child::State { info, local, .. } => {
                let sp = self.addr(Place::local(*local));
                let callee = vir::Callee::Func(info.poll);
                self.call(callee, vec![sp, cx], Some(Place::local(r)), false);
            }
            Child::Heap(f) => {
                let fp = Operand::Copy(Place::local(*f));
                self.call_rt(Rt::FutPoll, vec![fp, cx], Some(Place::local(r)));
            }
        }
        let ready = Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(r)), cint(0, Ty::U32));
        self.assign(Place::local(done), ready);
        self.goto(next);
        self.switch_to(next);
    }

    /// Where child `c`'s result lives once it is done (state offset 0, or heap future `+16`).
    fn child_result(&mut self, c: &Child, vt: Ty) -> Place {
        let ptr = match c {
            Child::State { local, .. } => self.addr(Place::local(*local)),
            Child::Heap(f) => self.rvalue_temp(
                Ty::Ptr,
                Rvalue::Binary(
                    BinOp::PtrAdd,
                    Operand::Copy(Place::local(*f)),
                    cint(16, Ty::U64),
                ),
            ),
        };
        let base = self.operand_place(ptr, Ty::Ptr);
        proj(&base, Proj::Deref(vt))
    }

    /// Cancel block body: drop pending children and the results of finished ones (`slot`: the
    /// result type a heap child leaves).
    fn cancel_children(&mut self, children: &[Child], done: &[Local], elem: TyId, slot: TyId) {
        for (c, &d) in children.iter().zip(done) {
            let (fin, pend, next) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(Operand::Copy(Place::local(d)), fin, pend);
            self.switch_to(fin);
            let t = match c {
                Child::State { .. } => elem,
                Child::Heap(_) => slot,
            };
            let vt = self.cx.ty(t);
            if vt != Ty::Unit {
                let p = self.child_result(c, vt);
                self.drop_glue(p, t);
            }
            if let Child::Heap(f) = c {
                self.call_rt(Rt::FutDrop, vec![Operand::Copy(Place::local(*f))], None);
            }
            self.goto(next);
            self.switch_to(pend);
            match c {
                Child::State {
                    def, targs, local, ..
                } => {
                    let drop_fn = self.cx.func(Work::AsyncDrop(*def, targs.clone()));
                    let sp = self.addr(Place::local(*local));
                    self.call(vir::Callee::Func(drop_fn), vec![sp], None, false);
                }
                Child::Heap(f) => {
                    self.call_rt(Rt::FutDrop, vec![Operand::Copy(Place::local(*f))], None);
                }
            }
            self.goto(next);
            self.switch_to(next);
        }
    }

    /// All children are done: move the results into a fresh `T[]` (an owned temporary).
    /// (`err`: heap children hold a `Result<T, err>` whose `Ok` payload moves.)
    fn collect_results(
        &mut self,
        children: &[Child],
        arr_ty: TyId,
        elem: TyId,
        err: Option<TyId>,
    ) -> Operand {
        let n = cint(children.len() as i128, Ty::U64);
        let arr = self.inline_array_with_len(n, elem);
        let ap = Place::local(arr);
        let vt = self.cx.ty(elem);
        for (i, c) in children.iter().enumerate() {
            if vt != Ty::Unit {
                let src = match (c, err) {
                    (Child::Heap(_), Some(e)) => {
                        let rty = self.cx.intern(velt_sema::hir::TyKind::Result(elem, e));
                        let rv = self.cx.ty(rty);
                        let ov = self.cx.view(rty, 0);
                        proj(
                            &proj(&self.child_result(c, rv), Proj::Cast(ov)),
                            Proj::Field(1),
                        )
                    }
                    _ => self.child_result(c, vt),
                };
                let dst = self.elem_place(&ap, cint(i as i128, Ty::U64), elem);
                self.assign(dst, Rvalue::Use(Operand::Copy(src)));
            }
            if let Child::Heap(f) = c {
                self.call_rt(Rt::FutDrop, vec![Operand::Copy(Place::local(*f))], None);
            }
        }
        self.own_array(arr, arr_ty)
    }
}
