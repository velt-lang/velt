//! `__intrinsic_http_handler(async (raw: u64): Promise<u64> => …)` (rt_abi_async.md §7): the
//! runtime's `VeltHandler { init, poll, drop, state_size, state_align, env }`, as the 6 × `u64`
//! tuple std/http passes to `velt_rt_http_serve` (the same 48 bytes).
//!
//! The closure's own state machine is the per-request state (result: the `VeltResp*` at offset
//! 0); `poll`/`drop` are its `$poll`/`$drop`, and `init(env, req, state)` writes its initial
//! state with `raw = req`. The environment is a heap box the runtime shares among concurrent
//! requests (on several worker threads) and frees with its drop function (first word) after
//! the server is closed and its last request finished. Requests therefore *borrow* their
//! captures from it: the state machine is lowered with owned captures in `Borrow` mode
//! ([`Cx::shared_envs`]), so `init` stores pointers into the environment (aggregates) or the
//! captured scalars and nothing is copied per request. A capture the body moves out of (wholly
//! or in part) is cloned into each request's state instead. Sema guarantees that the body never
//! mutates its captures, so the sharing is race-free.

use velt_sema::hir::{self, DefId, PassMode, TyId};

use super::super::flags::FlagScan;
use crate::lower::operand::proj;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, Work};
use crate::vir::{Function, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// Capture modes of handler closure `def` as its state machine uses them (module docs).
    fn handler_capture_modes(&self, def: DefId) -> Vec<PassMode> {
        let f = self.fn_def(def);
        let consumed = FlagScan::run(self.hir, &f.body).consumed;
        f.captures
            .iter()
            .map(|c| match c.mode {
                PassMode::Borrow | PassMode::BorrowMut => {
                    ice("an http handler closure borrows a variable of its creator")
                }
                PassMode::Owned if !consumed[c.inner.0 as usize] => PassMode::Borrow,
                m => m,
            })
            .collect()
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// `HttpHandler(f)` of type `ty` (the 6-tuple).
    pub(in crate::lower) fn http_handler(&mut self, f: &hir::Expr, ty: TyId) -> Operand {
        let hir::ExprKind::Closure(def) = f.kind else {
            ice("`__intrinsic_http_handler` needs an async closure literal")
        };
        let fd = self.cx.fn_def(def);
        if !fd.is_async || fd.throws.is_some() {
            ice("an http handler must be a non-throwing async closure");
        }
        let targs = self.targs.clone();
        let modes = self.cx.handler_capture_modes(def);
        self.cx.shared_envs.insert((def, targs.clone()), modes);
        let info = self
            .cx
            .async_info(def, &targs)
            .unwrap_or_else(|| ice("http handler state layout unavailable"));
        let env = self.handler_env(def);
        let init = self.cx.func(Work::HandlerInit(def, targs.clone()));
        let drop = self.cx.func(Work::AsyncDrop(def, targs));
        let (size, align) = self.cx.size_align(Ty::Agg(info.state));
        let mut ops = vec![];
        for p in [cfunc(init), cfunc(info.poll), cfunc(drop)] {
            ops.push(self.cast_to(p, Ty::Ptr, Ty::U64));
        }
        ops.push(cint(size as i128, Ty::U64));
        ops.push(cint(align as i128, Ty::U64));
        ops.push(self.cast_to(env, Ty::Ptr, Ty::U64));
        let ty = self.sub(ty);
        match self.cx.ty(ty) {
            Ty::Agg(a) if self.cx.aggs[a.0 as usize].fields.len() == ops.len() => {
                self.rvalue_temp(Ty::Agg(a), Rvalue::Aggregate(a, ops))
            }
            _ => ice("`__intrinsic_http_handler` must have type [u64; 6]"),
        }
    }

    /// The handler's environment: a heap box holding the captures, headed by its drop function,
    /// which the runtime calls once a closed server's last request has finished.
    fn handler_env(&mut self, def: DefId) -> Operand {
        if self.cx.fn_def(def).captures.is_empty() {
            return cint(0, Ty::Ptr);
        }
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let env = self.alloc(Ty::Agg(ea));
        let drop = cfunc(self.cx.func(Work::EnvDrop(def, targs)));
        self.fill_env(def, env.clone(), drop, cint(0, Ty::Ptr));
        env
    }

    /// `init(env, req, state)` of handler closure `def<targs>` (module docs).
    pub(in crate::lower) fn build_handler_init(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("http handler state layout unavailable"));
        let modes = cx
            .shared_envs
            .get(&(def, targs.to_vec()))
            .cloned()
            .unwrap_or_else(|| ice("http handler without capture modes"));
        let f = cx.fn_def(def);
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let req = lw.new_local(Ty::Ptr, Some("req".into()));
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let mut vals = vec![];
        for p in &f.params {
            let v = match f.captures.iter().position(|c| c.inner == p.local) {
                Some(k) => lw.shared_capture(def, env, k, modes[k]),
                None => match lw.vty(p.ty) {
                    Ty::Unit => None,
                    vt => Some(lw.cast_to(Operand::Copy(Place::local(req)), Ty::Ptr, vt)),
                },
            };
            vals.push(v);
        }
        let dst = Place {
            local: st,
            proj: vec![Proj::Deref(Ty::Agg(info.state))],
        };
        lw.init_state(&info, &dst, vals);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("{}$init", lw.cx.instance_symbol(&f.name, targs));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr, Ty::Ptr], Ty::Unit)
    }

    /// Capture `k` as one request's state stores it, read from the shared env: a pointer into
    /// the env (borrowed aggregates), the value (scalars, Copy), or a clone (moved captures).
    fn shared_capture(
        &mut self,
        def: DefId,
        env: Local,
        k: usize,
        mode: PassMode,
    ) -> Option<Operand> {
        let c = self.cx.fn_def(def).captures[k];
        let local_ty = self.cx.fn_def(def).body.locals[c.inner.0 as usize].ty;
        let ty = self.sub(local_ty);
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            return None;
        }
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let slot = proj(&base, Proj::Field(2 + k as u32));
        Some(match (mode, vt) {
            (PassMode::Borrow, Ty::Agg(_)) => self.addr(slot),
            (PassMode::Owned, _) => self.clone_value(Operand::Copy(slot), ty),
            _ => Operand::Copy(slot),
        })
    }
}
