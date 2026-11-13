//! Module state (module-level `let`s, hir_encodings.md "Module state"): every slot lives in the
//! process-wide block that `velt_rt_module_state(&DESC)` returns, allocated and initialized by
//! `init` on the first access (rt_abi.md "Module state"). A program without module
//! state gets none of this.
//!
//! A program running on the executor in Velt reaches the block through its hooks
//! (`sched_module_*`, which keep the module lock among its workers); one that runs nothing on it
//! (no task or timer hook called: `Cx::sched_used`) through the runtime's entries, as without it,
//! and doesn't load the executor (velt_sema `lazy_std` `sched_hooks`, #2649 P3(a)). Which is
//! known once the rest of the program is built, so each access calls a small function
//! (`Work::ModuleAccess`), built last (`Cx::drain`).

use rustc_hash::FxHashSet;

use velt_sema::hir::{self, ExprKind as E, StmtKind as S};

use super::cint;
use super::hook_calls::{self as hooks, ty, HookTy};
use super::operand::proj;
use super::{Cx, FnLower, Glue, ScopeKind, Work};
use crate::vir::{
    self, Const, Function, Operand, Place, Proj, Rvalue, StaticData, StaticId, Terminator, Ty,
};

/// An access of module state, one function each (`Work::ModuleAccess`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Access {
    /// `(slot: u64) -> ptr`: the block, to reach slot `slot` (checked to be initialized).
    State,
    /// `() -> ptr`: the block, allocated and initialized on the first access, unchecked.
    Block,
    /// `(state: ptr, n: u64)`: slots up to `n` may be read from now on.
    Ready,
}

/// The item of hook `qualified` when it is one of the executor in Velt's task or timer hooks
/// (std/lang/hooks/sched, its module-state hooks excepted): a program calling one runs on the
/// executor.
pub(super) fn executor_task_hook(qualified: &str) -> Option<&str> {
    let item = qualified.strip_prefix("std/lang/hooks/sched::")?;
    (!item.starts_with("schedModule")).then_some(item)
}

impl Cx<'_> {
    /// Builds the queued functions. Module state's accesses, and the executor in Velt's exports,
    /// wait until `velt_main` is built (`main`: the second call), as whether the program runs on
    /// the executor is known then; the
    /// function releasing the static atomics at exit is built last of all: it releases the ones
    /// the program's code uses, which every other function has said by then.
    pub(super) fn drain(&mut self, main: bool) {
        loop {
            while let Some((fid, work)) = self.queue.pop_front() {
                if let (Work::ModuleAccess(_), Some(held)) = (&work, &mut self.held) {
                    held.push((fid, work));
                    continue;
                }
                if matches!(work, Work::StaticsDrop) && (!self.queue.is_empty() || self.held_any())
                {
                    self.queue.push_back((fid, work));
                    if self.queue.len() == 1 && self.held_any() {
                        break;
                    }
                    continue;
                }
                self.build_now(fid, &work);
            }
            match (main, self.held.take()) {
                (true, Some(held)) => {
                    if self.module_sched() {
                        self.seed_sched_exports();
                    }
                    self.queue.extend(held);
                }
                (true, None) if self.statics_drop() => {}
                (true, None) => return,
                (false, held) => {
                    self.held = held;
                    return;
                }
            }
        }
    }

    fn held_any(&self) -> bool {
        self.held.as_ref().is_some_and(|h| !h.is_empty())
    }

    /// Does the program run on the executor in Velt (a task or timer hook called)? Then module
    /// state goes through its hooks.
    pub(super) fn module_sched(&self) -> bool {
        self.sched_used || self.sched_tasks
    }

    /// The descriptor `{ size, align, init, drop, names }` of the program's module state.
    fn module_desc(&mut self) -> StaticId {
        if let Some(d) = self.module_desc {
            return d;
        }
        let ms = self
            .hir
            .module_state
            .as_ref()
            .unwrap_or_else(|| super::ice("module state slot without module state"));
        let ty = ms.ty;
        // Each slot's name, NUL-terminated, for the `ReferenceError` of a slot read before its
        // initializer ran.
        let mut names = vec![];
        for &d in &ms.slots {
            if let hir::Def::Global(g) = self.hir.def(d) {
                let short = g.name.rsplit("::").next().unwrap_or(&g.name);
                names.extend(short.bytes().filter(|&b| b != 0));
            }
            names.push(0);
        }
        let vt = self.ty(ty);
        let (size, align) = match vt {
            Ty::Agg(a) => (self.aggs[a.0 as usize].size, self.aggs[a.0 as usize].align),
            _ => super::ice("module state is not an aggregate"),
        };
        let (deferred, len) = (ms.deferred, ms.slots.len() as u32);
        let init = self.func(Work::ModuleInit(0, deferred));
        let drop = self.func(Work::Glue(Glue::Drop, ty));
        let names = self.static_bytes(names, 1);
        let mut bytes = vec![0u8; 48];
        bytes[0..8].copy_from_slice(&(size as u64).to_le_bytes());
        bytes[8..16].copy_from_slice(&(align as u64).to_le_bytes());
        let mut relocs = vec![
            (16, Const::Func(init)),
            (24, Const::Func(drop)),
            (32, Const::Static(names)),
        ];
        // The root file's slots, for a block made for code swapped in (`velt dev`), whose
        // `main` does not run again: null when `init` does them all.
        if deferred < len {
            relocs.push((40, Const::Func(self.func(Work::ModuleInit(deferred, len)))));
        }
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
            mutable: None,
            ..Default::default()
        });
        self.module_desc = Some(id);
        id
    }
}

impl FnLower<'_, '_> {
    /// Is `local` one of the locals of an expression checked outside any body being lowered
    /// (`init_locals`), declared where it is bound?
    pub(super) fn init_local(&self, local: hir::LocalId) -> bool {
        self.init_locals.is_some_and(|from| local.0 >= from)
    }

    /// `let local = init` in an expression checked outside any body (`init_locals`: a global's
    /// initializer, lowered by the module state's initializer, or a field initializer): the
    /// local belongs to no function body (each initializer numbers its own from 0), so it gets a
    /// fresh VIR local of the initializer's type here.
    pub(super) fn declare_init_local(&mut self, local: hir::LocalId, init: Option<&hir::Expr>) {
        let Some(e) = init else {
            super::ice("a `let` without an initializer in a global's initializer")
        };
        self.declare_init_binding(local, e.ty, false);
    }

    /// A local a pattern binds in a global's initializer (`init_locals`: the `??`, `!` and
    /// optional chains there bind the payload), declared as [`Self::declare_init_local`]
    /// declares a `let`'s, when the pattern binds it: a binding by reference of an aggregate
    /// holds a pointer to the matched part, as in a function body (`declare_body_locals`).
    pub(super) fn declare_init_binding(
        &mut self,
        local: hir::LocalId,
        ty: hir::TyId,
        by_ref: bool,
    ) {
        let i = local.0 as usize;
        let ty = self.sub(ty);
        let (t, indirect) = match self.cx.ty(ty) {
            Ty::Agg(_) if by_ref => (Ty::Ptr, true),
            t => (t, false),
        };
        let vir = (t != Ty::Unit).then(|| self.new_local(t, Some(format!("init.{i}"))));
        let droppable = vir.is_some() && !by_ref && self.cx.needs_drop(ty);
        let li = super::LInfo::new(vir, ty, indirect, droppable, super::LState::Uninit);
        if self.info.len() <= i {
            self.info.resize_with(i + 1, || {
                super::LInfo::new(None, ty, false, false, super::LState::Uninit)
            });
        }
        self.info[i] = li;
    }

    /// The module state block (a `Ptr`), to reach slot `slot` (the runtime stops with a
    /// `ReferenceError` when that slot's initializer has not run yet).
    fn module_block(&mut self, slot: u32) -> Operand {
        if !self.std_sched() {
            // Without the executor in Velt there is nothing to decide: the runtime's entry.
            return self.state_through(cint(slot as i128, Ty::U64));
        }
        // The descriptor now, so that the initializers it names are built with the rest of the
        // program, before the access functions decide.
        self.cx.module_desc();
        let f = self.cx.func(Work::ModuleAccess(Access::State));
        let p = self.temp(Ty::Ptr);
        let args = vec![cint(slot as i128, Ty::U64)];
        self.call(vir::Callee::Func(f), args, Some(Place::local(p)), false);
        Operand::Copy(Place::local(p))
    }

    /// [`Self::module_block`]'s function: the executor in Velt's hook or the runtime's entry.
    fn state_through(&mut self, slot: Operand) -> Operand {
        let desc = self.cx.module_desc();
        if self.cx.module_sched() {
            // The executor in Velt's workers keep the module lock (sched_module.velt).
            let d = self.desc_arg(desc);
            let b = hooks::sched_module_state(self, ty::USize::arg(d), ty::U64::arg(slot));
            let b = b.emit(self).op();
            return self.rvalue_temp(Ty::Ptr, Rvalue::Cast(b, Ty::Ptr));
        }
        let f = self.cx.extern_sym(
            "velt_rt_module_state",
            vec![Ty::Ptr, Ty::U64],
            Ty::Ptr,
            false,
        );
        let p = self.temp(Ty::Ptr);
        self.call(
            vir::Callee::Extern(f),
            vec![Operand::Const(Const::Static(desc), Ty::Ptr), slot],
            Some(Place::local(p)),
            false,
        );
        Operand::Copy(Place::local(p))
    }

    /// The place of module state slot `slot` of the current task.
    pub(super) fn module_slot(&mut self, slot: u32) -> Place {
        let block = self.module_block(slot);
        self.slot_in(block, slot)
    }

    fn slot_in(&mut self, block: Operand, slot: u32) -> Place {
        let ty = self.cx.hir.module_state.as_ref().map(|m| m.ty);
        let ty = ty.unwrap_or_else(|| super::ice("module state slot without module state"));
        let vt = self.cx.ty(ty);
        let base = proj(&self.operand_place(block, Ty::Ptr), Proj::Deref(vt));
        self.field_place(&base, ty, slot)
    }

    /// The slot of global `d` when it is a module-level `let`.
    pub(super) fn slot_of(&self, d: hir::DefId) -> Option<u32> {
        match self.cx.hir.def(d) {
            hir::Def::Global(g) => g.slot,
            _ => None,
        }
    }

    /// Is the place `e` a module state slot (a module-level `let`, or a module `const` that is
    /// not a constant), or a place rooted in one?
    pub(super) fn rooted_in_slot(&self, e: &hir::Expr) -> bool {
        match &e.kind {
            E::Global(d) => self.slot_of(*d).is_some(),
            E::Field { base, .. } | E::Index { base, .. } => self.rooted_in_slot(base),
            E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } | E::Downcast(base) => {
                self.rooted_in_slot(base)
            }
            _ => false,
        }
    }

    /// Does any code assign global `d`'s slot as a whole (`d = v`, `d += v`, `d++`)? A slot
    /// nothing reassigns (a module `const`, or a `let` only changed in place) keeps its value
    /// while any call runs: only what lies inside it may be replaced.
    pub(super) fn slot_reassigned(&mut self, d: hir::DefId) -> bool {
        if self.slot_of(d).is_none() {
            return false;
        }
        let hir = self.cx.hir;
        self.cx
            .reassigned_slots
            .get_or_insert_with(|| reassigned_slots(hir))
            .contains(&d)
    }

    /// `(state: ptr)`: evaluate the initializers of slots `from..to` in order into their slots
    /// (the block is zeroed: nothing old to drop). An error an initializer throws ends the
    /// process (`Uncaught`). After the last slot every slot may be read (`u64::MAX`).
    pub(super) fn build_module_init(cx: &mut Cx<'_>, from: u32, to: u32) -> Function {
        let (all, deferred) = cx
            .hir
            .module_state
            .as_ref()
            .map(|m| (m.slots.clone(), m.deferred))
            .unwrap_or_default();
        let len = all.len();
        let slots = all
            .get(from as usize..to as usize)
            .unwrap_or_default()
            .to_vec();
        let mut lw = FnLower::bare(cx, vec![]);
        lw.init_locals = Some(0);
        let state = lw.new_local(Ty::Ptr, Some("state".into()));
        lw.push_scope(ScopeKind::Block);
        for (i, d) in (from as usize..).zip(slots) {
            let hir::Def::Global(g) = lw.cx.hir.def(d) else {
                super::ice("module state slot is not a global")
            };
            lw.cx.assert_live(d);
            lw.push_scope(ScopeKind::Uncaught);
            lw.push_scope(ScopeKind::Block);
            // An imported module's top-level statements are a call of a throwing function
            // (sema `module_scripts`), like any initializer that may throw.
            let v = lw.consume(&g.init);
            let v = lw.detach(v, g.ty);
            if !lw.dead() {
                let p = lw.slot_in(Operand::Copy(Place::local(state)), i as u32);
                lw.store(p, v);
                // Slots up to this one may be read from now on.
                let n = cint(ready_count(i + 1, len), Ty::U64);
                if lw.std_sched() {
                    let ready = lw.cx.func(Work::ModuleAccess(Access::Ready));
                    let args = vec![Operand::Copy(Place::local(state)), n];
                    lw.call(vir::Callee::Func(ready), args, None, false);
                } else {
                    lw.ready_through(Operand::Copy(Place::local(state)), n);
                }
            }
            lw.pop_scope();
            lw.pop_scope();
        }
        lw.pop_scope();
        lw.terminate(Terminator::Return(super::unit()));
        // The state's own initializer, or a part of the root file's slots.
        let name = match (from, to) {
            (0, t) if t == deferred => "_Gmodule_init".to_string(),
            _ => format!("_Gmodule_init_{from}_{to}"),
        };
        lw.finish(name, vec![Ty::Ptr], Ty::Unit)
    }

    /// The address of module descriptor `desc`, as a hook's `usize` argument.
    fn desc_arg(&mut self, desc: StaticId) -> Operand {
        let p = Operand::Const(Const::Static(desc), Ty::Ptr);
        self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64))
    }

    /// The module state block, allocated and initialized on the first access, without the
    /// check that a slot is initialized (rt_abi.md "Module state").
    fn whole_block(&mut self) -> Operand {
        if !self.std_sched() {
            return self.block_through();
        }
        self.cx.module_desc();
        let f = self.cx.func(Work::ModuleAccess(Access::Block));
        let p = self.temp(Ty::Ptr);
        self.call(vir::Callee::Func(f), vec![], Some(Place::local(p)), false);
        Operand::Copy(Place::local(p))
    }

    /// [`Self::whole_block`]'s function: the executor in Velt's hook or the runtime's entry.
    fn block_through(&mut self) -> Operand {
        let desc = self.cx.module_desc();
        if self.cx.module_sched() {
            let d = self.desc_arg(desc);
            let b = hooks::sched_module_block(self, ty::USize::arg(d))
                .emit(self)
                .op();
            return self.rvalue_temp(Ty::Ptr, Rvalue::Cast(b, Ty::Ptr));
        }
        let f = self
            .cx
            .extern_sym("velt_rt_module_block", vec![Ty::Ptr], Ty::Ptr, false);
        let p = self.temp(Ty::Ptr);
        let args = vec![Operand::Const(Const::Static(desc), Ty::Ptr)];
        self.call(vir::Callee::Extern(f), args, Some(Place::local(p)), false);
        Operand::Copy(Place::local(p))
    }

    /// Initialize the module state before `main` runs, as Node evaluates the modules first
    /// (the root file's slots excepted: its `main` initializes them, `init_slots`).
    pub(super) fn init_module_state(&mut self) {
        if self.cx.hir.module_state.is_some() {
            self.whole_block();
        }
    }

    /// `Intrinsic::InitSlots(from, to)`: the root file's slots `from..to`, where the root
    /// file's statements reach their declarations.
    pub(super) fn init_slots(&mut self, from: u32, to: u32) {
        let block = self.whole_block();
        let f = self.cx.func(Work::ModuleInit(from, to));
        self.call(vir::Callee::Func(f), vec![block], None, false);
    }
}

/// The `ready` count after slot `n - 1` of `len` is stored: `u64::MAX` after the last one (the
/// whole block may be dropped at exit).
fn ready_count(n: usize, len: usize) -> i128 {
    match n == len {
        true => u64::MAX as i128,
        false => n as i128,
    }
}

/// The globals some function body or global initializer assigns as a whole (one walk over the
/// program, only for a program with module state).
pub(super) fn reassigned_slots(hir: &hir::Program) -> FxHashSet<hir::DefId> {
    let mut out = FxHashSet::default();
    if hir.module_state.is_none() {
        return out;
    }
    walk_writes(hir, &mut |_, e| {
        if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
            if let E::Global(d) = place.kind {
                out.insert(d);
            }
        }
    });
    out
}

/// Call `f` with every expression of every function body and global initializer that may
/// write a place (an assignment, a compound assignment, a call, an `await`), and the definition it is in
/// (each closure's body is a function of its own, walked with the others).
pub(super) fn walk_writes(hir: &hir::Program, f: &mut dyn FnMut(hir::DefId, &hir::Expr)) {
    for (i, def) in hir.defs.iter().enumerate() {
        let d = hir::DefId(i as u32);
        let mut g = |e: &hir::Expr| f(d, e);
        match def {
            hir::Def::Fn(fd) => block(&fd.body.block, &mut g),
            hir::Def::Global(gd) => expr(&gd.init, &mut g),
            _ => {}
        }
    }
}

/// [`walk_writes`] over one expression.
pub(super) fn walk_expr_writes(e: &hir::Expr, f: &mut dyn FnMut(&hir::Expr)) {
    expr(e, f);
}

fn block(b: &hir::Block, f: &mut dyn FnMut(&hir::Expr)) {
    for s in &b.stmts {
        match &s.kind {
            S::Let { init, .. } => init.iter().for_each(|e| expr(e, f)),
            S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => expr(init, f),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                expr(cond, f);
                block(then, f);
                els.iter().for_each(|b| block(b, f));
            }
            S::While {
                cond, body, step, ..
            } => {
                expr(cond, f);
                block(body, f);
                step.iter().for_each(|e| expr(e, f));
            }
            S::ForOf { iter, body, .. } => {
                expr(iter, f);
                block(body, f);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                block(body, f);
                catch.iter().for_each(|(_, b)| block(b, f));
                finally.iter().for_each(|b| block(b, f));
            }
            S::Block(b) => block(b, f),
        }
    }
    b.value.iter().for_each(|e| expr(e, f));
}

fn expr(e: &hir::Expr, f: &mut dyn FnMut(&hir::Expr)) {
    match &e.kind {
        E::Assign { .. }
        | E::CompoundAssign { .. }
        | E::Call { .. }
        | E::Await(_)
        | E::New { .. } => f(e),
        E::Block(b) => return block(b, f),
        _ => {}
    }
    super::frame_envs::children(e, &mut |x| expr(x, f));
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The function of module state access `a` (`Work::ModuleAccess`), built once every other
    /// function but the static atomics' release is: the executor in Velt's hook when the program
    /// runs on it, else the runtime's entry.
    pub(super) fn build_module_access(cx: &'c mut Cx<'h>, a: Access) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        lw.push_scope(ScopeKind::Block);
        let (name, params, ret) = match a {
            Access::State => {
                let slot = lw.new_local(Ty::U64, Some("slot".into()));
                let b = lw.state_through(Operand::Copy(Place::local(slot)));
                lw.terminate(Terminator::Return(b));
                ("_Gmodule_state", vec![Ty::U64], Ty::Ptr)
            }
            Access::Block => {
                let b = lw.block_through();
                lw.terminate(Terminator::Return(b));
                ("_Gmodule_block", vec![], Ty::Ptr)
            }
            Access::Ready => {
                let state = lw.new_local(Ty::Ptr, Some("state".into()));
                let n = lw.new_local(Ty::U64, Some("n".into()));
                let (st, n) = (
                    Operand::Copy(Place::local(state)),
                    Operand::Copy(Place::local(n)),
                );
                lw.ready_through(st, n);
                lw.terminate(Terminator::Return(super::unit()));
                ("_Gmodule_ready", vec![Ty::Ptr, Ty::U64], Ty::Unit)
            }
        };
        lw.scopes.clear();
        lw.finish(name.into(), params, ret)
    }

    /// Slots up to `n` of the block at `state` may be read from now on.
    fn ready_through(&mut self, state: Operand, n: Operand) {
        if self.cx.module_sched() {
            let st = self.rvalue_temp(Ty::U64, Rvalue::Cast(state, Ty::U64));
            hooks::sched_module_ready(self, ty::USize::arg(st), ty::U64::arg(n)).emit(self);
            return;
        }
        let ready = self.cx.extern_sym(
            "velt_rt_module_ready",
            vec![Ty::Ptr, Ty::U64],
            Ty::Unit,
            false,
        );
        self.call(vir::Callee::Extern(ready), vec![state, n], None, false);
    }
}
