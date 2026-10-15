//! Module state (module-level `let`s, hir_encodings.md "Module state"): every slot lives in a
//! per-task block that `velt_rt_module_state(&DESC)` returns, allocated and initialized by
//! `init` on the task's first access (rt_abi.md "Module state"). A program without module
//! state gets none of this.

use velt_sema::hir;

use super::operand::proj;
use super::{Cx, FnLower, Glue, ScopeKind, Work};
use crate::vir::{
    self, Const, Function, Operand, Place, Proj, StaticData, StaticId, Terminator, Ty,
};

impl Cx<'_> {
    /// The descriptor `{ size, align, init, drop }` of the program's module state.
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
        let vt = self.ty(ty);
        let (size, align) = match vt {
            Ty::Agg(a) => (self.aggs[a.0 as usize].size, self.aggs[a.0 as usize].align),
            _ => super::ice("module state is not an aggregate"),
        };
        let init = self.func(Work::ModuleInit);
        let drop = self.func(Work::Glue(Glue::Drop, ty));
        let mut bytes = vec![0u8; 32];
        bytes[0..8].copy_from_slice(&(size as u64).to_le_bytes());
        bytes[8..16].copy_from_slice(&(align as u64).to_le_bytes());
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs: vec![(16, Const::Func(init)), (24, Const::Func(drop))],
        });
        self.module_desc = Some(id);
        id
    }
}

impl FnLower<'_, '_> {
    /// The current task's module state block (a `Ptr`).
    fn module_block(&mut self) -> Operand {
        let desc = self.cx.module_desc();
        let f = self
            .cx
            .extern_sym("velt_rt_module_state", vec![Ty::Ptr], Ty::Ptr, false);
        let p = self.temp(Ty::Ptr);
        self.call(
            vir::Callee::Extern(f),
            vec![Operand::Const(Const::Static(desc), Ty::Ptr)],
            Some(Place::local(p)),
            false,
        );
        Operand::Copy(Place::local(p))
    }

    /// The place of module state slot `slot` of the current task.
    pub(super) fn module_slot(&mut self, slot: u32) -> Place {
        let block = self.module_block();
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

    /// `(state: ptr)`: evaluate every initializer in order into its slot (the block is zeroed:
    /// nothing old to drop).
    pub(super) fn build_module_init(cx: &mut Cx<'_>) -> Function {
        let slots = cx
            .hir
            .module_state
            .as_ref()
            .map(|m| m.slots.clone())
            .unwrap_or_default();
        let mut lw = FnLower::bare(cx, vec![]);
        let state = lw.new_local(Ty::Ptr, Some("state".into()));
        lw.push_scope(ScopeKind::Block);
        for (i, d) in slots.into_iter().enumerate() {
            let hir::Def::Global(g) = lw.cx.hir.def(d) else {
                super::ice("module state slot is not a global")
            };
            lw.push_scope(ScopeKind::Block);
            let v = lw.consume(&g.init);
            let v = lw.detach(v, g.ty);
            if !lw.dead() {
                let p = lw.slot_in(Operand::Copy(Place::local(state)), i as u32);
                lw.store(p, v);
            }
            lw.pop_scope();
        }
        lw.pop_scope();
        lw.terminate(Terminator::Return(super::unit()));
        lw.finish("_Gmodule_init".to_string(), vec![Ty::Ptr], Ty::Unit)
    }

    /// Initialize the module state before `main` runs, as Node evaluates the modules first.
    pub(super) fn init_module_state(&mut self) {
        if self.cx.hir.module_state.is_some() {
            self.module_block();
        }
    }
}
