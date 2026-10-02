//! Dynamic callees and static interface dispatch:
//! - `Callee::Indirect`: closures / function values `{ code, env }` → `code(env, args…)`;
//! - `Callee::Virtual`: object vtable dispatcher → method entry → `entry(obj, args…)`;
//! - `Callee::Dyn`: interface value `{ data, vtable }` → `entry(data, args…)`;
//! - `Callee::ParamMethod`: resolved at monomorphization time through `Program::impls`.
//!
//! All dynamic calls use the *borrow ABI*: aggregates by pointer, the caller keeps ownership of
//! every argument; entries that need adapting (owned params, env param) are thunks (glue/thunk.rs).

use std::collections::HashMap;
use std::rc::Rc;

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::operand::proj;
use super::{cint, ice, FnLower, ThunkKind, Work};
use crate::vir::{self, Operand, Proj, Rvalue, Ty};

impl super::Cx<'_> {
    /// Unify a (possibly generic) pattern type with a concrete type, binding `Param(i)`.
    pub(super) fn match_ty(&self, pat: TyId, ty: TyId, binds: &mut Vec<Option<TyId>>) -> bool {
        match (self.types.kind(pat), self.types.kind(ty)) {
            (TyKind::Param(n), _) => {
                let n = *n as usize;
                if binds.len() <= n {
                    binds.resize(n + 1, None);
                }
                match binds[n] {
                    Some(b) => b == ty,
                    None => {
                        binds[n] = Some(ty);
                        true
                    }
                }
            }
            (TyKind::Adt(a, xs), TyKind::Adt(b, ys)) | (TyKind::Dyn(a, xs), TyKind::Dyn(b, ys)) => {
                a == b && self.match_all(xs, ys, binds)
            }
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => self.match_all(xs, ys, binds),
            (TyKind::Array(x), TyKind::Array(y))
            | (TyKind::Option(x), TyKind::Option(y))
            | (TyKind::Shared(x), TyKind::Shared(y)) => self.match_ty(*x, *y, binds),
            (TyKind::Result(a, b), TyKind::Result(c, d))
            | (TyKind::Map(a, b), TyKind::Map(c, d)) => {
                self.match_ty(*a, *c, binds) && self.match_ty(*b, *d, binds)
            }
            _ => pat == ty,
        }
    }

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

    /// Error type of interface method `slot`: every implementation (and the default) shares it
    /// (sema's dispatch groups), and it mentions no type parameters.
    fn slot_throws(&mut self, iface: DefId, slot: u32) -> Option<TyId> {
        let hir::Def::Interface(idef) = self.hir.def(iface) else {
            ice("interface call on a non-interface")
        };
        let first_impl = self.impls_of(iface).first().copied();
        let method = idef.methods[slot as usize]
            .default
            .or_else(|| first_impl.map(|i| self.hir.impls[i as usize].methods[slot as usize]))?;
        let f = self.fn_def(method);
        if f.is_async {
            return None;
        }
        self.error_ty(f.throws)
    }

    fn match_all(&self, xs: &[TyId], ys: &[TyId], binds: &mut Vec<Option<TyId>>) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.match_ty(*x, *y, binds))
    }

    /// The base classes of `ty`, nearest first.
    pub(super) fn bases_of(&mut self, ty: TyId) -> Vec<TyId> {
        let mut all = self.self_and_bases(ty);
        all.remove(0);
        all
    }

    /// `ty` followed by its base classes (an impl for a base class also serves subclasses).
    fn self_and_bases(&mut self, ty: TyId) -> Vec<TyId> {
        let mut out = vec![ty];
        let mut cur = ty;
        while let TyKind::Adt(d, args) = self.kind(cur) {
            match self.hir.def(d) {
                hir::Def::Adt(a) if a.base.is_some() => {
                    cur = self.subst(a.base.unwrap_or_else(|| ice("base")), &args);
                    out.push(cur);
                }
                _ => break,
            }
        }
        out
    }

    /// Indexes into `Program::impls` of the impls of `iface`, in program order (indexed once,
    /// since programs may hold thousands of impls of one interface).
    fn impls_of(&mut self, iface: DefId) -> Rc<[u32]> {
        let index = self.iface_impls.get_or_insert_with(|| {
            let mut by_iface: HashMap<DefId, Vec<u32>> = HashMap::new();
            for (i, imp) in self.hir.impls.iter().enumerate() {
                by_iface.entry(imp.iface).or_default().push(i as u32);
            }
            by_iface.into_iter().map(|(k, v)| (k, v.into())).collect()
        });
        index.get(&iface).cloned().unwrap_or_else(|| Rc::from([]))
    }

    /// The impl of `iface<iargs>` for concrete `ty` (or a base class of it): its index.
    pub(super) fn find_impl(&mut self, iface: DefId, iargs: &[TyId], ty: TyId) -> u32 {
        let impls = self.impls_of(iface);
        for cand in self.self_and_bases(ty) {
            for &i in impls.iter() {
                let imp = &self.hir.impls[i as usize];
                let mut binds = vec![None; imp.generics as usize];
                if self.match_ty(imp.ty, cand, &mut binds)
                    && imp.iface_args.len() == iargs.len()
                    && self.match_all(&imp.iface_args, iargs, &mut binds)
                {
                    return i;
                }
            }
        }
        ice(format_args!(
            "no impl of interface for type `{}`",
            self.type_name(ty)
        ))
    }

    /// Method `slot` of impl `index` for concrete `ty`: the def and its type args. A default
    /// method takes the interface args plus the implementor as its last type param.
    pub(super) fn impl_method(&mut self, index: u32, ty: TyId, slot: u32) -> (DefId, Vec<TyId>) {
        let imp = &self.hir.impls[index as usize];
        let (iface, pattern, generics) = (imp.iface, imp.ty, imp.generics);
        let method = imp.methods[slot as usize];
        let hir::Def::Interface(idef) = self.hir.def(iface) else {
            ice("impl of a non-interface")
        };
        let is_default = idef.methods[slot as usize].default == Some(method);
        let mut binds = vec![None; generics as usize];
        let implementor = self
            .self_and_bases(ty)
            .into_iter()
            .find(|&c| {
                binds = vec![None; generics as usize];
                self.match_ty(pattern, c, &mut binds)
            })
            .unwrap_or_else(|| ice("impl does not match its type"));
        let targs: Vec<TyId> = binds
            .into_iter()
            .map(|b| b.unwrap_or_else(|| ice("impl type parameter left unbound")))
            .collect();
        if is_default {
            let iface_args = imp.iface_args.clone();
            let mut args: Vec<TyId> = iface_args.iter().map(|&a| self.subst(a, &targs)).collect();
            args.push(implementor);
            (method, args)
        } else {
            (method, targs)
        }
    }
}

impl FnLower<'_, '_> {
    pub(super) fn resolve_param_method(
        &mut self,
        iface: DefId,
        iargs: &[TyId],
        slot: u32,
        recv: TyId,
    ) -> (DefId, Vec<TyId>) {
        let index = self.cx.find_impl(iface, iargs, recv);
        self.cx.impl_method(index, recv, slot)
    }

    /// A named function as a value (of function type `ty`): `{ code: env-ignoring thunk,
    /// env: null }`; the thunk converts errors when `ty` allows more than `def` throws.
    pub(super) fn fn_ref(&mut self, def: DefId, targs: &[TyId], ty: TyId) -> Operand {
        let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
        let f = self.cx.fn_def(def);
        let own = (!f.is_async)
            .then(|| self.cx.fn_throws(f, &targs))
            .flatten();
        let wanted = match self.kind(ty) {
            TyKind::FnPtr { throws, .. } => self.cx.error_ty(Some(throws)),
            _ => None,
        };
        let adapt = (wanted != own).then_some(wanted).flatten();
        let thunk = self.cx.func(Work::Thunk(ThunkKind::Env(adapt), def, targs));
        let a = self.cx.closure_agg();
        self.rvalue_temp(
            Ty::Agg(a),
            Rvalue::Aggregate(a, vec![super::cfunc(thunk), cint(0, Ty::Ptr)]),
        )
    }

    /// Call `target(first, args…)` with the borrow ABI; `modes` are the callee's param modes
    /// (`this` first) when known; `transfer`: the call starts a spawned task (its arguments are
    /// copied for it, transfer.rs).
    fn call_ptr(
        &mut self,
        target: Operand,
        first: Operand,
        args: &[hir::Expr],
        modes: Option<&[PassMode]>,
        (ret, throws, transfer): (TyId, Option<TyId>, bool),
    ) -> Operand {
        let receiver_mut = modes.is_some_and(|m| m.first() == Some(&PassMode::BorrowMut));
        let arg_modes = modes.map(|m| m.get(1..).unwrap_or_default());
        let (mut argv, mut params) = self.borrow_args(args, arg_modes, receiver_mut, transfer);
        argv.insert(0, first);
        params.insert(0, Ty::Ptr);
        let abi = self.cx.ret_abi(ret, throws);
        if abi.out.is_some() {
            params.push(Ty::Ptr);
        }
        let callee = vir::Callee::Ptr {
            target,
            params,
            ret: abi.ret,
        };
        self.finish_call(callee, argv, ret, throws)
    }

    pub(super) fn call_indirect(
        &mut self,
        f: &hir::Expr,
        args: &[hir::Expr],
        ty: TyId,
        transfer: bool,
    ) -> Operand {
        let fty = self.sub(f.ty);
        let fv = self.expr(f);
        let fp = self.place_of(fv, fty);
        let code = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(0)))),
        );
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(1)))),
        );
        let ret = self.sub(ty);
        let throws = match self.cx.kind(fty) {
            TyKind::FnPtr { throws, .. } => self.cx.error_ty(Some(throws)),
            _ => None,
        };
        // The callee is unknown: every non-Copy argument is a plain borrow.
        self.call_ptr(code, env, args, None, (ret, throws, transfer))
    }

    /// Load the vtable pointer of the class object `obj` (of static class type `cls`).
    pub(super) fn obj_vtable(&mut self, obj: Operand, cls: TyId) -> Operand {
        let oa = self.cx.obj_agg(cls);
        let op = self.operand_place(obj, Ty::Ptr);
        let hdr = proj(&proj(&op, Proj::Deref(Ty::Agg(oa))), Proj::Field(0));
        self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(hdr)))
    }

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
            TyKind::Dyn(iface, _) => (
                self.cx.dyn_modes(iface, slot),
                self.cx.slot_throws(iface, slot),
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
