//! Async functions as state machines (docs/internals/contracts/rt_abi_async.md §1).
//!
//! An async function instance `f` becomes:
//! - `f$poll(state: ptr, cx: ptr) -> u32` ([`Work::Poll`]): the body lowered once, like a sync
//!   function, with its result written at `state + 0` and every `await` split into
//!   "poll the child; if pending, remember where we are and return 0" plus a *resume block*.
//!   The entry block switches on the state's tag: `0` starts the body, `k` resumes after
//!   suspension `k`, `DROP_BIT | k` runs the *cancel block* of suspension `k` (drops the pending
//!   child and every live value, without running `finally` code), anything else returns 0.
//! - `f$drop(state: ptr)` ([`Work::AsyncDrop`]): sets `DROP_BIT` in the tag and calls the poll
//!   function, so cancellation reuses the scope/drop-flag machinery of the body.
//! - `f` itself ([`Work::Fn`]) with the ordinary calling convention: builds the initial state
//!   from its arguments and boxes it with `velt_rt_fut_box` (a promise *value*).
//!
//! The state struct is not designed up front: after the poll function is built, spill.rs runs a
//! liveness analysis on it and moves every VIR local that is live at function entry (i.e. live
//! across some suspension: params, drop flags, locals, temporaries, embedded child states)
//! into the state; everything else stays a VIR local. Its layout is `[result @0] tag: u32,
//! spilled…`; field 0 is the tag, and the result region is addressed directly at offset 0.
//!
//! `await` of a direct call to a compiled async function embeds the child's state in the
//! parent's (no allocation); other awaits poll heap futures (suspend.rs). Task/promise
//! intrinsics are in tasks.rs, shared-state intrinsics in sync.rs, http handlers in handler.rs.

mod all;
mod all_settle;
mod ctor;
mod handler;
mod kept;
mod spill;
mod start;
mod suspend;
mod sync;
mod tasks;
mod value;

use std::collections::HashMap;

use velt_sema::hir::{self, DefId, FnDef, PassMode, TyId, TyKind};

use super::flags::FlagScan;
use super::{cint, Cx, FnLower, LInfo, LState, Work};
use crate::vir::{
    self, AggId, BlockId, FuncId, Local, Operand, Place, Proj, Rvalue, Terminator, Ty,
};

/// Aggregate id used for the state struct while its layout is unknown (patched by spill.rs).
const PLACEHOLDER: AggId = AggId(u32::MAX);
/// Tag bit that turns a resume into a cancellation (set by the drop function).
const DROP_BIT: i128 = 0x8000_0000;
/// Tag of a finished state machine (never resumed; dropping it is a no-op).
const DONE: i128 = 0x7FFF_FFFF;

/// Layout facts of a compiled async function instance, known once its poll function is built.
#[derive(Clone, Debug)]
pub(super) struct AsyncInfo {
    pub poll: FuncId,
    pub state: AggId,
    /// Per `FnDef::params` entry (captures first): the state field holding it, if stored.
    pub inputs: Vec<Option<u32>>,
    /// VIR type of the result region at offset 0 (`Result<T, E>` for throwing functions).
    pub result: Ty,
}

/// Per-function state while lowering a poll function.
pub(super) struct AsyncCx {
    state: Local,
    cx: Local,
    /// Entry dispatch: (tag, block).
    cases: Vec<(i128, BlockId)>,
    next_tag: i128,
}

impl Cx<'_> {
    /// The awaited result type `T` of an async function (`ret` is `Promise<T>`, or `T`).
    pub(super) fn async_result(&self, f: &FnDef) -> TyId {
        match self.types.kind(f.ret) {
            TyKind::Promise(t, _) => *t,
            _ => f.ret,
        }
    }

    /// Layout of `def<targs>`'s state, building its poll function now if needed. `None` while
    /// that poll function is itself being built (a recursive await), so callers box instead.
    pub(super) fn async_info(&mut self, def: DefId, targs: &[TyId]) -> Option<AsyncInfo> {
        let key = (def, targs.to_vec());
        if let Some(i) = self.asyncs.get(&key) {
            return Some(i.clone());
        }
        let work = Work::Poll(def, targs.to_vec());
        let fid = self.func(work.clone());
        self.build_now(fid, &work);
        self.asyncs.get(&key).cloned()
    }

    /// Result type and thrown type of a *call* of `f`: an async function's call yields
    /// `Promise<T, E>` and never throws (its errors surface at `await`). Unsubstituted.
    pub(super) fn call_sig(&mut self, f: &FnDef) -> (TyId, Option<TyId>) {
        if f.is_async {
            let r = self.async_result(f);
            let never = self.intern(TyKind::Never);
            (
                self.intern(TyKind::Promise(r, f.throws.unwrap_or(never))),
                None,
            )
        } else {
            (f.ret, f.throws)
        }
    }

    /// Is `def` a compiled async function?
    pub(super) fn is_async_fn(&self, def: DefId) -> bool {
        matches!(self.hir.def(def), hir::Def::Fn(f) if f.is_async)
    }

    /// Build `f$poll` and record the state layout in `asyncs`.
    fn poll_fn(&mut self, def: DefId, targs: &[TyId]) -> vir::Function {
        let f = self.fn_def(def);
        let (mut func, inputs, result) = {
            let mut lw = FnLower::bare(self, targs.to_vec());
            let shared = lw.cx.shared_envs.get(&(def, targs.to_vec())).cloned();
            let inputs = lw.lower_poll_body(f, shared);
            let result = lw.result_region();
            let sym = format!("{}$poll", lw.cx.instance_symbol(&f.name, targs));
            (
                lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32),
                inputs,
                result,
            )
        };
        let stored: Vec<Local> = inputs.iter().flatten().copied().collect();
        let (state, fields) = spill::spill(self, &mut func, result, &f.name, &stored);
        func.is_poll = true;
        func.param_attrs = self.poll_param_attrs(state);
        let inputs = inputs
            .into_iter()
            .map(|l| l.and_then(|l| fields.get(&l).copied()))
            .collect();
        let poll = self.func(Work::Poll(def, targs.to_vec()));
        let info = AsyncInfo {
            poll,
            state,
            inputs,
            result,
        };
        self.asyncs.insert((def, targs.to_vec()), info);
        func
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn build_poll(cx: &'c mut Cx<'h>, def: DefId, targs: &[TyId]) -> vir::Function {
        cx.poll_fn(def, targs)
    }

    /// Params `(state, cx)`, the dispatch switch, then the body. Returns the VIR local of each
    /// input (param/capture) for the state layout.
    fn lower_poll_body(&mut self, f: &FnDef, shared: Option<Vec<PassMode>>) -> Vec<Option<Local>> {
        let state = self.new_local(Ty::Ptr, Some("state".into()));
        let cx = self.new_local(Ty::Ptr, Some("cx".into()));
        let result = self.cx.async_result(f);
        self.ret_ty = Some(self.sub(result));
        let targs = self.targs.clone();
        self.throws = self.cx.fn_throws(f, &targs);
        self.out_ptr = Some(state);
        let start = self.new_block();
        self.live[start.0 as usize] = true;
        self.asyncx = Some(AsyncCx {
            state,
            cx,
            cases: vec![(0, start)],
            next_tag: 1,
        });
        self.switch_to(start);
        let scan = FlagScan::run(self.cx.hir, &f.body);
        self.ref_bindings = scan.ref_bindings.iter().copied().collect();
        let inputs = self.declare_async_locals(f, shared);
        self.declare_drop_flags(f, &scan);
        self.lower_body(f);
        self.emit_dispatch();
        inputs
    }

    /// Params and captures are values stored in the state (owned/copied), or pointers for
    /// borrowed aggregates; every other local is declared as in a sync function. `shared`
    /// overrides the capture modes (http handlers borrow from their shared environment).
    fn declare_async_locals(
        &mut self,
        f: &FnDef,
        shared: Option<Vec<PassMode>>,
    ) -> Vec<Option<Local>> {
        let mut info: Vec<Option<LInfo>> = f.body.locals.iter().map(|_| None).collect();
        let modes: HashMap<hir::LocalId, PassMode> = match shared {
            Some(ms) => f.captures.iter().map(|c| c.inner).zip(ms).collect(),
            None => f.captures.iter().map(|c| (c.inner, c.mode)).collect(),
        };
        let mut inputs = vec![];
        for p in &f.params {
            let mode = modes.get(&p.local).copied().unwrap_or(p.mode);
            let name = f.body.locals[p.local.0 as usize].name.clone();
            let ty = self.sub(p.ty);
            let by_ref = matches!(mode, PassMode::Borrow | PassMode::BorrowMut);
            let (vir, indirect) = match self.cx.ty(ty) {
                Ty::Unit => (None, false),
                Ty::Agg(_) if by_ref => (Some(self.new_local(Ty::Ptr, Some(name))), true),
                t => (Some(self.new_local(t, Some(name))), false),
            };
            let droppable = mode == PassMode::Owned && vir.is_some() && self.cx.needs_drop(ty);
            info[p.local.0 as usize] = Some(LInfo::new(vir, ty, indirect, droppable, LState::Init));
            inputs.push(vir);
        }
        self.declare_body_locals(f, info);
        inputs
    }

    /// VIR type of the result region at offset 0 of the state.
    fn result_region(&mut self) -> Ty {
        let ret = self.ret_ty.unwrap_or_else(|| super::ice("ret"));
        match self.throws {
            Some(e) => {
                let r = self.cx.intern(TyKind::Result(ret, e));
                self.cx.ty(r)
            }
            None => self.cx.ty(ret),
        }
    }

    fn actx(&mut self) -> &mut AsyncCx {
        self.asyncx
            .as_mut()
            .unwrap_or_else(|| super::ice("suspension outside a poll function"))
    }

    /// `state.tag` (the state layout is patched in by spill.rs).
    fn tag_place(&mut self) -> Place {
        Place {
            local: self.actx().state,
            proj: vec![Proj::Deref(Ty::Agg(PLACEHOLDER)), Proj::Field(0)],
        }
    }

    /// The `cx` param of the poll function.
    fn poll_cx(&mut self) -> Operand {
        Operand::Copy(Place::local(self.actx().cx))
    }

    fn set_tag(&mut self, tag: i128) {
        let p = self.tag_place();
        self.assign(p, Rvalue::Use(cint(tag, Ty::U32)));
    }

    /// Terminator of a completed body: the result is stored, report READY.
    pub(super) fn finish_poll(&mut self) {
        self.set_tag(DONE);
        self.terminate(Terminator::Return(cint(1, Ty::U32)));
    }

    /// Cancellation before the first poll: drop the owned inputs (their drop flags, if any,
    /// are not initialized yet, but all inputs are owned at that point).
    pub(super) fn cancel_before_start(&mut self, f: &FnDef) {
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        for p in &f.params {
            let info = &self.info[p.local.0 as usize];
            if info.droppable {
                let ty = info.ty;
                let place = self.local_place(p.local);
                self.drop_glue(place, ty);
            }
        }
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
        self.switch_to(saved);
        self.actx().cases.push((DROP_BIT, d));
    }

    /// Fill the entry block: `switch state.tag`.
    fn emit_dispatch(&mut self) {
        self.switch_to(BlockId(0));
        let tp = self.tag_place();
        let tag = self.rvalue_temp(Ty::U32, Rvalue::Use(Operand::Copy(tp)));
        let other = self.new_block();
        let cases = std::mem::take(&mut self.actx().cases);
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: other,
        });
        self.switch_to(other);
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
    }
}
