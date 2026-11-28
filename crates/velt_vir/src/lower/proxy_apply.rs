//! Calling a proxy (#3978; hir_encodings.md "Proxy and Reflect"). A proxy of a function is a
//! function, as in JavaScript: `typeof p` is `"function"` (`[PROXY_IS_FUNCTION]`), and a call
//! runs the handler's `apply` trap with the target, `this` and the arguments as an array, or,
//! without one, calls the target with the same `this` and arguments. A proxy of anything else
//! is no function (Node's `p is not a function`).
//!
//! `[PROXY_CALLEE]` (std/lang/proxy/index.velt) walks the chain of proxies to what the call runs:
//! `null` when a proxy on it has an `apply` trap, which `[PROXY_APPLY](thisArg, args)` runs; else
//! the function at its end, called through its own slots, as a plain call (`AnyApply`) or with
//! `this` (`AnyApplyThis`, which only a `ThisFn` reads); `undefined` (the target is no function)
//! answers `ST_NOT_FN` from its slot.
//!
//! | call | `this` |
//! |---|---|
//! | `p(args)` (the proxy's `AnyApply` slot) | `undefined` |
//! | `o.p(args)`, `o[k](args)` of a proxy member (`this_fn_method_call`, `AnyApplyThis`) | `o` |

use velt_sema::hir::TyId;

use super::any::TAG_NULL;
use super::any_member::{SLOT_APPLY, ST_OK, ST_UNSUPPORTED};
use super::glue::VtableKey;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Terminator, Ty};

/// The `this` of a call of a proxy.
pub(super) enum ProxyThis {
    /// A plain call: `undefined`.
    Undefined,
    /// The address of a borrowed `any` (a glue parameter).
    Borrowed(vir::Local),
    /// An owned `any`, which the call consumes.
    Owned(Place),
}

impl FnLower<'_, '_> {
    /// Whether the `any` descriptor `desc` is a proxy's (`pty`: `Proxy`).
    pub(super) fn desc_is_proxy(&mut self, desc: Operand, pty: TyId) -> Operand {
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let pdesc = self.vtable_addr(VtableKey::Impl(imp, pty));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, pdesc))
    }

    /// The call of the proxy at `data` (of type `ty`: `Proxy`) with `this` and the owned
    /// arguments at `argv`, its result into `*out`; `drop` (the value the proxy was read as, owned)
    /// is dropped after it. Ends the block, returning the call's status (module docs).
    pub(super) fn proxy_apply_call(
        &mut self,
        (data, ty): (vir::Local, TyId),
        this: ProxyThis,
        (argv, argc, out): (vir::Local, vir::Local, vir::Local),
        drop: Option<(&Place, TyId)>,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let c = Place::local(self.temp(avt));
        if !self.proxy_invoke(velt_common::std_key!(proxy_callee), data, ty, vec![], &c) {
            if let ProxyThis::Owned(t) = this {
                self.drop_glue(t, any);
            }
            if let Some((p, t)) = drop {
                self.drop_glue(p.clone(), t);
            }
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let (cd, cdesc) = self.any_parts(Operand::Copy(c.clone()), any);
        let tag = self.any_tag_of(cdesc.clone());
        let null = cint(TAG_NULL as i128, Ty::U64);
        let trapped = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, null));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(trapped, yes, no);
        // An `apply` trap on the chain: `[PROXY_APPLY](thisArg, args)`.
        self.switch_to(yes);
        let tv = match &this {
            ProxyThis::Owned(t) => t.clone(),
            ProxyThis::Borrowed(a) => {
                let held = self.deref_param(*a, any);
                let v = self.share_value(Operand::Copy(held), any);
                Place::local(self.copy_to_temp(v, avt))
            }
            ProxyThis::Undefined => {
                let imp = self.cx.hir.any_impl.unwrap_or(0);
                let u = self.dynamic_undefined_moved(imp, any);
                Place::local(self.copy_to_temp(u, avt))
            }
        };
        let (arr, arr_ty) = self.argv_array(argv, argc);
        let outp = self.any_out(out);
        let args = vec![(tv, any), (arr, arr_ty)];
        let st = match self.proxy_invoke(velt_common::std_key!(proxy_apply), data, ty, args, &outp) {
            true => ST_OK,
            false => ST_UNSUPPORTED,
        };
        self.drop_glue(c.clone(), any);
        if let Some((p, t)) = drop {
            self.drop_glue(p.clone(), t);
        }
        self.ret_status(st);
        // A function (or `undefined`): its own call, with the same `this`.
        self.switch_to(no);
        let [a0, a1, a2] = [argv, argc, out].map(|l| Operand::Copy(Place::local(l)));
        let this_addr = match &this {
            ProxyThis::Owned(t) => Some(self.addr(t.clone())),
            ProxyThis::Borrowed(a) => Some(Operand::Copy(Place::local(*a))),
            ProxyThis::Undefined => None,
        };
        let via_this = this_addr.and_then(|t| {
            let parts = (cd.clone(), cdesc.clone(), t);
            self.any_apply_this_call(parts, (a0.clone(), a1.clone(), a2.clone()))
        });
        let st = match via_this {
            Some(st) => st,
            None => {
                let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
                self.any_slot(cdesc, SLOT_APPLY, vec![cd, a0, a1, a2], p)
            }
        };
        self.drop_glue(c, any);
        if let ProxyThis::Owned(t) = this {
            self.drop_glue(t, any);
        }
        if let Some((p, t)) = drop {
            self.drop_glue(p.clone(), t);
        }
        self.terminate(Terminator::Return(st));
    }

    /// In the `AnyCall` slot of a proxy (`data`, of type `ty`): `p.call(...)`, `p.apply(...)` and
    /// `p.bind(...)` of a proxy whose call runs an `apply` trap (`[PROXY_CALLEE]` is `null`) are
    /// `[PROXY_FN_METHOD](key, args)`, which calls the proxy. Continues in the block for the other
    /// keys and proxies, whose `call` reaches the target.
    pub(super) fn proxy_fn_keys(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let names: Vec<&str> = (["call", "apply", "bind"].into_iter())
            .filter(|n| self.cx.any_called(n))
            .collect();
        if names.is_empty() {
            return;
        }
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let rest = self.new_block();
        for name in names {
            let next = self.on_key(key, name);
            let c = Place::local(self.temp(avt));
            if !self.proxy_invoke(velt_common::std_key!(proxy_callee), data, ty, vec![], &c) {
                self.goto(rest);
                self.switch_to(next);
                continue;
            }
            let (_, cdesc) = self.any_parts(Operand::Copy(c.clone()), any);
            let tag = self.any_tag_of(cdesc);
            let null = cint(TAG_NULL as i128, Ty::U64);
            let trapped = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, null));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(trapped, yes, no);
            self.switch_to(yes);
            let k = self.proxy_key(super::any_proxy::ProxyKey::Str(key));
            let (arr, arr_ty) = self.argv_array(argv, argc);
            let outp = self.any_out(out);
            let args = vec![(k, any), (arr, arr_ty)];
            let m = velt_common::std_key!(proxy_fn_method);
            let st = match self.proxy_invoke(m, data, ty, args, &outp) {
                true => ST_OK,
                false => ST_UNSUPPORTED,
            };
            self.ret_status(st);
            self.switch_to(no);
            self.drop_glue(c, any);
            self.goto(rest);
            self.switch_to(next);
        }
        self.goto(rest);
        self.switch_to(rest);
    }

    /// In an `AnyCall` slot, after reading the member `f` (owned, of the dynamic type `fty`; `any`
    /// parts `fd`, `fdesc`) for a call on the object `data` (of type `pty`): when `f` is a proxy,
    /// its call with the object as `this`. Continues in the block for any other `f`.
    pub(super) fn proxy_method_call(
        &mut self,
        (f, fty, fd, fdesc): (&Place, TyId, &Operand, &Operand),
        (data, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local),
        pty: TyId,
    ) {
        let Some(proxy) = self.cx.prelude_proxy_ty() else {
            return;
        };
        let is_p = self.desc_is_proxy(fdesc.clone(), proxy);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_p, yes, no);
        self.switch_to(yes);
        let fl = self.copy_to_temp(fd.clone(), Ty::Ptr);
        let r = self.proxy_receiver(data, pty);
        let call = (argv, argc, out);
        self.proxy_apply_call((fl, proxy), ProxyThis::Owned(r), call, Some((f, fty)));
        self.switch_to(no);
    }

    /// In `AnyApplyThis`'s glue, for a value at `data` with the descriptor `desc` and `this` at
    /// the address `this`: when it is a proxy, its call with that `this`. Continues in the block
    /// for any other value.
    pub(super) fn proxy_apply_this(
        &mut self,
        (data, desc, this): (vir::Local, Operand, vir::Local),
        (argv, argc, out): (vir::Local, vir::Local, vir::Local),
    ) {
        let Some(proxy) = self.cx.prelude_proxy_ty() else {
            return;
        };
        let is_p = self.desc_is_proxy(desc, proxy);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_p, yes, no);
        self.switch_to(yes);
        let call = (argv, argc, out);
        self.proxy_apply_call((data, proxy), ProxyThis::Borrowed(this), call, None);
        self.switch_to(no);
    }

    /// `typeof` of an `any` whose tag says `"object"` (`data`, `desc`), into `out`: `"function"`
    /// for a proxy of a function (`[PROXY_IS_FUNCTION]`), in a program that has `Proxy`. Continues
    /// in the block for any other value.
    pub(super) fn proxy_typeof(&mut self, (data, desc): (Operand, Operand), out: &Place, done: vir::BlockId) {
        let Some(proxy) = self.cx.prelude_proxy_ty() else {
            return;
        };
        let is_p = self.desc_is_proxy(desc, proxy);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_p, yes, no);
        self.switch_to(yes);
        let dl = self.copy_to_temp(data, Ty::Ptr);
        let key = velt_common::std_key!(proxy_is_function);
        let Some(is_fn) = self.proxy_invoke_typed(key, dl, proxy) else {
            self.goto(no);
            self.switch_to(no);
            return;
        };
        let fun = self.new_block();
        self.branch(is_fn, fun, no);
        self.switch_to(fun);
        let s = self.str_lit("function");
        self.assign(out.clone(), Rvalue::Use(s));
        self.goto(done);
        self.switch_to(no);
    }
}
