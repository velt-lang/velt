//! Dynamic dispatch (callee.rs has the common call path):
//! - `Callee::Virtual`: object vtable dispatcher → method entry → `entry(obj, args…)`;
//! - `Callee::Dyn`: interface value `{ data, vtable }` → `entry(data, args…)`.
//!
//! Entries use the borrow ABI, so the receiver is borrowed; a spawned call transfers it
//! (transfer.rs).

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::operand::proj;
use super::{ice, FnLower};
use crate::vir::{Operand, Proj, Rvalue, Ty};

impl super::Cx<'_> {
    /// Param modes (`this` first) of interface method `slot`: the join over the default and
    /// every implementation (`BorrowMut` if any of them modifies the param); `None` when
    /// nothing implements the interface.
    fn dyn_modes(&mut self, iface: DefId, slot: u32) -> Option<Vec<PassMode>> {
        if let Some(modes) = self.dyn_modes_memo.get(&(iface, slot)) {
            return modes.clone();
        }
        let modes = self.join_impl_modes(iface, slot);
        self.dyn_modes_memo.insert((iface, slot), modes.clone());
        modes
    }

    /// [`Self::dyn_modes`] computed (once per slot: every dyn call of a widely implemented
    /// interface would otherwise visit all its impls).
    fn join_impl_modes(&mut self, iface: DefId, slot: u32) -> Option<Vec<PassMode>> {
        let hir::Def::Interface(idef) = self.hir.def(iface) else {
            ice("interface call on a non-interface")
        };
        let impls = self.impls_of(iface);
        let hir_impls = &self.hir.impls;
        let methods = idef.methods[slot as usize].default.into_iter().chain(
            impls
                .iter()
                .map(|&i| hir_impls[i as usize].methods[slot as usize]),
        );
        let mut join: Option<Vec<PassMode>> = None;
        for m in methods {
            let modes = method_modes(self.fn_def(m));
            match &mut join {
                None => join = Some(modes),
                Some(j) => {
                    for (a, b) in j.iter_mut().zip(modes) {
                        if b == PassMode::BorrowMut {
                            *a = b;
                        }
                    }
                }
            }
        }
        join
    }

    /// Error type of a call through interface method `slot` of `iface<args>`: sema's
    /// `InterfaceMethodDef::throws` (in the interface's type params) with the `Dyn`'s args. A
    /// promise slot never throws: its implementations reject the promise instead.
    fn slot_throws(&mut self, iface: DefId, slot: u32, args: &[TyId]) -> Option<TyId> {
        let hir::Def::Interface(idef) = self.hir.def(iface) else {
            ice("interface call on a non-interface")
        };
        let t = idef.methods[slot as usize].throws?;
        let t = self.subst(t, args);
        self.error_ty(Some(t))
    }
}

impl FnLower<'_, '_> {
    /// The receiver of a vtable call. Entries borrow it (an owning target, such as an async
    /// method, takes its own reference in its thunk), so a receiver that sema moved into the
    /// call (its last use, when the method owns `this`) is a temporary dropped after the call.
    /// `transfer` (the call starts a spawned task): the receiver is a deep copy for the task
    /// (through the vtable's clone glue when the static type is a base class or an interface),
    /// which the caller releases before the task starts, as for the arguments (transfer.rs).
    fn receiver(&mut self, recv: &hir::Expr, transfer: bool) -> Operand {
        let v = if super::call::is_moved(recv) {
            let v = self.consume(recv);
            let ty = self.sub(recv.ty);
            self.own_value(v, ty)
        } else {
            self.expr(recv)
        };
        match transfer {
            true => self.transfer_copy(v, recv.ty),
            false => v,
        }
    }

    pub(super) fn call_virtual(
        &mut self,
        slot: u32,
        args: &[hir::Expr],
        ty: TyId,
        transfer: bool,
    ) -> Operand {
        let recv = args
            .first()
            .unwrap_or_else(|| ice("virtual call without receiver"));
        let cls = self.sub(recv.ty);
        let TyKind::Adt(d, cargs) = self.cx.kind(cls) else {
            ice("virtual call on a non-class receiver")
        };
        let method = self.cx.adt_def(d).vtable[slot as usize];
        let modes = method_modes(self.cx.fn_def(method));
        let throws = self.cx.call_sig(self.cx.fn_def(method)).1;
        let throws = throws.map(|e| self.cx.subst(e, &cargs));
        let throws = self.cx.error_ty(throws);
        let rv = self.receiver(recv, transfer);
        let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(rv));
        let vt = self.obj_vtable(obj.clone(), cls);
        let entry = self.dispatch(vt, slot as i128);
        let ret = self.sub(ty);
        self.call_ptr(
            entry,
            obj,
            &args[1..],
            Some(&modes),
            (ret, throws, transfer),
        )
    }

    pub(super) fn call_dyn(
        &mut self,
        slot: u32,
        args: &[hir::Expr],
        ty: TyId,
        transfer: bool,
    ) -> Operand {
        let recv = args
            .first()
            .unwrap_or_else(|| ice("interface call without receiver"));
        let dty = self.sub(recv.ty);
        let (modes, throws) = match self.cx.kind(dty) {
            TyKind::Dyn(iface, args) => (
                self.cx.dyn_modes(iface, slot),
                self.cx.slot_throws(iface, slot, &args),
            ),
            _ => ice("interface call on a non-interface receiver"),
        };
        let rv = self.receiver(recv, transfer);
        let rp = self.place_of(rv, dty);
        let data = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&rp, Proj::Field(0)))),
        );
        let vt = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&rp, Proj::Field(1)))),
        );
        let entry = self.dispatch(vt, slot as i128);
        let ret = self.sub(ty);
        self.call_ptr(
            entry,
            data,
            &args[1..],
            modes.as_deref(),
            (ret, throws, transfer),
        )
    }
}

/// Param modes of a method, `this` first.
fn method_modes(f: &hir::FnDef) -> Vec<PassMode> {
    f.params.iter().map(|p| p.mode).collect()
}
