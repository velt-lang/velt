//! Promise *values* of async functions: the heap-future form of an initial state (boxed
//! promises, spawned tasks). A `Promise<T, E>` value's result at `+16` is the state's result
//! region: `Result<T, E>` when it can reject (awaiting it rethrows the error, suspend.rs), else
//! `T`. A task spawned as a statement (its promise dropped at once) that can reject is wrapped
//! in a small compiled future `{ result: T @0, inner: f state }` whose poll (`Work::ValuePoll`)
//! unwraps `Ok` and turns `Err` into an uncaught-error exit (like an unhandled promise
//! rejection: `Uncaught <Type>[: message]`, exit 1).

use velt_sema::hir::{DefId, TyId, TyKind};

use super::AsyncInfo;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, Work};
use crate::vir::{self, AggId, BinOp, FuncId, Function, Local, Operand, Place, Proj, Rvalue};
use crate::vir::{Terminator, Ty};

impl Cx<'_> {
    /// Wrapper layout of throwing async function `def<targs>` and its inner-state field index.
    fn value_wrap(&mut self, def: DefId, targs: &[TyId], info: &AsyncInfo) -> (AggId, u32) {
        let key = (def, targs.to_vec());
        if let Some(&w) = self.lay.value_wraps.get(&key) {
            return w;
        }
        let r = self.async_result(self.fn_def(def));
        let r = self.subst(r, targs);
        let rt = self.ty(r);
        let mut tys = vec![];
        if rt != Ty::Unit {
            tys.push(rt);
        }
        tys.push(Ty::Agg(info.state));
        let name = format!("{} promise", self.fn_def(def).name);
        let w = (self.new_agg(name, &tys), tys.len() as u32 - 1);
        self.lay.value_wraps.insert(key, w);
        w
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The heap-future form of the initial state in local `s`: (poll, drop, state local);
    /// `detached`: nobody can await it, so an error is reported as uncaught.
    pub(super) fn value_future(
        &mut self,
        def: DefId,
        targs: &[TyId],
        info: &AsyncInfo,
        s: Local,
        detached: bool,
    ) -> (FuncId, FuncId, Local) {
        let throws = self.cx.fn_throws(self.cx.fn_def(def), targs);
        if throws.is_none() || !detached {
            let drop = self.cx.func(Work::AsyncDrop(def, targs.to_vec()));
            return (info.poll, drop, s);
        }
        let (wa, inner) = self.cx.value_wrap(def, targs, info);
        let w = self.temp(Ty::Agg(wa));
        let st = Operand::Copy(Place::local(s));
        self.assign(proj(&Place::local(w), Proj::Field(inner)), Rvalue::Use(st));
        let poll = self.cx.func(Work::ValuePoll(def, targs.to_vec()));
        let drop = self.cx.func(Work::ValueDrop(def, targs.to_vec()));
        (poll, drop, w)
    }

    /// Box a heap-future form (see [`value_future`](Self::value_future)) with `velt_rt_fut_box`.
    pub(super) fn box_future(&mut self, (poll, drop, s): (FuncId, FuncId, Local)) -> Operand {
        let st = self.locals[s.0 as usize].ty;
        let (size, align) = self.cx.size_align(st);
        let a = self.addr(Place::local(s));
        let d = self.temp(Ty::Ptr);
        let args = vec![
            cfunc(poll),
            cfunc(drop),
            a,
            cint(size as i128, Ty::U64),
            cint(align as i128, Ty::U64),
        ];
        self.call_rt(Rt::FutBox, args, Some(Place::local(d)));
        Operand::Copy(Place::local(d))
    }

    /// `(w, cx) -> u32`: poll the inner state; `Ok(v)` → `v` at `w + 0`; `Err(e)` → uncaught.
    pub(in crate::lower) fn build_value_poll(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("async state layout unavailable"));
        let (wa, inner) = cx.value_wrap(def, targs, &info);
        let f = cx.fn_def(def);
        let ret = cx.async_result(f);
        let ret = cx.subst(ret, targs);
        let err = cx
            .fn_throws(f, targs)
            .unwrap_or_else(|| ice("not throwing"));
        let name = f.name.clone();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let w = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let base = proj(&Place::local(w), Proj::Deref(Ty::Agg(wa)));
        let sp = lw.addr(proj(&base, Proj::Field(inner)));
        let r = lw.temp(Ty::U32);
        let args = vec![sp.clone(), Operand::Copy(Place::local(cxl))];
        lw.call(
            vir::Callee::Func(info.poll),
            args,
            Some(Place::local(r)),
            false,
        );
        let (pend, ready) = (lw.new_block(), lw.new_block());
        let pending = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(r)), cint(0, Ty::U32)),
        );
        lw.branch(pending, pend, ready);
        lw.switch_to(pend);
        lw.terminate(Terminator::Return(cint(0, Ty::U32)));
        lw.switch_to(ready);
        let rty = lw.cx.intern(TyKind::Result(ret, err));
        let rv = lw.cx.ty(rty);
        let sp = lw.operand_place(sp, Ty::Ptr);
        let res = lw.copy_to_temp(Operand::Copy(proj(&sp, Proj::Deref(rv))), rv);
        let rp = Place::local(res);
        let tag = Operand::Copy(proj(&rp, Proj::Field(0)));
        let is_err = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb) = (lw.new_block(), lw.new_block());
        lw.branch(is_err, err_bb, ok_bb);
        lw.switch_to(err_bb);
        let ev = lw.cx.view(rty, 1);
        let ep = proj(&proj(&rp, Proj::Cast(ev)), Proj::Field(1));
        lw.report_uncaught(&ep, err);
        lw.drop_glue(ep, err);
        lw.call_rt(Rt::Exit, vec![cint(1, Ty::I32)], None);
        lw.switch_to(ok_bb);
        if lw.cx.ty(ret) != Ty::Unit {
            let ov = lw.cx.view(rty, 0);
            let v = Operand::Copy(proj(&proj(&rp, Proj::Cast(ov)), Proj::Field(1)));
            lw.assign(proj(&base, Proj::Field(0)), Rvalue::Use(v));
        }
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!("{}$value_poll", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// `(w)`: cancel the inner state.
    pub(in crate::lower) fn build_value_drop(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("async state layout unavailable"));
        let (wa, inner) = cx.value_wrap(def, targs, &info);
        let drop = cx.func(Work::AsyncDrop(def, targs.to_vec()));
        let name = cx.fn_def(def).name.clone();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let w = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(w), Proj::Deref(Ty::Agg(wa)));
        let sp = lw.addr(proj(&base, Proj::Field(inner)));
        lw.call(vir::Callee::Func(drop), vec![sp], None, false);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("{}$value_drop", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }
}
