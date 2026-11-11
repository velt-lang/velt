//! Transfer glue for function values (glue/transfer.rs): a closure's environment carries its
//! own transfer entry (`build_env_transfer`, the env header's third word), which moves a unique
//! environment, copies a shared one, and gives the environment captured variables' cells of
//! its own. Called with a tagged pointer, it runs the many-threads check instead (many.rs).

use velt_sema::hir::{DefId, PassMode, TyId};

use super::transfer::NO_TRANSFER;
use crate::lower::closure::closure_name;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, Cx, FnLower, Work};
use crate::vir::{BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The tag of an environment pointer given to its transfer entry for a call's copy, beside the
/// many-threads check's low bit (many.rs). Environments are aligned to 8.
const ASK_CALL_COPY: i128 = 2;

impl<'c, 'h> FnLower<'c, 'h> {
    /// `{ code, env }`: a heap env goes through its transfer entry; a null env (no captures)
    /// or one without a transfer entry (a frame env, closure.rs) has nothing of its own to move.
    pub(super) fn transfer_closure(&mut self, place: &Place) {
        let envp = proj(place, Proj::Field(1));
        let (env, bit) = self.closure_env(place);
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        // The transfer entry, not the drop entry, tells: a frame env that owns captures has a
        // drop entry (its frame drop) but never a transfer entry.
        let f = self.env_transfer_entry(env.clone());
        let has = self.non_null(f.clone());
        self.when(has, done);
        let new = self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Ptr);
        let new = self.retag_env(new, bit);
        self.assign(envp, Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// What a call of a `shared` function runs on, from its environment `env`'s transfer `entry`:
    /// a copy for the caller to drop, or null when the code copies what it captured itself.
    pub(in crate::lower) fn env_call_copy(&mut self, entry: Operand, env: Operand) -> Operand {
        let asked = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, env, cint(ASK_CALL_COPY, Ty::I64)),
        );
        self.call_entry(entry, vec![asked], vec![Ty::Ptr], Ty::Ptr)
    }

    /// Prologue of an environment's transfer entry: returns from the entry when `p` has the tag
    /// asking for a call's copy.
    fn answer_asked(&mut self, p: Operand, ask: i128, answer: impl FnOnce(&mut Self, Operand)) {
        let bits = self.rvalue_temp(Ty::U64, Rvalue::Cast(p.clone(), Ty::U64));
        let low = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, bits, cint(ask, Ty::U64)),
        );
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, low, cint(0, Ty::U64)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let untagged = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, p, cint(-ask, Ty::I64)),
        );
        answer(self, untagged);
        self.switch_to(no);
    }

    /// The transfer entry of heap env `env`: it follows the drop and clone entries
    /// (closure.rs `ENV_HEADER`).
    pub(in crate::lower) fn env_transfer_entry(&mut self, env: Operand) -> Operand {
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

    /// Prologue answering for a call's copy: the environment copied, with one transfer scope for the
    /// whole environment, so an object reached twice is copied once and a cycle is copied as a cycle
    /// (velt_rt `transfer_map`); null for an async closure that copies for itself (`copy` false).
    fn answer_call_copy(
        &mut self,
        here: Operand,
        def: DefId,
        targs: &[TyId],
        caps: &[(u32, PassMode, TyId)],
        copy: bool,
    ) {
        let counted: Vec<TyId> = (caps.iter())
            .filter(|(_, mode, ty)| *mode == PassMode::Owned && self.cx.holds_counted(*ty))
            .map(|c| c.2)
            .collect();
        let copier = cfunc(self.cx.func(Work::EnvClone(def, targs.to_vec())));
        self.answer_asked(here, ASK_CALL_COPY, |lw, e| {
            let copy = match copy {
                true => {
                    for t in &counted {
                        lw.cx.note_transfer(*t);
                    }
                    if !counted.is_empty() {
                        lw.call_rt(Rt::XferBegin, vec![], None);
                    }
                    let copy = lw.call_entry(copier, vec![e], vec![Ty::Ptr], Ty::Ptr);
                    if !counted.is_empty() {
                        lw.call_rt(Rt::XferEnd, vec![], None);
                    }
                    copy
                }
                false => cint(0, Ty::Ptr),
            };
            lw.terminate(Terminator::Return(copy));
        });
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
        // Calls from several threads at once would also share a captured promise between
        // tasks: one held this way is awaited on one task (#212).
        let unshareable = caps.iter().find_map(|(_, mode, ty)| {
            (*mode == PassMode::Owned)
                .then(|| lw.cx.unshareable_part(*ty).map(|_| *ty))
                .flatten()
        });
        // A local async closure's calls share what it captured (async_fn/ctor.rs
        // `take_capture`): calls from several threads would update the counts at once. Sema
        // keeps such closures away from these places (ownership/local_async); this catches a
        // path it does not follow. A function in `shared(...)` (the strict walk) is called on a
        // copy of its environment, which this entry makes (`answer_call_copy`): only a captured
        // variable's cell, which a copy still shares, stops it there.
        let shares = lw.cx.fn_def(def).shares_captures;
        let local_cells = shares && !cells.is_empty();
        let local = shares
            && caps
                .iter()
                .any(|(_, mode, ty)| *mode == PassMode::Owned && lw.cx.holds_counted(*ty));
        let sync = !lw.cx.fn_def(def).is_async || lw.cx.fn_def(def).is_generator;
        let here = Operand::Copy(Place::local(env));
        // Only a program that calls a function in `shared(...)` asks for a call's copy, so only
        // its environments' entries have the answer. A local async closure shares what it captured
        // with each call instead of copying it, so its call in `shared(...)` gets a copy here, as a
        // sync function's does. A stopgap until shared state stops being copied per call (#2726).
        if lw.cx.boxing.shared_fns {
            lw.answer_call_copy(here, def, targs, &caps, sync || shares);
        }
        // A function in `shared(...)` (the strict walk, many.rs) runs each call on a copy, so a
        // change it makes would be lost.
        let changes = lw.cx.fn_def(def).changes_held || (sync && !cells.is_empty());
        let (caps2, cells2) = (caps.clone(), cells.clone());
        lw.check_if_tagged(env, move |lw, e| {
            if changes {
                lw.panic_if_strict(
                    "a function that changes or shares a variable it captured is in `shared(...)`: each call runs on a copy of what it captured, so the change would be lost; keep what changes in a `shared(...)`, or put the function in a `Mutex`",
                );
            }
            if let Some(t) = unshareable {
                lw.end_walk_scope();
                lw.panic_many_threads(t);
            }
            if local_cells || (local && !lw.many_strict) {
                lw.walk_panic(
                    "an async closure that changes or shares what it captured is shared between threads (`shared(...)`, a `Mutex`'s value, or an HTTP handler): its calls would use the captured values from several threads at once; capture `shared` values instead",
                );
            }
            let ep = lw.operand_place(e, Ty::Ptr);
            let base = proj(&ep, Proj::Deref(Ty::Agg(ea)));
            for &(field, mode, ty) in &caps2 {
                if mode == PassMode::Owned {
                    lw.many_check(proj(&base, Proj::Field(field)), ty);
                }
            }
            for &(field, ty) in &cells2 {
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
        // An environment that holds itself (through an array of functions, say) ends: its copy in
        // this transfer takes the place of the sender's reference.
        if lw.cx.boxing.shared_fns {
            let ev = Operand::Copy(Place::local(env));
            let give = ev.clone();
            lw.return_env_copy_then(ev, |lw| lw.release_count(give));
        }
        let clone = cfunc(lw.cx.func(Work::EnvClone(def, targs.to_vec())));
        let ev = Operand::Copy(Place::local(env));
        let new = lw.call_entry(clone, vec![ev.clone()], vec![Ty::Ptr], Ty::Ptr);
        lw.assign(Place::local(out), Rvalue::Use(new));
        lw.release_count(ev);
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

    /// Prologue of an environment's clone entry: returns the copy of `env` made in this transfer, if
    /// there is one. What `find_copy` answered, for `record_copy`.
    pub(in crate::lower) fn return_env_copy(&mut self, env: Operand) -> Operand {
        self.return_env_copy_then(env, |_| {})
    }

    /// `return_env_copy`, running `give_up` before it returns one.
    fn return_env_copy_then(&mut self, env: Operand, give_up: impl FnOnce(&mut Self)) -> Operand {
        let found = self.temp(Ty::Ptr);
        self.call_rt(Rt::XferFind, vec![env], Some(Place::local(found)));
        let f = Operand::Copy(Place::local(found));
        let n = self.cast_to(f.clone(), Ty::Ptr, Ty::U64);
        let is_copy = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, n, cint(NO_TRANSFER, Ty::U64)),
        );
        let (hit, go) = (self.new_block(), self.new_block());
        self.branch(is_copy, hit, go);
        self.switch_to(hit);
        self.retain(f.clone());
        give_up(self);
        self.terminate(Terminator::Return(f.clone()));
        self.switch_to(go);
        f
    }

    /// One reference to the shared env `env` given up: others hold it.
    fn release_count(&mut self, env: Operand) {
        let c = self.count_place(env);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, Operand::Copy(c.clone()), cint(1, Ty::U64)),
        );
        self.assign(c, Rvalue::Use(n));
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
