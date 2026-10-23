//! Errors thrown by a proxy's traps (issue #1174; hir_encodings.md "Proxy and Reflect"). A trap
//! may throw, as in JavaScript: velt_sema infers what the `Proxy` methods of std/proxy.vlt
//! throw from the closures stored into `ProxyHandler`'s traps, and an operation on `any` in a
//! user module (or in std/proxy, whose `Reflect` functions reach a target that may be a proxy)
//! throws what the method it runs on a proxy throws (velt_sema's `any_op`):
//!
//! | operation | method |
//! |---|---|
//! | `AnyGet` / `AnyIndex` | `__get(key, receiver)` |
//! | `AnySet` / `AnySetIndex` | `__set(key, value, receiver)` |
//! | `AnyHas` | `__has(key)` |
//! | `AnyKeys` | `__keys()` |
//!
//! When that method throws, such an operation in a user function first tests whether the value
//! holds a `Proxy` (its descriptor is the `Proxy` class's table) and then calls the method
//! directly, as a call of a throwing function: its error goes to the innermost `catch` or
//! leaves the function (errors.rs), like any other. Other values take the descriptor slot as
//! before. When no trap of the program throws, nothing here changes the code.
//!
//! Everywhere else a `Proxy` method is called from (descriptor slots, typed proxies, printing,
//! JSON, the standard library), its error ends the process with Node's `Uncaught …` report, as
//! an error nothing catches does (stage 2 of #1174 routes those too).

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::any_glue::Ready;
use super::any_proxy::{ProxyKey, PRELUDE_PROXY};
use super::glue::VtableKey;
use super::operand::proj;
use super::{ice, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl super::Cx<'_> {
    /// The prelude's `Proxy` class type, when the program has loaded it.
    fn prelude_proxy_ty(&mut self) -> Option<TyId> {
        if let Some(t) = self.prelude_proxy {
            return t;
        }
        let d = (self.hir.defs.iter())
            .position(|d| matches!(d, hir::Def::Adt(a) if a.name == PRELUDE_PROXY));
        let t = d.map(|d| self.intern(TyKind::Adt(hir::DefId(d as u32), vec![])));
        self.prelude_proxy = Some(t);
        t
    }
}

impl FnLower<'_, '_> {
    /// A call of the `Proxy` method `r` from glue or from code whose operations don't route a
    /// trap's error (module docs): an error it throws ends the process as uncaught.
    pub(super) fn finish_ready_call(
        &mut self,
        callee: Callee,
        argv: Vec<Operand>,
        r: &Ready,
    ) -> Operand {
        if r.throws.is_none() {
            return self.finish_call(callee, argv, r.ret, None);
        }
        self.push_scope(ScopeKind::Uncaught);
        let v = self.finish_call(callee, argv, r.ret, r.throws);
        self.scopes.pop();
        v
    }

    /// The `Proxy` class type and its method `name`, ready to call, when this body routes the
    /// errors of an operation on `any` that runs `name` on a proxy and that method throws.
    fn proxy_route(&mut self, name: &str) -> Option<(TyId, Ready)> {
        if !self.proxy_routes || self.dead() {
            return None;
        }
        let pty = self.cx.prelude_proxy_ty()?;
        let (d, _) = self.plain_adt(pty)?;
        let m = (self.dyn_members_of(d).into_iter())
            .find(|m| m.name == name && m.kind == DynMemberKind::Method)?;
        let r = self.ready(pty, &m)?;
        r.throws?;
        Some((pty, r))
    }

    /// Before the slot call of an operation on the `any` (`data`, `desc`) that runs the `Proxy`
    /// method `name` on a proxy: when this body routes that method's errors (module docs), a
    /// branch to `direct` (given the `Proxy` object and its type) for a proxy, which joins the
    /// block returned (with what `direct` gave); the slot call follows in the other branch.
    /// `None`: no branch.
    pub(super) fn proxy_branch<R>(
        &mut self,
        name: &str,
        data: &Operand,
        desc: &Operand,
        direct: impl FnOnce(&mut Self, vir::Local, TyId, &Ready) -> R,
    ) -> Option<(vir::BlockId, R)> {
        let (pty, r) = self.proxy_route(name)?;
        let imp = self.cx.hir.any_impl?;
        let table = self.vtable_addr(VtableKey::Impl(imp, pty));
        let is_proxy = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), table));
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_proxy, yes, no);
        self.switch_to(yes);
        let obj = self.copy_to_temp(data.clone(), Ty::Ptr);
        let v = direct(self, obj, pty, &r);
        self.goto(done);
        self.switch_to(no);
        Some((done, v))
    }

    /// After the slot call: the join of [`proxy_branch`](Self::proxy_branch)'s branches.
    pub(super) fn proxy_join<R>(&mut self, done: Option<(vir::BlockId, R)>) {
        if let Some((done, _)) = done {
            self.goto(done);
            self.switch_to(done);
        }
    }

    /// Call the `Proxy` method `r` on the `Proxy` object `obj` (of type `pty`) with the owned
    /// `args`, as a call of a throwing function: an error goes to the innermost handler, after
    /// the arguments the method borrows are dropped. Its result, owned.
    fn proxy_direct_call(
        &mut self,
        r: &Ready,
        obj: vir::Local,
        pty: TyId,
        args: Vec<(Place, TyId)>,
    ) -> Operand {
        let f = self.cx.fn_def(r.def);
        let modes: Vec<hir::PassMode> = f.params.iter().map(|p| p.mode).collect();
        let fid = self.cx.func_for(r.def, r.targs.clone());
        let this = self.any_this(obj, pty);
        // The borrowed arguments are dropped when this scope ends, also on the error's way out.
        self.push_scope(ScopeKind::Block);
        let mut argv = vec![this];
        for ((p, t), mode) in args.into_iter().zip(modes.into_iter().skip(1)) {
            let owned = mode == hir::PassMode::Owned;
            match self.cx.ty(t) {
                Ty::Unit => {}
                Ty::Agg(_) => argv.push(self.addr(p.clone())),
                _ => argv.push(Operand::Copy(p.clone())),
            }
            if !owned {
                self.own_place(p, t);
            }
        }
        let v = self.finish_call(Callee::Func(fid), argv, r.ret, r.throws);
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        self.pop_scope();
        v
    }

    /// `__get(key, receiver)` on the proxy `obj`: its result into the `any` at address `out`.
    pub(super) fn proxy_get_direct(
        &mut self,
        (obj, pty, r): (vir::Local, TyId, &Ready),
        key: ProxyKey,
        out: Operand,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let k = self.proxy_key(key);
        let recv = self.proxy_receiver(obj, pty);
        let v = self.proxy_direct_call(r, obj, pty, vec![(k, any), (recv, any)]);
        let op = self.operand_place(out, Ty::Ptr);
        let outp = proj(&op, Proj::Deref(avt));
        self.any_box_with(v, r.ret, &outp, true);
    }

    /// `__set(key, *value, receiver)` on the proxy `obj` (`value`: the address of an owned
    /// `any`); a trap answering `false` is Node's `TypeError` (module code is strict). `shown`:
    /// the key's string, for that message.
    pub(super) fn proxy_set_direct(
        &mut self,
        (obj, pty, r): (vir::Local, TyId, &Ready),
        key: ProxyKey,
        value: Operand,
        shown: &dyn Fn(&mut Self) -> Operand,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let k = self.proxy_key(key);
        let vp = self.operand_place(value, Ty::Ptr);
        let val = Place::local(self.copy_to_temp(Operand::Copy(proj(&vp, Proj::Deref(avt))), avt));
        let recv = self.proxy_receiver(obj, pty);
        let v = self.proxy_direct_call(r, obj, pty, vec![(k, any), (val, any), (recv, any)]);
        let ok = self.ready_bool(v, r.ret);
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        let ka = shown(self);
        self.panic_with_key(
            "TypeError: 'set' on proxy: trap returned falsish for property '",
            &ka,
            "'",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
    }

    /// `__has(key)` on the proxy `obj` (`key`: the address of the key's string).
    pub(super) fn proxy_has_direct(
        &mut self,
        (obj, pty, r): (vir::Local, TyId, &Ready),
        key: Operand,
    ) -> Operand {
        let any = self.cx.any_ty();
        let kl = self.copy_to_temp(key, Ty::Ptr);
        let k = self.proxy_key(ProxyKey::Str(kl));
        let v = self.proxy_direct_call(r, obj, pty, vec![(k, any)]);
        self.ready_bool(v, r.ret)
    }

    /// `__keys()` on the proxy `obj`: its `string[]`, owned.
    pub(super) fn proxy_keys_direct(&mut self, (obj, pty, r): (vir::Local, TyId, &Ready)) -> Operand {
        self.proxy_direct_call(r, obj, pty, vec![])
    }

    /// A `Proxy` method's `boolean` result `v` (of type `ret`) as a `Bool` operand.
    fn ready_bool(&mut self, v: Operand, ret: TyId) -> Operand {
        if self.cx.ty(ret) != Ty::Bool {
            ice("a `Proxy` method answering a boolean");
        }
        self.rvalue_temp(Ty::Bool, Rvalue::Use(v))
    }
}
