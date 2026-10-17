//! Method calls through a typed proxy whose handler has a `get` trap (issue #823;
//! hir_encodings.md "Proxy and Reflect"). JavaScript evaluates `s.m(a)` as "read `m` from `s`,
//! then call what was read with `this` = `s`": on a proxy, the read runs
//! `get(target, "m", proxy)` before the arguments are evaluated. The usual trap forwards with
//! `Reflect.get(target, key, receiver)` (sigx's `signal()`, Vue's `reactive()`), which gives the
//! target's method, and the call then runs that method with `this` the proxy.
//!
//! So, in a program with proxiable types:
//! - a method read as a value through `any` (`AnyGet` of a method name) is a function value
//!   whose code is the method's *unbound* thunk (`ThunkKind::Unbound`, env null). Called on its
//!   own it throws a `TypeError` (it has no `this`), but it identifies the method;
//! - a method call on a proxiable class tests the receiver's tag. A proxy whose handler has a
//!   `get` trap runs it for the method's name, and the answer must be that method read as a
//!   value (the instance the call would run: one candidate for a direct call; for a virtual
//!   call, the instance the target's vtable entry holds). The method then runs with `this` the
//!   proxy, as without a `get` trap (proxy_typed.rs). Any other answer is a `TypeError` naming
//!   #823 for now, never a call that skips the trap.
//!
//! Programs without proxiable types read a method through `any` as before (not supported
//! yet) and their calls are unchanged.

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::any_glue::Ready;
use super::any_proxy::ProxyKey;
use super::glue::VtableKey;
use super::operand::proj;
use super::{cfunc, cint, Cx, FnLower, ScopeKind, ThunkKind, Work};
use crate::vir::{BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// The function type of the method `def<targs>` read as a value (its parameters after
    /// `this`), or `None` when no such value is made (generic, throwing, a generator).
    pub(super) fn method_value_ty(&mut self, def: DefId, targs: &[TyId]) -> Option<TyId> {
        let f = self.fn_def(def);
        if f.generics as usize > targs.len() || f.is_generator || f.params.is_empty() {
            return None;
        }
        let (ret, throws) = self.call_sig(f);
        let throws = throws.map(|e| self.subst(e, targs));
        if self.error_ty(throws).is_some() {
            return None;
        }
        let params: Vec<TyId> = f.params[1..]
            .iter()
            .map(|p| p.ty)
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| self.subst(t, targs))
            .collect();
        let ret = self.subst(ret, targs);
        if !self.any_boxable(ret) {
            return None;
        }
        let throws = self.intern(TyKind::Never);
        Some(self.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        }))
    }

    /// The method instances a virtual call at `slot` on a value of class `cls` may run: the
    /// entry of `cls` and of every class without type arguments in its hierarchy.
    fn proxy_slot_cands(&mut self, cls: TyId, slot: u32) -> Vec<(DefId, Vec<TyId>)> {
        let TyKind::Adt(d, _) = self.kind(cls) else {
            return vec![];
        };
        let root = self.class_root(d);
        let mut tys = vec![cls];
        for i in 0..self.hir.defs.len() as u32 {
            let x = DefId(i);
            let class = matches!(self.hir.def(x), hir::Def::Adt(a)
                if a.kind == AdtKind::Class && a.generics == 0);
            if class && x != d && self.class_root(x) == root {
                tys.push(self.intern(TyKind::Adt(x, vec![])));
            }
        }
        let mut out: Vec<(DefId, Vec<TyId>)> = vec![];
        for t in tys {
            let TyKind::Adt(x, _) = self.kind(t) else {
                continue;
            };
            let Some(&m) = self.adt_def(x).vtable.get(slot as usize) else {
                continue;
            };
            let targs = self.method_targs(m, t);
            if !out.contains(&(m, targs.clone())) {
                out.push((m, targs));
            }
        }
        out
    }
}

impl FnLower<'_, '_> {
    /// `AnyGet` of the method `r` on a class object: in a program with proxiable types, the
    /// method read as a value (module docs) into `out`. `false` when none is made.
    pub(super) fn method_value_into(&mut self, r: &Ready, out: &Place) -> bool {
        if !self.cx.has_proxies() {
            return false;
        }
        let Some(fty) = self.cx.method_value_ty(r.def, &r.targs) else {
            return false;
        };
        let code = self
            .cx
            .func(Work::Thunk(ThunkKind::Unbound, r.def, r.targs.clone()));
        let ca = self.cx.closure_agg();
        let v = self.rvalue_temp(
            Ty::Agg(ca),
            Rvalue::Aggregate(ca, vec![cfunc(code), cint(0, Ty::Ptr)]),
        );
        self.any_box_into(v, fty, out);
        true
    }

    /// In `lower_args`, once the receiver `recv` of a direct call of the method `def<targs>`
    /// on a proxiable class is lowered (`proxy_recv`): the `get` trap's read of the method.
    pub(super) fn proxy_recv_lowered(&mut self, (def, targs): (DefId, Vec<TyId>), recv: &Operand) {
        if !self.method_readable(def, &targs) {
            return self.proxy_method_check(recv);
        }
        let name = self.method_name(def);
        self.proxy_method_get(recv, &name, &[(def, targs)], None);
    }

    /// A virtual call at `slot` on the class `cls` whose receiver is `recv` and whose entry
    /// is `entry`: the `get` trap's read of the method when `recv` is a proxy.
    pub(super) fn proxy_virtual_get(
        &mut self,
        recv: &Operand,
        cls: TyId,
        slot: u32,
        entry: &Operand,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(cls) else {
            return;
        };
        let m = self.cx.adt_def(d).vtable[slot as usize];
        let targs = self.cx.method_targs(m, cls);
        if !self.method_readable(m, &targs) {
            return self.proxy_method_check(recv);
        }
        let name = self.method_name(m);
        let cands = self.cx.proxy_slot_cands(cls, slot);
        self.proxy_method_get(recv, &name, &cands, Some(entry));
    }

    /// Can the method `def<targs>` be read as a value (so a `get` trap's answer can be checked
    /// against it)? Not an async method or one that owns its receiver: a call of one through
    /// a proxy with a `get` trap is a `TypeError` naming #823 (`proxy_method_check`).
    fn method_readable(&mut self, def: DefId, targs: &[TyId]) -> bool {
        let f = self.cx.fn_def(def);
        let owned = f
            .params
            .first()
            .is_some_and(|p| p.mode == hir::PassMode::Owned);
        !f.is_async && !owned && self.cx.method_value_ty(def, targs).is_some()
    }

    /// Is `args[0]` of a call of the method `def` a receiver of a proxiable class?
    pub(super) fn proxiable_receiver(&mut self, def: DefId, args: &[hir::Expr]) -> bool {
        if self.cx.fn_def(def).self_ty.is_none() || args.is_empty() {
            return false;
        }
        let rty = self.sub(args[0].ty);
        self.cx.is_class(rty) && self.cx.proxiable(rty)
    }

    /// Is the method `def` overridden in the class `cls` (so a direct call of it on a `cls`
    /// receiver is `super.m()`, which JavaScript does not read through the receiver)?
    pub(super) fn overridden_in(&mut self, def: DefId, cls: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(cls) else {
            return false;
        };
        // A virtual method has the same slot in every class of the hierarchy.
        let mut cur = Some(d);
        while let Some(c) = cur {
            if let Some(slot) = self.cx.adt_def(c).vtable.iter().position(|&m| m == def) {
                let own = self.cx.adt_def(d).vtable.get(slot).copied();
                return own.is_some_and(|m| m != def);
            }
            cur = self.cx.adt_def(c).base.and_then(|b| match self.cx.kind(b) {
                TyKind::Adt(bd, _) => Some(bd),
                _ => None,
            });
        }
        false
    }

    /// The property name of the method `def` (its name after the class's).
    fn method_name(&self, def: DefId) -> String {
        let name = &self.cx.fn_def(def).name;
        name.rsplit('.').next().unwrap_or(name).to_string()
    }

    /// Before a call of the method `name` on the receiver `recv` of a proxiable class (module
    /// docs): a proxy whose handler has a `get` trap reads the method through it, and the
    /// answer must be one of `cands` read as a value (with `entry`, the one whose `this` entry
    /// it is).
    fn proxy_method_get(
        &mut self,
        recv: &Operand,
        name: &str,
        cands: &[(DefId, Vec<TyId>)],
        entry: Option<&Operand>,
    ) {
        if self.dead() {
            return;
        }
        let recv = self.rvalue_temp(Ty::Ptr, Rvalue::Use(recv.clone()));
        let entry = entry.map(|e| self.rvalue_temp(Ty::Ptr, Rvalue::Use(e.clone())));
        self.proxy_split(
            &recv,
            |lw, u| lw.proxy_method_cold(u, name, cands, entry.as_ref()),
            |_| {},
        );
    }

    fn proxy_method_cold(
        &mut self,
        u: Operand,
        name: &str,
        cands: &[(DefId, Vec<TyId>)],
        entry: Option<&Operand>,
    ) {
        let pty = self.cx.proxy_class_ty();
        let TyKind::Adt(pd, _) = self.cx.kind(pty) else {
            super::ice("the `Proxy` class")
        };
        let m = self
            .dyn_members_of(pd)
            .into_iter()
            .find(|m| m.name == "__hasGet" && m.kind == hir::DynMemberKind::Method);
        let Some(m) = m else {
            super::ice("the `Proxy` class without `__hasGet`")
        };
        let fid = self.cx.func_for(m.def, vec![]);
        let b = self.cx.intern(TyKind::Bool);
        let has = self.finish_call(Callee::Func(fid), vec![u.clone()], b, None);
        let (trap, done) = (self.new_block(), self.new_block());
        self.branch(has, trap, done);
        self.switch_to(trap);
        self.push_scope(ScopeKind::Block);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let data = self.copy_to_temp(u, Ty::Ptr);
        let ka = self.proxy_key_addr(name);
        let kl = self.copy_to_temp(ka, Ty::Ptr);
        let k = self.proxy_key(ProxyKey::Str(kl));
        let r = self.proxy_receiver(data, pty);
        let res = Place::local(self.temp(avt));
        if !self.proxy_invoke("__get", data, pty, vec![(k, any), (r, any)], &res) {
            super::ice("the `Proxy` class without `__get`");
        }
        let fd = self.copy_to_temp(Operand::Copy(proj(&res, Proj::Field(0))), Ty::Ptr);
        let fdesc = Operand::Copy(proj(&res, Proj::Field(1)));
        let ok = Place::local(self.temp(Ty::Bool));
        self.assign(
            ok.clone(),
            Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
        );
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        for (def, targs) in cands {
            let Some(fty) = self.cx.method_value_ty(*def, targs) else {
                continue;
            };
            let next = self.new_block();
            let want = self.vtable_addr(VtableKey::Impl(imp, fty));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, fdesc.clone(), want));
            let yes = self.new_block();
            self.branch(is, yes, next);
            self.switch_to(yes);
            let vp = self.any_value_place(fd, fty);
            let code = Operand::Copy(proj(&vp, Proj::Field(0)));
            let thunk = self
                .cx
                .func(Work::Thunk(ThunkKind::Unbound, *def, targs.clone()));
            let mut same =
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, code, cfunc(thunk)));
            if let Some(e) = entry {
                let own = self.cx.self_entry(*def, targs.clone());
                let at =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, e.clone(), cfunc(own)));
                same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, same, at));
            }
            let hit = self.new_block();
            self.branch(same, hit, next);
            self.switch_to(hit);
            self.assign(
                ok.clone(),
                Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
            );
            self.goto(next);
            self.switch_to(next);
        }
        self.drop_glue(res, any);
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(Operand::Copy(ok), good, bad);
        self.switch_to(bad);
        self.panic_msg(&format!(
            "TypeError: the `get` trap of this proxy answered '{name}' with something other than the method itself, which is not supported yet (#823)"
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
        self.pop_scope();
        self.goto(done);
        self.switch_to(done);
    }

    /// An object converted to the object type `ty` by copying its fields (`AdtLit` with
    /// `converted`): when the source is of a proxiable type and holds a typed proxy, the result
    /// is that proxy (decision 2 of #823: a proxy passed where another object type is expected
    /// keeps its identity, and its traps still run); otherwise the copy. `strict`
    /// (`Conversion::ProxyOnly`): a copy the program could tell from the original, which a
    /// plain source refuses with a `TypeError` instead.
    pub(super) fn converted_lit(
        &mut self,
        ty: TyId,
        fields: &[hir::Expr],
        strict: bool,
    ) -> Operand {
        let Some(base) = fields.iter().find_map(copied_source) else {
            return self.adt_lit(ty, fields);
        };
        let (from, to) = (self.sub(base.ty), self.sub(ty));
        self.cx.note_proxy_cast(from, to);
        if self.dead() {
            return self.adt_lit(ty, fields);
        }
        if !(self.cx.proxiable(from) && self.cx.proxiable(to)) {
            if !strict {
                return self.adt_lit(ty, fields);
            }
            self.refuse_copy();
            return self.zero_value(Ty::Ptr);
        }
        let v = self.expr(base);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        let out = Place::local(self.temp(Ty::Ptr));
        let tagged = self.proxy_tagged(p.clone());
        let (cold, hot, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(tagged, cold, hot);
        self.switch_to(cold);
        let s = self.share_value(p, to);
        self.assign(out.clone(), Rvalue::Use(s));
        self.goto(join);
        self.switch_to(hot);
        if strict {
            self.refuse_copy();
        } else {
            self.push_scope(ScopeKind::Block);
            // The copy is registered as a temporary, as `adt_lit` leaves it: moved into `out`,
            // which is registered instead once both paths meet.
            let c = self.adt_lit(ty, fields);
            if let Operand::Copy(p) = &c {
                self.take_temp(&p.clone());
            }
            self.assign(out.clone(), Rvalue::Use(c));
            self.pop_scope();
            if !self.dead() {
                self.goto(join);
            }
        }
        self.switch_to(join);
        self.own_place(out.clone(), to);
        Operand::Copy(out)
    }

    /// A conversion that would copy a plain object where the program could tell the
    /// copy from the original (`Conversion::ProxyOnly`): a `TypeError`.
    fn refuse_copy(&mut self) {
        self.panic_msg("TypeError: converting this object to another object type would copy it, and this program could tell the copy from the original; only a proxy keeps its identity through this conversion yet (#823)");
        self.terminate(Terminator::Unreachable);
    }
}

/// The source place a field of a converted object literal reads (`src.f`, or a share of it).
fn copied_source(f: &hir::Expr) -> Option<&hir::Expr> {
    use hir::ExprKind as K;
    match &f.kind {
        K::Field { base, .. } => Some(base),
        K::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
            args,
        } => args.first().and_then(copied_source),
        _ => None,
    }
}
