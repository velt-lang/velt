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
//!
//! Except through a function value: a sync callback the handler reaches (`i.onChange("x")`,
//! stored in a captured object) may assign a variable it captured itself, and concurrent
//! requests would update that variable's cell at once (#873). So when `serve` builds the
//! environment, the many-threads check also notes, per capture that can reach a function value,
//! whether it reaches captured variables' cells (`velt_rt_saw_cells`); the env's clone word,
//! which a handler environment does not use (it is never a function value), holds those
//! captures as a bit mask (bit `min(k, 63)` for capture `k`). When the mask is not zero, `serve`
//! gets a second `init` (`$copy$init`), which gives each request its own copy of those
//! captures, with cells of its own (`thread_copy`): the copy model the docs describe for
//! everything a handler captured (docs/std/http.md). Copying from the shared environment updates
//! the counts of the cells there, so requests take turns (`velt_rt_copy_lock`). Such a capture
//! that the body only borrows is held by value in the state, with a drop flag `init` sets: the
//! request owns (and drops) it only when it copied it ([`HandlerCaps`]). Handlers whose captures
//! reach no such cells run the plain `init`: no copies, no lock.

use std::collections::HashSet;

use velt_sema::hir::{self, DefId, FnDef, LocalId, PassMode, TyId};

use super::super::flags::FlagScan;
use crate::lower::closure::ENV_HEADER;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, Work};
use crate::vir::{BinOp, Const, Function, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// How a handler closure's state machine holds its captures (module docs).
#[derive(Clone)]
pub(in crate::lower) struct HandlerCaps {
    /// Per capture: `Borrow` (read from the shared environment), `Owned` (cloned per request)
    /// or `Copy`.
    pub(in crate::lower) modes: Vec<PassMode>,
    /// Per capture: can it reach a function value, so that a request copies it when the
    /// environment's mask says so?
    pub(in crate::lower) copied: Vec<bool>,
}

impl HandlerCaps {
    /// The borrowed captures a request may copy, in capture order: the state holds them by
    /// value, and their drop flags are its inputs after the params'.
    pub(in crate::lower) fn flagged<'a>(
        &'a self,
        f: &'a FnDef,
    ) -> impl Iterator<Item = LocalId> + 'a {
        f.captures
            .iter()
            .enumerate()
            .filter(|(k, _)| self.copied[*k] && self.modes[*k] == PassMode::Borrow)
            .map(|(_, c)| c.inner)
    }

    fn any_copied(&self) -> bool {
        self.copied.iter().any(|&c| c)
    }
}

/// The env mask bit of capture `k` (module docs).
fn mask_bit(k: usize) -> i128 {
    1 << k.min(63)
}

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
        let caps = self.handler_caps(def);
        self.cx.shared_envs.insert((def, targs.clone()), caps.clone());
        let info = self
            .cx
            .async_info(def, &targs)
            .unwrap_or_else(|| ice("http handler state layout unavailable"));
        let (env, mask) = self.handler_env(def, &caps);
        let mut init = cfunc(self.cx.func(Work::HandlerInit(def, targs.clone(), false)));
        if let Some(mask) = mask {
            // Requests copy the captures the mask names (module docs).
            let copying = cfunc(self.cx.func(Work::HandlerInit(def, targs.clone(), true)));
            let chosen = self.temp(Ty::Ptr);
            self.assign(Place::local(chosen), Rvalue::Use(init));
            let any = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(mask)), cint(0, Ty::U64)),
            );
            let done = self.new_block();
            self.when(any, done);
            self.assign(Place::local(chosen), Rvalue::Use(copying));
            self.goto(done);
            self.switch_to(done);
            init = Operand::Copy(Place::local(chosen));
        }
        let drop = self.cx.func(Work::AsyncDrop(def, targs));
        let (size, align) = self.cx.size_align(Ty::Agg(info.state));
        let mut ops = vec![];
        for p in [init, cfunc(info.poll), cfunc(drop)] {
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

    /// The capture modes of handler closure `def` and the captures its requests may copy
    /// (module docs): owned in the creator, not a cell, and able to reach a function value.
    fn handler_caps(&mut self, def: DefId) -> HandlerCaps {
        let modes = self.cx.handler_capture_modes(def);
        let f = self.cx.fn_def(def);
        let mut copied = vec![];
        for (c, m) in f.captures.iter().zip(&modes) {
            let local = &f.body.locals[c.inner.0 as usize];
            let candidate = c.mode == PassMode::Owned && !local.boxed && *m != PassMode::Copy;
            let ty = self.sub(local.ty);
            copied.push(
                candidate
                    && self.cx.ty(ty) != Ty::Unit
                    && self.cx.needs_drop(ty)
                    && self.cx.reaches_fn(ty),
            );
        }
        HandlerCaps { modes, copied }
    }

    /// After the many-threads check of capture `k` (type `ty`): when it reached captured
    /// variables' cells, set its bit in `mask` (requests copy it), or stop the program if it
    /// cannot be copied.
    fn note_copied(&mut self, mask: Local, k: usize, ty: TyId) {
        let saw = self.temp(Ty::U8);
        self.call_rt(Rt::TakeCells, vec![], Some(Place::local(saw)));
        let yes = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(saw)), cint(0, Ty::U8)),
        );
        let done = self.new_block();
        self.when(yes, done);
        if self.cx.uncopyable(ty) {
            let why = self.uncopyable_why(ty);
            let name = self.cx.type_name(ty);
            self.panic_msg(&format!(
                "an HTTP handler captured a `{name}` that holds a function value which changes a variable it captured: concurrent requests would change it at once, so each request needs its own copy, and {why}; keep that state in `shared(...)` instead"
            ));
        }
        let m = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(
                BinOp::BitOr,
                Operand::Copy(Place::local(mask)),
                cint(mask_bit(k), Ty::U64),
            ),
        );
        self.assign(Place::local(mask), Rvalue::Use(m));
        self.goto(done);
        self.switch_to(done);
    }

    /// The handler's environment: a heap box holding the captures, headed by its drop function,
    /// which the runtime calls once a closed server's last request has finished. When requests
    /// may copy captures, also the mask of those they copy, which the env's clone word holds
    /// (module docs).
    fn handler_env(&mut self, def: DefId, caps: &HandlerCaps) -> (Operand, Option<Local>) {
        if self.cx.fn_def(def).captures.is_empty() {
            return (cint(0, Ty::Ptr), None);
        }
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let env = self.counted_alloc(Ty::Agg(ea));
        let drop = cfunc(self.cx.func(Work::EnvDrop(def, targs)));
        // Concurrent requests read the captures from several threads (transfer.rs).
        self.transfer_args = true;
        self.fill_env(def, env.clone(), drop, cint(0, Ty::Ptr));
        self.transfer_args = false;
        // Concurrent requests may call the function values it captured from several threads
        // at once (glue/many.rs); the check notes which captures reach captured variables'
        // cells.
        let ea = self.cx.env_agg(def, &self.targs.clone());
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let base = proj(&ep, Proj::Deref(Ty::Agg(ea)));
        let mask = caps.any_copied().then(|| {
            let mask = self.temp(Ty::U64);
            self.assign(Place::local(mask), Rvalue::Use(cint(0, Ty::U64)));
            let clear = self.temp(Ty::U8);
            self.call_rt(Rt::TakeCells, vec![], Some(Place::local(clear)));
            mask
        });
        for (field, mode, ty) in self.value_captures(def) {
            if mode != PassMode::Owned {
                continue;
            }
            self.many_check(proj(&base, Proj::Field(field)), ty);
            let k = (field - ENV_HEADER) as usize;
            if let (Some(mask), true) = (mask, caps.copied[k]) {
                self.note_copied(mask, k, ty);
            }
        }
        if let Some(mask) = mask {
            let word = self.cast_to(Operand::Copy(Place::local(mask)), Ty::U64, Ty::Ptr);
            self.assign(proj(&base, Proj::Field(1)), Rvalue::Use(word));
        }
        (env, mask)
    }

    /// `init(env, req, state)` of handler closure `def<targs>` (module docs); with `copy`, the
    /// one `serve` picks when the env's mask is not zero, which copies the captures it names.
    pub(in crate::lower) fn build_handler_init(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
        copy: bool,
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("http handler state layout unavailable"));
        let caps = cx
            .shared_envs
            .get(&(def, targs.to_vec()))
            .cloned()
            .unwrap_or_else(|| ice("http handler without capture modes"));
        let f = cx.fn_def(def);
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let req = lw.new_local(Ty::Ptr, Some("req".into()));
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let mask = copy.then(|| lw.begin_copies(def, env));
        let flagged: HashSet<LocalId> = caps.flagged(f).collect();
        let mut vals = vec![];
        let mut flags = vec![];
        for p in &f.params {
            let v = match f.captures.iter().position(|c| c.inner == p.local) {
                Some(k) if caps.copied[k] => {
                    let (v, copied) = lw.copied_capture(def, env, k, caps.modes[k], mask);
                    if flagged.contains(&p.local) {
                        flags.push(Some(copied));
                    }
                    Some(v)
                }
                Some(k) => lw.shared_capture(def, env, k, caps.modes[k]),
                None => match lw.vty(p.ty) {
                    Ty::Unit => None,
                    vt => Some(lw.cast_to(Operand::Copy(Place::local(req)), Ty::Ptr, vt)),
                },
            };
            vals.push(v);
        }
        vals.extend(flags);
        let dst = Place {
            local: st,
            proj: vec![Proj::Deref(Ty::Agg(info.state))],
        };
        lw.init_state(&info, &dst, vals);
        if copy {
            lw.call_rt(Rt::XferEnd, vec![], None);
            lw.call_rt(Rt::CopyUnlock, vec![], None);
        }
        lw.terminate(Terminator::Return(unit()));
        let suffix = if copy { "$copy$init" } else { "$init" };
        let sym = format!("{}{suffix}", lw.cx.instance_symbol(&f.name, targs));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr, Ty::Ptr], Ty::Unit)
    }

    /// The copy mask in handler `def`'s env (module docs). A request's copies take turns, and
    /// form one transfer (an object or variable two captures reach is copied once).
    fn begin_copies(&mut self, def: DefId, env: Local) -> Local {
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let mask = self.temp(Ty::U64);
        let word = Operand::Copy(proj(&base, Proj::Field(1)));
        let bits = self.cast_to(word, Ty::Ptr, Ty::U64);
        self.assign(Place::local(mask), Rvalue::Use(bits));
        self.call_rt(Rt::CopyLock, vec![], None);
        self.call_rt(Rt::XferBegin, vec![], None);
        mask
    }

    /// Capture `k` that requests may copy, as one request's state stores it, and whether the
    /// request copied it (a borrowed one's drop flag). With a `mask` whose bit `k` is set it is
    /// a copy with cells of its own; otherwise what [`shared_capture`](Self::shared_capture)
    /// gives, except that a borrowed one is the value itself (the state holds it by value and
    /// does not drop it; module docs).
    fn copied_capture(
        &mut self,
        def: DefId,
        env: Local,
        k: usize,
        mode: PassMode,
        mask: Option<Local>,
    ) -> (Operand, Operand) {
        let c = self.cx.fn_def(def).captures[k];
        let local_ty = self.cx.fn_def(def).body.locals[c.inner.0 as usize].ty;
        let ty = self.sub(local_ty);
        let vt = self.cx.ty(ty);
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let slot = proj(&base, Proj::Field(ENV_HEADER + k as u32));
        let shared = |lw: &mut Self, slot: Place| match mode {
            PassMode::Owned => lw.clone_value(Operand::Copy(slot), ty),
            _ => Operand::Copy(slot),
        };
        let Some(mask) = mask else {
            return (shared(self, slot), Operand::Const(Const::Bool(false), Ty::Bool));
        };
        let bits = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(
                BinOp::BitAnd,
                Operand::Copy(Place::local(mask)),
                cint(mask_bit(k), Ty::U64),
            ),
        );
        let copy = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bits, cint(0, Ty::U64)));
        let out = self.temp(vt);
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(copy.clone(), yes, no);
        self.switch_to(yes);
        let v = self.thread_copy(Operand::Copy(slot.clone()), ty);
        self.assign(Place::local(out), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(no);
        let v = shared(self, slot);
        self.assign(Place::local(out), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(done);
        (Operand::Copy(Place::local(out)), copy)
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
        let slot = proj(&base, Proj::Field(ENV_HEADER + k as u32));
        Some(match (mode, vt) {
            (PassMode::Borrow, Ty::Agg(_)) => self.addr(slot),
            (PassMode::Owned, _) => self.clone_value(Operand::Copy(slot), ty),
            _ => Operand::Copy(slot),
        })
    }
}
