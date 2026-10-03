//! `spawn(p)` (docs/reference/async.md "Tasks"). A direct call of a compiled async function
//! or an async closure literal becomes a task from its initial state, with its arguments or
//! captures transferred (transfer.rs); a call through a function value, vtable or interface
//! passes the task copies. Any other promise — a stored one, already started by this task —
//! keeps running where it started, and its result is transferred where it is produced
//! (`velt_rt_fut_transfer`, #160), so the task's join handle delivers nothing this task still
//! references.

use velt_sema::hir::{self, TyId, TyKind};

use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, FnLower, Glue, ScopeKind, Work};
use crate::vir::{Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// `spawn(p)`: a direct compiled call or async closure literal starts from an inline initial
    /// state (`velt_rt_spawn`); any other promise is a heap future (`velt_rt_spawn_fut`).
    /// `detached`: the join handle is dropped at once (a `spawn(...)` statement), so the task's
    /// error is reported as uncaught.
    pub(in crate::lower) fn spawn(&mut self, p: &hir::Expr, ty: TyId, detached: bool) -> Operand {
        let pty = self.sub(ty);
        let slot = self.cx.promise_slot(pty);
        let st = self.cx.ty(slot);
        let rsize = cint(self.cx.size_align(st).0 as i128, Ty::U64);
        let res = self.cx.promise_result(pty);
        let rt = self.cx.ty(res);
        let wrapped_size = cint(self.cx.size_align(rt).0 as i128, Ty::U64);
        let started = match &p.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Def(d, targs),
                args,
            } if self.cx.is_async_fn(*d) => {
                let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
                if self.cx.async_info(*d, &targs).is_some() {
                    // The task may run on another thread: its arguments are transferred.
                    self.transfer_args = true;
                    let state = self.state_from_call(*d, &targs, args);
                    self.transfer_args = false;
                    state.map(|(info, s)| self.value_future(*d, &targs, &info, s, detached))
                } else {
                    None
                }
            }
            hir::ExprKind::Closure(d) if self.cx.is_async_fn(*d) => {
                let targs = self.targs.clone();
                // The task may run on another thread: its captures are transferred.
                self.transfer_args = true;
                let state = self.state_from_closure(*d);
                self.transfer_args = false;
                state.map(|(info, s)| self.value_future(*d, &targs, &info, s, detached))
            }
            _ => None,
        };
        // Drops the task's result if nobody claims it (its join handle was dropped). A detached
        // task from an initial state reports its own error (value.rs) and leaves only `T`; a
        // detached heap future (`spawn(p);` of a stored promise) leaves its slot, whose error is
        // reported here.
        let result_drop = if detached && started.is_some() {
            self.result_drop_fn(res).unwrap_or_else(|| cint(0, Ty::Ptr))
        } else if detached {
            self.unclaimed_drop_fn(pty)
        } else {
            self.result_drop_fn(slot)
                .unwrap_or_else(|| cint(0, Ty::Ptr))
        };
        if let Some((poll, drop, s)) = started {
            let st = self.locals[s.0 as usize].ty;
            let (size, align) = self.cx.size_align(st);
            let a = self.addr(Place::local(s));
            let args = vec![
                cfunc(poll),
                cfunc(drop),
                a,
                cint(size as i128, Ty::U64),
                cint(align as i128, Ty::U64),
                if detached { wrapped_size } else { rsize },
                result_drop,
            ];
            return self.rt_value(Rt::Spawn, args, ty);
        }
        let fut = match self.kind(p.ty) {
            TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                let v = self.call_indirect(p, &[], pty, false);
                self.take_owned(v)
            }
            _ if dynamic_call(p) => {
                // The callee keeps a share of each argument: they are copies for the task,
                // whose other references are released before it starts (transfer.rs).
                self.push_scope(ScopeKind::Temps);
                self.transfer_call = true;
                let fut = self.take_promise(p);
                // The spawned call took the flag (call.rs `call_expr`); it must not leak to a
                // later call.
                debug_assert!(
                    !self.transfer_call,
                    "ICE: spawned call did not take transfer_call"
                );
                self.pop_scope();
                fut
            }
            _ => self.take_promise(p),
        };
        if !self.dead() {
            self.transfer_result(fut.clone(), pty);
        }
        self.rt_value(Rt::SpawnFut, vec![fut, rsize, result_drop], ty)
    }

    /// The promise value `fut` (of type `pty`) leaves this task: its result is transferred
    /// where it is produced (glue/transfer.rs).
    fn transfer_result(&mut self, fut: Operand, pty: TyId) {
        let slot = self.cx.promise_slot(pty);
        if self.cx.holds_counted(slot) {
            let g = cfunc(self.cx.func(Work::Glue(Glue::Transfer, slot)));
            self.call_rt(Rt::FutTransfer, vec![fut, g], None);
        }
    }
}

/// Is `p` a call through a function value, vtable or interface (borrow ABI, callee.rs)?
fn dynamic_call(p: &hir::Expr) -> bool {
    matches!(
        p.kind,
        hir::ExprKind::Call {
            callee: hir::Callee::Indirect(_)
                | hir::Callee::Virtual { .. }
                | hir::Callee::Dyn { .. },
            ..
        }
    )
}
