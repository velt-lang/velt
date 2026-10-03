//! Dynamic callees and static interface dispatch:
//! - `Callee::Indirect`: closures / function values `{ code, env }` → `code(env, args…)`;
//! - `Callee::Virtual` and `Callee::Dyn`: vtable and interface calls (dispatch.rs);
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

    fn match_all(&self, xs: &[TyId], ys: &[TyId], binds: &mut Vec<Option<TyId>>) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.match_ty(*x, *y, binds))
    }

    /// The base classes of `ty`, nearest first.
    pub(super) fn bases_of(&mut self, ty: TyId) -> Vec<TyId> {
        let mut all = self.self_and_bases(ty);
        all.remove(0);
        all
    }

    /// Type args of constructor `ctor` for `new` of class type `ty`: an inherited constructor
    /// takes those of the base class that declares it (`class D extends B<string>`: `[string]`).
    pub(super) fn ctor_type_args(&mut self, ctor: DefId, ty: TyId) -> Vec<TyId> {
        let owner = match self.fn_def(ctor).self_ty.map(|t| self.kind(t)) {
            Some(TyKind::Adt(d, _)) => d,
            _ => ice("constructor without a class `this`"),
        };
        for cand in self.self_and_bases(ty) {
            if let TyKind::Adt(d, args) = self.kind(cand) {
                if d == owner {
                    return args;
                }
            }
        }
        ice("constructor of a class outside the instantiated class's ancestry")
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
    pub(super) fn impls_of(&mut self, iface: DefId) -> Rc<[u32]> {
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
    pub(super) fn call_ptr(
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
}
