//! Creating async states outside their poll function:
//! - [`init_state`](FnLower::init_state): write the tag and the inputs into a state place
//!   (embedded awaits, spawns, `async main`, boxed constructors);
//! - `f` itself ([`Work::Fn`] of an async def) with the ordinary calling convention: builds the
//!   state from its arguments and boxes it (`velt_rt_fut_box`) — a promise *value*. For async
//!   closures it is the closure's `code`, called with the borrow ABI: every call's state gets
//!   its own clone of the owned captures (an async closure may be called many times — e.g. a
//!   request handler — and its promises may outlive the closure); an owned argument is another
//!   reference to the caller's value (a share, as for a direct async call: the callee sees the
//!   caller's object, #196);
//! - `f$drop` ([`Work::AsyncDrop`]): set `DROP_BIT` in the tag and run the poll function.

use std::collections::HashMap;

use velt_sema::hir::{self, DefId, LocalId, PassMode, TyId};

use super::{AsyncInfo, CLOSE_BIT, DROP_BIT};
use crate::lower::closure::ENV_HEADER;
use crate::lower::operand::proj;
use crate::lower::{cint, ice, unit, Cx, FnLower};
use crate::vir::{self, BinOp, Function, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    /// A fresh (unpolled) state at `dst`: tag 0 and every stored input.
    pub(in crate::lower) fn init_state(
        &mut self,
        info: &AsyncInfo,
        dst: &Place,
        vals: Vec<Option<Operand>>,
    ) {
        self.assign(proj(dst, Proj::Field(0)), Rvalue::Use(cint(0, Ty::U32)));
        for (i, v) in vals.into_iter().enumerate() {
            if let (Some(Some(field)), Some(v)) = (info.inputs.get(i), v) {
                self.store(proj(dst, Proj::Field(*field)), v);
            }
        }
    }

    /// The boxed constructor of async function `def<targs>` (see module docs).
    pub(in crate::lower) fn build_async_new(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let (mut lw, params, info, s, _) = Self::ctor_state(cx, def, targs, false);
        let form = lw.value_future(def, targs, &info, s, false);
        let fut = lw.box_future(form);
        lw.terminate(Terminator::Return(fut));
        let sym = lw.cx.instance_symbol(&lw.cx.fn_def(def).name, targs);
        lw.finish(sym, params, Ty::Ptr)
    }

    /// A constructor of `def<targs>`'s state machine (async function or generator) with the
    /// ordinary calling convention: its VIR params, the layout, a local holding the initial
    /// state built from the arguments, and (with `out`) the trailing out-pointer param, which
    /// the caller adds to the params.
    pub(in crate::lower) fn ctor_state(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
        out: bool,
    ) -> (Self, Vec<Ty>, AsyncInfo, Local, Option<Local>) {
        let f = cx.fn_def(def);
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("async state layout unavailable"));
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let closure = !f.captures.is_empty();
        let mut params = vec![];
        let env = closure.then(|| {
            params.push(Ty::Ptr);
            lw.new_local(Ty::Ptr, Some("env".into()))
        });
        let caps: HashMap<LocalId, usize> = f
            .captures
            .iter()
            .enumerate()
            .map(|(k, c)| (c.inner, k))
            .collect();
        let mut incoming = vec![];
        for p in &f.params {
            if caps.contains_key(&p.local) {
                incoming.push(None);
                continue;
            }
            let ty = lw.sub(p.ty);
            let vt = lw.cx.ty(ty);
            let pt = if let Ty::Agg(_) = vt { Ty::Ptr } else { vt };
            let l = (vt != Ty::Unit).then(|| {
                params.push(pt);
                lw.new_local(pt, None)
            });
            incoming.push(l.map(|l| (l, vt, ty, p.mode)));
        }
        let out = out.then(|| lw.new_local(Ty::Ptr, Some("ret.out".into())));
        let mut vals = vec![];
        for (p, inc) in f.params.iter().zip(incoming) {
            vals.push(match (caps.get(&p.local), inc, env) {
                (Some(&k), _, Some(env)) => lw.take_capture(def, env, k),
                (None, Some((l, vt, ty, mode)), _) => {
                    Some(lw.incoming_input(l, vt, ty, mode, closure))
                }
                _ => None,
            });
        }
        let s = lw.temp(Ty::Agg(info.state));
        lw.init_state(&info, &Place::local(s), vals);
        (lw, params, info, s, out)
    }

    /// Capture `k` of async closure `def`, taken from the environment for the state.
    fn take_capture(&mut self, def: DefId, env: Local, k: usize) -> Option<Operand> {
        let f = self.cx.fn_def(def);
        let c = f.captures[k];
        let ty = self.sub(f.body.locals[c.inner.0 as usize].ty);
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            return None;
        }
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let slot = proj(&base, Proj::Field(ENV_HEADER + k as u32));
        Some(match c.mode {
            PassMode::Borrow | PassMode::BorrowMut => match vt {
                Ty::Agg(_) => Operand::Copy(slot),
                s => {
                    let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(slot)));
                    let pp = self.operand_place(p, Ty::Ptr);
                    Operand::Copy(proj(&pp, Proj::Deref(s)))
                }
            },
            PassMode::Copy => Operand::Copy(slot),
            // A resource that cannot be copied is shared with the state instead (#122): the
            // call runs on the task that owns the environment, and a call spawned through a
            // function value works on a copy of the closure made for the task (callee.rs
            // `call_indirect`), whose transfer rejects a resource still referenced here.
            PassMode::Owned if self.cx.uncopyable(ty) => self.share_value(Operand::Copy(slot), ty),
            PassMode::Owned => self.clone_value(Operand::Copy(slot), ty),
        })
    }

    /// An incoming argument (param local `l`) as the value the state stores. Under the borrow
    /// ABI (async closures) the caller keeps its arguments, so owned ones are shared (a spawned
    /// call passes copies, transfer.rs).
    fn incoming_input(
        &mut self,
        l: Local,
        vt: Ty,
        ty: TyId,
        mode: PassMode,
        borrowed: bool,
    ) -> Operand {
        let value = match vt {
            Ty::Agg(_) => Operand::Copy(Place {
                local: l,
                proj: vec![Proj::Deref(vt)],
            }),
            _ => Operand::Copy(Place::local(l)),
        };
        match mode {
            PassMode::Borrow | PassMode::BorrowMut if matches!(vt, Ty::Agg(_)) => {
                Operand::Copy(Place::local(l))
            }
            PassMode::Owned if borrowed && self.cx.needs_drop(ty) => self.share_value(value, ty),
            _ => value,
        }
    }

    /// [`Work::AsyncDrop`]: set `DROP_BIT` in the tag and run the poll function.
    pub(in crate::lower) fn build_async_drop(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        Self::build_tag_bit(cx, def, targs, DROP_BIT, "drop")
    }

    /// [`Work::AsyncCloseStart`]: set `CLOSE_BIT` in the tag; the closer then polls.
    pub(in crate::lower) fn build_close_start(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        Self::build_tag_bit(cx, def, targs, CLOSE_BIT, "close")
    }

    /// `f$<what>(state)`: set `bit` in the tag; for `DROP_BIT` also run the poll function.
    fn build_tag_bit(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
        bit: i128,
        what: &str,
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("async state layout unavailable"));
        let name = cx.fn_def(def).name.clone();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let tag = Place {
            local: st,
            proj: vec![Proj::Deref(Ty::Agg(info.state)), Proj::Field(0)],
        };
        let t = lw.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::BitOr, Operand::Copy(tag.clone()), cint(bit, Ty::U32)),
        );
        lw.assign(tag, Rvalue::Use(t));
        if bit == DROP_BIT {
            let d = lw.temp(Ty::U32);
            let args = vec![Operand::Copy(Place::local(st)), cint(0, Ty::Ptr)];
            lw.call(
                vir::Callee::Func(info.poll),
                args,
                Some(Place::local(d)),
                false,
            );
        }
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("{}${what}", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }

    /// Initial state of a call `def<targs>(args)` in a fresh local (for spawning), unless the
    /// layout is unavailable (recursion).
    pub(in crate::lower) fn state_from_call(
        &mut self,
        def: DefId,
        targs: &[TyId],
        args: &[hir::Expr],
    ) -> Option<(AsyncInfo, Local)> {
        let info = self.cx.async_info(def, targs)?;
        let modes: Vec<PassMode> = self.cx.fn_def(def).params.iter().map(|p| p.mode).collect();
        let vals = self.async_args(args, &modes);
        let s = self.temp(Ty::Agg(info.state));
        self.init_state(&info, &Place::local(s), vals);
        Some((info, s))
    }

    /// Initial state of an async closure literal `def` taking no arguments, filled directly from
    /// the captured variables (owned captures are moved in).
    pub(in crate::lower) fn state_from_closure(
        &mut self,
        def: DefId,
    ) -> Option<(AsyncInfo, Local)> {
        let targs = self.targs.clone();
        let info = self.cx.async_info(def, &targs)?;
        let f = self.cx.fn_def(def);
        let mut vals = vec![];
        for p in &f.params {
            let cap = f.captures.iter().find(|c| c.inner == p.local);
            let v = cap.and_then(|c| {
                let outer = self.local_target(c.outer)?;
                let ty = self.info[c.outer.0 as usize].ty;
                Some(match c.mode {
                    PassMode::Borrow | PassMode::BorrowMut => match self.cx.ty(ty) {
                        Ty::Agg(_) => self.addr(outer),
                        _ => Operand::Copy(outer),
                    },
                    // Like a closure environment (closure.rs `build_env`): a shared capture is
                    // another reference, and a value entering a spawned task is transferred.
                    PassMode::Owned if c.share => {
                        let v = self.share_value(Operand::Copy(outer), ty);
                        self.maybe_transfer(v, ty)
                    }
                    PassMode::Owned => self.maybe_transfer(Operand::Copy(outer), ty),
                    _ => Operand::Copy(outer),
                })
            });
            if let Some(c) = cap.filter(|c| c.mode == PassMode::Owned && !c.share) {
                self.mark_moved(c.outer);
            }
            vals.push(v);
        }
        let s = self.temp(Ty::Agg(info.state));
        self.init_state(&info, &Place::local(s), vals);
        Some((info, s))
    }
}
