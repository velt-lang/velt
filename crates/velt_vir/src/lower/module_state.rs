//! Module state (module-level `let`s, hir_encodings.md "Module state"): every slot lives in the
//! process-wide block that `velt_rt_module_state(&DESC)` returns, allocated and initialized by
//! `init` on the first access (rt_abi.md "Module state"). A program without module
//! state gets none of this.

use rustc_hash::FxHashSet;

use velt_sema::hir::{self, ExprKind as E, StmtKind as S};

use super::cint;
use super::operand::proj;
use super::{Cx, FnLower, Glue, ScopeKind, Work};
use crate::vir::{
    self, Const, Function, Operand, Place, Proj, StaticData, StaticId, Terminator, Ty,
};

impl Cx<'_> {
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
        });
        self.module_desc = Some(id);
        id
    }
}

impl FnLower<'_, '_> {
    /// `let local = init` in a global's initializer, lowered by the module state's initializer
    /// (`init_locals`): the local belongs to no function body (each initializer numbers its own
    /// from 0), so it gets a fresh VIR local of the initializer's type here.
    pub(super) fn declare_init_local(&mut self, local: hir::LocalId, init: Option<&hir::Expr>) {
        let Some(e) = init else {
            super::ice("a `let` without an initializer in a global's initializer")
        };
        let i = local.0 as usize;
        let ty = self.sub(e.ty);
        let t = self.cx.ty(ty);
        let vir = (t != Ty::Unit).then(|| self.new_local(t, Some(format!("init.{i}"))));
        let droppable = vir.is_some() && self.cx.needs_drop(ty);
        let li = super::LInfo::new(vir, ty, false, droppable, super::LState::Uninit);
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
        let desc = self.cx.module_desc();
        let f = self.cx.extern_sym(
            "velt_rt_module_state",
            vec![Ty::Ptr, Ty::U64],
            Ty::Ptr,
            false,
        );
        let p = self.temp(Ty::Ptr);
        self.call(
            vir::Callee::Extern(f),
            vec![
                Operand::Const(Const::Static(desc), Ty::Ptr),
                cint(slot as i128, Ty::U64),
            ],
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
        lw.init_locals = true;
        let state = lw.new_local(Ty::Ptr, Some("state".into()));
        lw.push_scope(ScopeKind::Block);
        for (i, d) in (from as usize..).zip(slots) {
            let hir::Def::Global(g) = lw.cx.hir.def(d) else {
                super::ice("module state slot is not a global")
            };
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
                let ready = lw.cx.extern_sym(
                    "velt_rt_module_ready",
                    vec![Ty::Ptr, Ty::U64],
                    Ty::Unit,
                    false,
                );
                lw.call(
                    vir::Callee::Extern(ready),
                    vec![
                        Operand::Copy(Place::local(state)),
                        cint(ready_count(i + 1, len), Ty::U64),
                    ],
                    None,
                    false,
                );
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

    /// The module state block, allocated and initialized on the first access, without the
    /// check that a slot is initialized (rt_abi.md "Module state").
    fn whole_block(&mut self) -> Operand {
        let desc = self.cx.module_desc();
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
fn reassigned_slots(hir: &hir::Program) -> FxHashSet<hir::DefId> {
    let mut out = FxHashSet::default();
    if hir.module_state.is_none() {
        return out;
    }
    for def in &hir.defs {
        match def {
            hir::Def::Fn(f) => block(&f.body.block, &mut out),
            hir::Def::Global(g) => expr(&g.init, &mut out),
            _ => {}
        }
    }
    out
}

fn block(b: &hir::Block, out: &mut FxHashSet<hir::DefId>) {
    for s in &b.stmts {
        match &s.kind {
            S::Let { init, .. } => init.iter().for_each(|e| expr(e, out)),
            S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => expr(init, out),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                expr(cond, out);
                block(then, out);
                els.iter().for_each(|b| block(b, out));
            }
            S::While {
                cond, body, step, ..
            } => {
                expr(cond, out);
                block(body, out);
                step.iter().for_each(|e| expr(e, out));
            }
            S::ForOf { iter, body, .. } => {
                expr(iter, out);
                block(body, out);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                block(body, out);
                catch.iter().for_each(|(_, b)| block(b, out));
                finally.iter().for_each(|b| block(b, out));
            }
            S::Block(b) => block(b, out),
        }
    }
    b.value.iter().for_each(|e| expr(e, out));
}

fn expr(e: &hir::Expr, out: &mut FxHashSet<hir::DefId>) {
    match &e.kind {
        E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
            if let E::Global(d) = place.kind {
                out.insert(d);
            }
        }
        // A closure's body is a function of its own, walked with the others.
        E::Block(b) => return block(b, out),
        _ => {}
    }
    super::frame_envs::children(e, &mut |x| expr(x, out));
}
