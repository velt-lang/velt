//! Closures. A function value is `{ code, env }`; `code` takes `env` as a hidden first param.
//! The environment is `{ drop: ptr, clone: ptr, captures… }` (layout.rs): Borrow/BorrowMut
//! captures store a pointer to the captured variable, Copy/Owned captures store the value (Owned
//! ones move it in). Environments of closures that borrow (non-escaping) live in the creating
//! function's frame with null drop/clone, and so do those of closure literals passed directly
//! as a borrowed argument without owned captures (their clone function makes a heap copy);
//! all others are counted heap boxes owning their captures (semantics stage 2: copies of a
//! function value share its env), released through the drop function in the header and deep
//! copied by the clone function. Closures without captures and named
//! functions have a null env and an env-ignoring thunk as `code` (glue/thunk.rs).

use velt_sema::hir::{self, DefId, FnDef, PassMode, TyId};

use super::operand::proj;
use super::{cfunc, cint, FnLower, LInfo, LState, ThunkKind, Work};
use crate::vir::{Function, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl super::Cx<'_> {
    /// Heap env unless the closure borrows a variable without owning any capture.
    pub(super) fn closure_env_is_heap(&self, def: DefId) -> bool {
        let caps = &self.fn_def(def).captures;
        let borrows = caps
            .iter()
            .any(|c| matches!(c.mode, PassMode::Borrow | PassMode::BorrowMut));
        let owns = caps.iter().any(|c| c.mode == PassMode::Owned);
        !caps.is_empty() && (owns || !borrows)
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// A borrowed argument: a closure literal passed by borrow cannot escape the call (except
    /// through `clone()`, which copies the env to the heap), so its env lives in this frame.
    pub(super) fn borrowed_arg(&mut self, a: &hir::Expr) -> Operand {
        match a.kind {
            hir::ExprKind::Closure(def) => self.closure_value(def, a.ty, true),
            _ => self.expr(a),
        }
    }

    /// `ExprKind::Closure(def)`: build `{ code, env }` (instantiated with the current type args).
    pub(super) fn closure(&mut self, def: DefId, ty: TyId) -> Operand {
        self.closure_value(def, ty, false)
    }

    /// `{ code, env }` of closure `def`; `borrowed`: only borrowed by a call (see
    /// [`borrowed_arg`](Self::borrowed_arg)), so value captures may stay in a frame env too.
    fn closure_value(&mut self, def: DefId, ty: TyId, borrowed: bool) -> Operand {
        let f = self.cx.fn_def(def);
        let targs = self.targs.clone();
        let ca = self.cx.closure_agg();
        if f.captures.is_empty() {
            let code = self.cx.func(Work::Thunk(ThunkKind::Env(None), def, targs));
            return self.rvalue_temp(
                Ty::Agg(ca),
                Rvalue::Aggregate(ca, vec![cfunc(code), cint(0, Ty::Ptr)]),
            );
        }
        let code = self.cx.func(Work::Fn(def, targs.clone()));
        let ea = self.cx.env_agg(def, &targs);
        let owns = f.captures.iter().any(|c| c.mode == PassMode::Owned);
        let heap = (owns || !borrowed) && self.cx.closure_env_is_heap(def);
        let (env, drop_fn, clone_fn) = if heap {
            let p = self.counted_alloc(Ty::Agg(ea));
            let d = cfunc(self.cx.func(Work::EnvDrop(def, targs.clone())));
            let c = cfunc(self.cx.func(Work::EnvClone(def, targs)));
            (p, d, c)
        } else {
            let s = self.temp(Ty::Agg(ea));
            let p = self.addr(Place::local(s));
            // A frame env of value captures still clones to a proper heap env.
            let c = match self.cx.closure_env_is_heap(def) {
                true => cfunc(self.cx.func(Work::EnvClone(def, targs))),
                false => cint(0, Ty::Ptr),
            };
            (p, cint(0, Ty::Ptr), c)
        };
        self.fill_env(def, env.clone(), drop_fn, clone_fn);
        let t = self.temp(Ty::Agg(ca));
        self.assign(
            Place::local(t),
            Rvalue::Aggregate(ca, vec![cfunc(code), env]),
        );
        let ty = self.sub(ty);
        self.own_temp(t, ty);
        Operand::Copy(Place::local(t))
    }

    /// Write the env header and the captures of closure `def` into the env at `env` (owned
    /// captures are moved in).
    pub(super) fn fill_env(
        &mut self,
        def: DefId,
        env: Operand,
        drop_fn: Operand,
        clone_fn: Operand,
    ) {
        let f = self.cx.fn_def(def);
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let envp = self.operand_place(env, Ty::Ptr);
        let base = proj(&envp, Proj::Deref(Ty::Agg(ea)));
        self.assign(proj(&base, Proj::Field(0)), Rvalue::Use(drop_fn));
        self.assign(proj(&base, Proj::Field(1)), Rvalue::Use(clone_fn));
        self.note_closure_reach(f);
        for (k, c) in f.captures.iter().enumerate() {
            let Some(outer) = self.local_target(c.outer) else {
                continue;
            };
            let slot = proj(&base, Proj::Field(2 + k as u32));
            if self.info[c.outer.0 as usize].cell
                && c.mode != PassMode::Borrow
                && c.mode != PassMode::BorrowMut
            {
                // A shared cell: the env holds one more reference to it (cells.rs).
                let ptr = self.info[c.outer.0 as usize]
                    .vir
                    .unwrap_or_else(|| super::ice("cell"));
                let p = Operand::Copy(Place::local(ptr));
                self.retain(p.clone());
                self.assign(slot, Rvalue::Use(p));
                continue;
            }
            let v = match c.mode {
                PassMode::Borrow | PassMode::BorrowMut => self.addr(outer),
                PassMode::Owned if c.share => {
                    let ty = self.info[c.outer.0 as usize].ty;
                    let v = self.share_value(Operand::Copy(outer), ty);
                    self.maybe_transfer(v, ty)
                }
                PassMode::Owned => {
                    let ty = self.info[c.outer.0 as usize].ty;
                    self.maybe_transfer(Operand::Copy(outer), ty)
                }
                _ => Operand::Copy(outer),
            };
            self.assign(slot, Rvalue::Use(v));
            if c.mode == PassMode::Owned && !c.share {
                self.mark_moved(c.outer);
            }
        }
    }

    /// Record (boxing/) whether a closure created here can reach a counted object through its
    /// captures (a shared cell, or a value of a type that can): only then are function values
    /// deep-copied when they cross to another thread (transfer.rs).
    fn note_closure_reach(&mut self, f: &FnDef) {
        if self.cx.boxing.fn_values {
            return;
        }
        let reaches = f.captures.iter().any(|c| {
            let local = &f.body.locals[c.inner.0 as usize];
            let borrowed = matches!(c.mode, PassMode::Borrow | PassMode::BorrowMut);
            let ty = self.sub(local.ty);
            (local.boxed && !borrowed) || self.cx.holds_counted(ty)
        });
        if reaches {
            self.cx.facts.fn_values = true;
        }
    }

    /// Closure body prologue: each captured local is a pointer into the env (value captures) or
    /// the pointer stored in it (borrowed captures).
    pub(super) fn bind_captures(
        &mut self,
        def: DefId,
        f: &FnDef,
        env: Local,
        info: &mut [Option<LInfo>],
    ) {
        let targs = self.targs.clone();
        let ea = self.cx.env_agg(def, &targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        for (k, c) in f.captures.iter().enumerate() {
            let ty = self.sub(f.body.locals[c.inner.0 as usize].ty);
            let slot = proj(&base, Proj::Field(2 + k as u32));
            let vir = (self.cx.ty(ty) != Ty::Unit).then(|| {
                let name = f.body.locals[c.inner.0 as usize].name.clone();
                let l = self.new_local(Ty::Ptr, Some(name));
                let cell = f.body.locals[c.inner.0 as usize].boxed;
                let ptr = match c.mode {
                    PassMode::Borrow | PassMode::BorrowMut => Rvalue::Use(Operand::Copy(slot)),
                    _ if cell => Rvalue::Use(Operand::Copy(slot)),
                    _ => Rvalue::AddrOf(slot),
                };
                self.assign(Place::local(l), ptr);
                l
            });
            info[c.inner.0 as usize] = Some(LInfo::new(vir, ty, true, false, LState::Init));
        }
    }

    /// Value captures of closure `def` with their env field index and concrete type (shared
    /// cells are `cell_captures`).
    fn value_captures(&mut self, def: DefId) -> Vec<(u32, PassMode, TyId)> {
        let f = self.cx.fn_def(def);
        let mut out = vec![];
        for (k, c) in f.captures.iter().enumerate() {
            let cell = f.body.locals[c.inner.0 as usize].boxed;
            if matches!(c.mode, PassMode::Copy | PassMode::Owned) && !cell {
                let ty = self.sub(f.body.locals[c.inner.0 as usize].ty);
                out.push((2 + k as u32, c.mode, ty));
            }
        }
        out
    }

    /// `(env: ptr)`: release one reference to the (counted) env box; the last one drops the
    /// owned captures and frees it.
    pub(super) fn build_env_drop(
        cx: &'c mut super::Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let ea = lw.cx.env_agg(def, targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let caps = lw.value_captures(def);
        let cells = lw.cell_captures(def);
        lw.release(Operand::Copy(Place::local(env)), |lw| {
            for (field, mode, ty) in caps {
                if mode == PassMode::Owned {
                    lw.drop_glue(proj(&base, Proj::Field(field)), ty);
                }
            }
            for (field, ty) in cells {
                let p = Operand::Copy(proj(&base, Proj::Field(field)));
                lw.release_cell_ptr(p, ty);
            }
            lw.counted_free(Operand::Copy(Place::local(env)), Ty::Agg(ea));
        });
        lw.terminate(Terminator::Return(super::unit()));
        let sym = format!(
            "_Genv_drop_{}",
            lw.cx.instance_symbol(&closure_name(lw.cx.hir, def), targs)
        );
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }

    /// `(env: ptr) -> ptr`: a new env box with cloned owned captures.
    pub(super) fn build_env_clone(
        cx: &'c mut super::Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let ea = lw.cx.env_agg(def, targs);
        let src = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let new = lw.counted_alloc(Ty::Agg(ea));
        let newp = lw.operand_place(new.clone(), Ty::Ptr);
        let dst = proj(&newp, Proj::Deref(Ty::Agg(ea)));
        lw.assign(dst.clone(), Rvalue::Use(Operand::Copy(src.clone())));
        // The source may be a frame env (null drop): the copy is a heap env.
        let drop = cfunc(lw.cx.func(Work::EnvDrop(def, targs.to_vec())));
        let clone = cfunc(lw.cx.func(Work::EnvClone(def, targs.to_vec())));
        lw.assign(proj(&dst, Proj::Field(0)), Rvalue::Use(drop));
        lw.assign(proj(&dst, Proj::Field(1)), Rvalue::Use(clone));
        for (field, mode, ty) in lw.value_captures(def) {
            if mode == PassMode::Owned {
                lw.clone_into(
                    proj(&src, Proj::Field(field)),
                    proj(&dst, Proj::Field(field)),
                    ty,
                );
            }
        }
        // A copy of the closure still shares the captured variables' cells.
        for (field, _) in lw.cell_captures(def) {
            lw.retain(Operand::Copy(proj(&dst, Proj::Field(field))));
        }
        lw.terminate(Terminator::Return(new));
        let sym = format!(
            "_Genv_clone_{}",
            lw.cx.instance_symbol(&closure_name(lw.cx.hir, def), targs)
        );
        lw.finish(sym, vec![Ty::Ptr], Ty::Ptr)
    }
}

impl FnLower<'_, '_> {
    /// Captures of closure `def` that hold a shared cell (escaping by-value captures of a
    /// `LocalDef::boxed` variable): env field index and the variable's concrete type.
    fn cell_captures(&mut self, def: DefId) -> Vec<(u32, TyId)> {
        let f = self.cx.fn_def(def);
        let mut out = vec![];
        for (k, c) in f.captures.iter().enumerate() {
            let cell = f.body.locals[c.inner.0 as usize].boxed;
            if cell && matches!(c.mode, PassMode::Copy | PassMode::Owned) {
                let ty = self.sub(f.body.locals[c.inner.0 as usize].ty);
                out.push((2 + k as u32, ty));
            }
        }
        out
    }
}

fn closure_name(hir: &hir::Program, def: DefId) -> String {
    match hir.def(def) {
        hir::Def::Fn(f) => f.name.clone(),
        _ => format!("def{}", def.0),
    }
}
