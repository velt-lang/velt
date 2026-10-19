//! Transfer glue for function values (glue/transfer.rs): a closure's environment carries its
//! own transfer entry (`build_env_transfer`, the env header's third word), which moves a unique
//! environment, copies a shared one, and gives the environment captured variables' cells of
//! its own. Called with a tagged pointer, it runs the many-threads check instead (many.rs).

use velt_sema::hir::{DefId, PassMode, TyId};

use crate::lower::closure::closure_name;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, Cx, FnLower, Work};
use crate::vir::{BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    /// `{ code, env }`: a heap env goes through its transfer entry; a null env (no captures)
    /// or one without a transfer entry (a frame env, closure.rs) has nothing of its own to move.
    pub(super) fn transfer_closure(&mut self, place: &Place) {
        let envp = proj(place, Proj::Field(1));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(envp.clone())));
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        // The transfer entry, not the drop entry, tells: a frame env that owns captures has a
        // drop entry (its frame drop) but never a transfer entry.
        let f = self.env_transfer_entry(env.clone());
        let has = self.non_null(f.clone());
        self.when(has, done);
        let new = self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Ptr);
        self.assign(envp, Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// The transfer entry of heap env `env`: it follows the drop and clone entries
    /// (closure.rs `ENV_HEADER`).
    pub(super) fn env_transfer_entry(&mut self, env: Operand) -> Operand {
        let (word, _) = self.cx.size_align(Ty::Ptr);
        let tp = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, env, cint(2 * i128::from(word), Ty::I64)),
        );
        let tpp = self.operand_place(tp, Ty::Ptr);
        self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&tpp, Proj::Deref(Ty::Ptr)))),
        )
    }

    /// `(env: ptr) -> ptr` of closure `(def, targs)`: a shared env is first deep-copied (its
    /// clone entry; the sender's reference released); then the env — now unique — has its
    /// owned captures transferred and every captured variable's cell made its own (a cell the
    /// creator still shares is copied: the task's assignments stay in the task, as with any
    /// copy).
    pub(in crate::lower) fn build_env_transfer(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let ea = lw.cx.env_agg(def, targs);
        let out = lw.temp(Ty::Ptr);
        lw.assign(
            Place::local(out),
            Rvalue::Use(Operand::Copy(Place::local(env))),
        );
        let caps = lw.value_captures(def);
        let cells = lw.cell_captures(def);
        let uncopyable = caps
            .iter()
            .find(|(_, mode, ty)| *mode == PassMode::Owned && lw.cx.uncopyable(*ty))
            .map(|c| c.2);
        // A local async closure's calls share what it captured (async_fn/ctor.rs
        // `take_capture`): calls from several threads would update the counts at once. Sema
        // keeps such closures away from these places (ownership/local_async); this catches a
        // path it does not follow.
        let local = lw.cx.fn_def(def).shares_captures
            && (!cells.is_empty()
                || caps
                    .iter()
                    .any(|(_, mode, ty)| *mode == PassMode::Owned && lw.cx.holds_counted(*ty)));
        let (caps2, cells2) = (caps.clone(), cells.clone());
        lw.check_if_tagged(env, |lw, e| {
            if let Some(t) = uncopyable {
                lw.panic_many_threads(t);
            }
            if local {
                lw.panic_msg(
                    "an async closure that changes or shares what it captured is shared between threads (`shared(...)`, a `Mutex`'s value, or an HTTP handler): its calls would use the captured values from several threads at once; capture `shared` values instead",
                );
            }
            let ep = lw.operand_place(e, Ty::Ptr);
            let base = proj(&ep, Proj::Deref(Ty::Agg(ea)));
            for (field, mode, ty) in caps2 {
                if mode == PassMode::Owned {
                    lw.many_check(proj(&base, Proj::Field(field)), ty);
                }
            }
            for (field, ty) in cells2 {
                let vt = lw.cx.ty(ty);
                let cell = lw.rvalue_temp(
                    Ty::Ptr,
                    Rvalue::Use(Operand::Copy(proj(&base, Proj::Field(field)))),
                );
                let cp = lw.operand_place(cell, Ty::Ptr);
                lw.many_check(proj(&cp, Proj::Deref(vt)), ty);
            }
        });
        let (unique, shared) = (lw.new_block(), lw.new_block());
        let one = lw.count_is_one(Operand::Copy(Place::local(env)));
        lw.branch(one, unique, shared);
        lw.switch_to(shared);
        if let Some(t) = uncopyable {
            // The function value is still used here (or held elsewhere), so the task would
            // need a copy of its environment.
            let why = lw.uncopyable_why(t);
            let name = lw.cx.type_name(t);
            lw.panic_msg(&format!(
                "cannot copy a function value that captured a `{name}` for another task: the function value is still used here, and {why}"
            ));
        }
        let clone = cfunc(lw.cx.func(Work::EnvClone(def, targs.to_vec())));
        let ev = Operand::Copy(Place::local(env));
        let new = lw.call_entry(clone, vec![ev.clone()], vec![Ty::Ptr], Ty::Ptr);
        lw.assign(Place::local(out), Rvalue::Use(new));
        let c = lw.count_place(ev);
        let n = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, Operand::Copy(c.clone()), cint(1, Ty::U64)),
        );
        lw.assign(c, Rvalue::Use(n));
        lw.goto(unique);
        lw.switch_to(unique);
        let base = proj(&Place::local(out), Proj::Deref(Ty::Agg(ea)));
        for (field, mode, ty) in caps {
            if mode == PassMode::Owned {
                lw.transfer_in_place(proj(&base, Proj::Field(field)), ty);
            }
        }
        for (field, ty) in cells {
            lw.own_cell(proj(&base, Proj::Field(field)), ty);
        }
        lw.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
        let sym = format!(
            "_Genv_transfer_{}",
            lw.cx.instance_symbol(&closure_name(lw.cx.hir, def), targs)
        );
        lw.finish(sym, vec![Ty::Ptr], Ty::Ptr)
    }

    /// The cell pointer at `slot` (holding a `ty`), made this env's own: transferred in place
    /// when nothing else references it, else replaced by a new cell with a copy of the value.
    pub(super) fn own_cell(&mut self, slot: Place, ty: TyId) {
        let vt = self.cx.ty(ty);
        let cell = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(slot.clone())));
        let (unique, shared, done) = (self.new_block(), self.new_block(), self.new_block());
        let one = self.count_is_one(cell.clone());
        self.branch(one, unique, shared);
        self.switch_to(shared);
        let cp = self.operand_place(cell.clone(), Ty::Ptr);
        let value = proj(&cp, Proj::Deref(vt));
        let new = self.shared_copy(cell.clone(), ty, false, |lw, _| {
            let fresh = lw.counted_alloc(vt);
            let fp = lw.operand_place(fresh.clone(), Ty::Ptr);
            let copy = lw.thread_copy(Operand::Copy(value.clone()), ty);
            lw.store(proj(&fp, Proj::Deref(vt)), copy);
            lw.cell_check(Rt::CellCopy, vec![fresh.clone(), cell.clone()]);
            fresh
        });
        self.assign(slot, Rvalue::Use(new));
        self.goto(done);
        self.switch_to(unique);
        self.cell_check(Rt::CellGive, vec![cell.clone()]);
        let cp = self.operand_place(cell, Ty::Ptr);
        self.transfer_in_place(proj(&cp, Proj::Deref(vt)), ty);
        self.goto(done);
        self.switch_to(done);
    }
}
