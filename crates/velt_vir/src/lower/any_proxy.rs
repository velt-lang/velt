//! The descriptor slots of a proxy (issue #823; hir_encodings.md "Proxy and Reflect"). A
//! proxy is an `any` holding the prelude's `Proxy` object (std/prelude/zz_proxy.vlt); the
//! member slots of its table (any_member.rs, any_set.rs) call that class's methods, which run
//! the handler's traps or forward to the target:
//!
//! | slot | calls |
//! |---|---|
//! | `AnyGet` / `AnyIndex` | `__get(key, receiver)` (a number key as its string) |
//! | `AnyCall` | `__get(key, receiver)`, then the function value it returns (`AnyApply` slot) |
//! | `AnySet` / `AnySetIndex` | `__set(key, value, receiver)`; `false` is Node's `TypeError` |
//! | `AnyApply` | not a function (a proxy of a function comes later) |
//!
//! The receiver is the proxy itself: the `Proxy` object again, with its own table.

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::any_member::{SLOT_APPLY, SLOT_CALL, ST_OK, ST_UNSUPPORTED};
use super::operand::proj;
use super::rt::Rt;
use super::FnLower;
use crate::vir::{self, Operand, Place, Terminator, Ty, STR_AGG};

/// The qualified names of the prelude's `Proxy` and `Reflect` classes: a program's own
/// classes are named after their modules, which are never under `std/`.
pub(super) const PRELUDE_PROXY: &str = "std/prelude/zz_proxy::Proxy";
pub(super) const PRELUDE_REFLECT: &str = "std/prelude/zz_proxy::Reflect";
/// The start of the names of `Reflect`'s methods.
pub(super) const PRELUDE_REFLECT_METHODS: &str = "std/prelude/zz_proxy::Reflect.";

/// How the key reaches a proxy slot.
#[derive(Clone, Copy)]
pub(super) enum ProxyKey {
    /// The slot's parameter: the address of a string.
    Str(vir::Local),
    /// The slot's parameter: an `f64`.
    Num(vir::Local),
}

impl FnLower<'_, '_> {
    /// Is `ty` the prelude's `Proxy` class (not a program's own class of that name)?
    pub(super) fn is_proxy(&self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        matches!(self.cx.hir.def(d), hir::Def::Adt(a) if a.name == PRELUDE_PROXY)
    }

    /// The key as an owned `any` temporary.
    pub(super) fn proxy_key(&mut self, key: ProxyKey) -> Place {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        let s = self.cx.intern(TyKind::Str);
        match key {
            ProxyKey::Str(k) => {
                let kp = self.deref_param(k, s);
                let v = self.share_value(Operand::Copy(kp), s);
                self.any_box_into(v, s, &out);
            }
            ProxyKey::Num(i) => {
                let ks = self.temp(Ty::Agg(STR_AGG));
                let ka = self.addr(Place::local(ks));
                let iv = Operand::Copy(Place::local(i));
                self.call_rt(Rt::StrFromF64, vec![iv, ka], None);
                self.any_box_into(Operand::Copy(Place::local(ks)), s, &out);
            }
        }
        out
    }

    /// The proxy itself (`data`, of type `ty`) as an owned `any`: the receiver its traps see.
    pub(super) fn proxy_receiver(&mut self, data: vir::Local, ty: TyId) -> Place {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        let vp = self.any_value_place(data, ty);
        let v = self.share_value(Operand::Copy(vp), ty);
        self.any_box_into(v, ty, &out);
        out
    }

    /// Call the `Proxy` method `name` with `this` = `data` and the owned `args`; its result
    /// boxed into `out`. `false` when the method cannot be called from glue.
    pub(super) fn proxy_invoke(
        &mut self,
        name: &str,
        data: vir::Local,
        ty: TyId,
        args: Vec<(Place, TyId)>,
        out: &Place,
    ) -> bool {
        let Some((d, _)) = self.plain_adt(ty) else {
            return false;
        };
        let m = self
            .dyn_members_of(d)
            .into_iter()
            .find(|m| m.name == name && m.kind == DynMemberKind::Method);
        let Some(r) = m.and_then(|m| self.ready(ty, &m)) else {
            for (p, t) in args {
                self.drop_glue(p, t);
            }
            return false;
        };
        let this = self.any_this(data, ty);
        self.any_invoke(&r, this, args, out);
        true
    }

    /// `AnyGet` / `AnyIndex` of a proxy: `__get(key, receiver)` into `*out`.
    pub(super) fn proxy_get_body(
        &mut self,
        data: vir::Local,
        key: ProxyKey,
        out: &Place,
        ty: TyId,
    ) {
        let any = self.cx.any_ty();
        let k = self.proxy_key(key);
        let r = self.proxy_receiver(data, ty);
        let st = match self.proxy_invoke("__get", data, ty, vec![(k, any), (r, any)], out) {
            true => ST_OK,
            false => ST_UNSUPPORTED,
        };
        self.ret_status(st);
    }

    /// `AnyCall` of a proxy: `p.m(args)` reads `m` through the `get` trap, then calls the value
    /// it returns (through that value's own apply slot, which owns the arguments).
    pub(super) fn proxy_call_body(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        // A handler without traps: the call on the target itself.
        let plain = Place::local(self.temp(avt));
        if !self.proxy_invoke("__plain", data, ty, vec![], &plain) {
            self.any_drop_args_all(argv, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let b = self.cx.intern(TyKind::Bool);
        let is_plain = self.any_unbox(&plain, any, b);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_plain, yes, no);
        self.switch_to(yes);
        let t = Place::local(self.temp(avt));
        if !self.proxy_invoke("__target", data, ty, vec![], &t) {
            self.any_drop_args_all(argv, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let (td, tdesc) = self.any_parts(Operand::Copy(t.clone()), any);
        let a = [key, argv, argc, out].map(|l| Operand::Copy(Place::local(l)));
        let mut args = vec![td];
        args.extend(a);
        let p = vec![Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
        let st = self.any_slot(tdesc, SLOT_CALL, args, p);
        self.drop_glue(t, any);
        self.terminate(Terminator::Return(st));
        self.switch_to(no);
        let f = Place::local(self.temp(avt));
        let k = self.proxy_key(ProxyKey::Str(key));
        let r = self.proxy_receiver(data, ty);
        if !self.proxy_invoke("__get", data, ty, vec![(k, any), (r, any)], &f) {
            self.any_drop_args_all(argv, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let (fd, fdesc) = self.any_parts(Operand::Copy(f.clone()), any);
        let a = [argv, argc, out].map(|l| Operand::Copy(Place::local(l)));
        let a = vec![fd, a[0].clone(), a[1].clone(), a[2].clone()];
        let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
        let st = self.any_slot(fdesc, SLOT_APPLY, a, p);
        self.drop_glue(f, any);
        self.terminate(Terminator::Return(st));
    }

    /// `AnySet` / `AnySetIndex` of a proxy: `__set(key, *v, receiver)`; a trap returning
    /// `false` throws Node's `TypeError` (module code is strict).
    pub(super) fn proxy_set_body(
        &mut self,
        data: vir::Local,
        key: ProxyKey,
        v: vir::Local,
        ty: TyId,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let k = self.proxy_key(key);
        // The key again for the message (the call takes `k`).
        let shown = self.proxy_key(key);
        let vp = self.any_out(v);
        let val = Place::local(self.copy_to_temp(Operand::Copy(vp), avt));
        let r = self.proxy_receiver(data, ty);
        let res = Place::local(self.temp(avt));
        let args = vec![(k, any), (val, any), (r, any)];
        if !self.proxy_invoke("__set", data, ty, args, &res) {
            self.drop_glue(shown, any);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let b = self.cx.intern(TyKind::Bool);
        let ok = self.any_unbox(&res, any, b);
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        // The key is a boxed string (`proxy_key`): its data is the string's address.
        let ka = Operand::Copy(proj(&shown, vir::Proj::Field(0)));
        self.panic_with_key(
            "TypeError: 'set' on proxy: trap returned falsish for property '",
            &ka,
            "'",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
        self.drop_glue(shown, any);
        self.ret_status(ST_OK);
    }

    /// Drop every argument of `argv`.
    fn any_drop_args_all(&mut self, argv: vir::Local, argc: vir::Local) {
        self.any_drop_args(argv, 0, argc);
    }
}
