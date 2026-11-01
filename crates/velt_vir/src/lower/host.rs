//! Calls of host functions (`declare host("m") function`, https://veltlang.com/docs/reference/host-functions/):
//! the call of the WebAssembly import the declaration names, with each argument in the form
//! the JS glue converts (`vir::HostImport`'s module docs). A byte buffer gives its data pointer
//! and length (an array's or a view's, as for system functions: system_views.rs), a string the
//! address of its value, a `HostRef` its slot (std's hooks in std/host/host_ref.vlt read and
//! make them). On a target without a JS host a call is an error, reported once lowering is done
//! (system.rs `unavailable_diagnostics`).
//!
//! The call of an async host function is the import, which calls the JS function and returns the
//! id of its promise, then the promise leaf of that id (`velt_rt_host_promise`), which yields
//! std's `HostSettled<W>` (std/host/promise.vlt), and the promise of std's hook awaiting it: it
//! throws an `IoError` for a rejection and makes the value (a `HostRef` of a slot).

use velt_sema::hir::{self, DefId, HostTy, IntTy, TyId, TyKind};

use super::rt::Rt;
use super::system::Arg;
use super::{ice, unit, Cx, FnLower};
use crate::vir::{self, HostImport, HostValue, Operand, Place, Rvalue, Ty};

/// std's hooks that make a `HostRef` of a slot and read the slot of one.
const REF_OF: &str = velt_common::std_hook!(host_ref_of);
const REF_OR_NULL: &str = velt_common::std_hook!(host_ref_or_null);
const REF_HANDLE: &str = velt_common::std_hook!(host_ref_handle);

/// What a host promise's leaf yields, and std's hooks awaiting it.
const SETTLED: &str = velt_common::std_hook!(host_settled);
const AWAIT: &str = velt_common::std_hook!(host_await);
const AWAIT_VOID: &str = velt_common::std_hook!(host_await_void);
const AWAIT_REF: &str = velt_common::std_hook!(host_await_ref);
const AWAIT_REF_OR_NULL: &str = velt_common::std_hook!(host_await_ref_or_null);

/// Does `target` run under a JS host, which provides host functions? Only the browser flavor of
/// WebAssembly does; native targets and WASI hosts do not.
pub(super) fn js_host(target: &str) -> bool {
    target.trim() == "wasm32-unknown-unknown"
}

/// What crosses for `t`, as the glue knows it.
fn value(t: HostTy) -> HostValue {
    match t {
        HostTy::Int(i) => match i {
            IntTy::I8 => HostValue::I8,
            IntTy::I16 => HostValue::I16,
            IntTy::I32 => HostValue::I32,
            IntTy::I64 | IntTy::ISize => HostValue::I64,
            IntTy::U8 => HostValue::U8,
            IntTy::U16 => HostValue::U16,
            IntTy::U32 => HostValue::U32,
            IntTy::U64 | IntTy::USize => HostValue::U64,
        },
        HostTy::F64 => HostValue::F64,
        HostTy::Bool => HostValue::Bool,
        HostTy::Str => HostValue::Str,
        HostTy::Bytes { readonly: false } => HostValue::Bytes,
        HostTy::Bytes { readonly: true } => HostValue::ReadonlyBytes,
        HostTy::Ref { nullable: false } => HostValue::Ref,
        HostTy::Ref { nullable: true } => HostValue::NullableRef,
        HostTy::Void => HostValue::Void,
    }
}

impl FnLower<'_, '_> {
    /// A call of the host function `def` (an `ExternFnDef` with `host`).
    pub(super) fn host_call(&mut self, def: DefId, args: &[hir::Expr]) -> Operand {
        let hir::Def::ExternFn(x) = self.cx.hir.def(def) else {
            ice("not an extern")
        };
        let Some(h) = x.host.clone() else {
            ice("not a host function")
        };
        let (name, ret, is_async) = (x.symbol.clone(), x.ret, x.is_async);
        if !js_host(&self.cx.target) {
            self.cx
                .unavailable
                .push((def, self.cx.cur_def, self.cx.user_site));
            for a in args {
                self.system_arg_effects(a);
            }
            return self.unavailable_result(ret);
        }
        // Every argument first (JS order), then the headers of the buffers and the slots of the
        // references: a later argument may grow an array an earlier one names.
        let callee = format!("host function `{name}`");
        let laters = self.later_each(args);
        let mut vals = Vec::with_capacity(args.len());
        for (i, ((a, &t), later)) in args.iter().zip(&h.params).zip(laters).enumerate() {
            let what = format!("argument {}", i + 1);
            let view = matches!(t, HostTy::Bytes { .. })
                .then(|| self.system_view(&callee, &what, a, later))
                .flatten();
            if let Some(v) = view {
                vals.push(v);
                continue;
            }
            let ty = self.sub(a.ty);
            let scalar = matches!(t, HostTy::Int(_) | HostTy::F64 | HostTy::Bool);
            if !scalar {
                self.note_held_buffer(a, later);
            }
            let v = self.expr(a);
            vals.push(match scalar {
                true => Arg::Scalar(self.freeze(v, a.ty)),
                false => {
                    let v = self.hold_buffer(v, a.ty, later);
                    let vt = self.cx.ty(ty);
                    Arg::Value(self.operand_place(v, vt), ty)
                }
            });
        }
        let (mut argv, mut params) = (vec![], vec![]);
        for (v, &t) in vals.into_iter().zip(&h.params) {
            self.host_arg(v, t, &mut argv, &mut params);
        }
        let import = HostImport {
            module: h.module.clone(),
            name,
            params: h.params.iter().map(|&t| value(t)).collect(),
            ret: value(h.ret),
            is_async,
        };
        if is_async {
            return self.host_promise(import, h.ret, argv, params, ret);
        }
        self.host_result(import, h.ret, argv, params, ret)
    }

    /// The call of an async host function (module docs): a promise of type `ret`.
    fn host_promise(
        &mut self,
        mut import: HostImport,
        t: HostTy,
        argv: Vec<Operand>,
        params: Vec<Ty>,
        ret: TyId,
    ) -> Operand {
        if import.ret == HostValue::ReadonlyBytes {
            import.ret = HostValue::Bytes;
        }
        let start = self.cx.host_extern(import, params, Ty::U32);
        let id = self.temp(Ty::U32);
        self.call(
            vir::Callee::Extern(start),
            argv,
            Some(Place::local(id)),
            false,
        );
        let leaf = self.temp(Ty::Ptr);
        let args = vec![Operand::Copy(Place::local(id))];
        self.call_rt(Rt::HostPromise, args, Some(Place::local(leaf)));
        let ret = self.sub(ret);
        let (hook, targs, wire) = match t {
            HostTy::Void => (AWAIT_VOID, vec![], self.cx.intern(TyKind::Bool)),
            HostTy::Ref { nullable } => {
                let hook = if nullable {
                    AWAIT_REF_OR_NULL
                } else {
                    AWAIT_REF
                };
                (hook, vec![], self.cx.intern(TyKind::Int(IntTy::U32)))
            }
            _ => {
                let value = self.cx.promise_result(ret);
                (AWAIT, vec![value], value)
            }
        };
        let settled = hook_def(self.cx, SETTLED);
        let slot = self.cx.intern(TyKind::Adt(settled, vec![wire]));
        let never = self.cx.intern(TyKind::Never);
        let leaf_ty = self.cx.intern(TyKind::Promise(slot, never));
        let mut fut = Operand::Copy(Place::local(leaf));
        if self.cx.foreign_differs(slot) {
            fut = self.adopt_foreign_promise(fut, leaf_ty);
            if let Operand::Copy(p) = &fut {
                self.take_temp(p);
            }
        }
        let f = self
            .cx
            .func_for_direct(hook_def(self.cx, hook), targs, self.mode);
        let out = self.temp(Ty::Ptr);
        self.call(
            vir::Callee::Func(f),
            vec![fut],
            Some(Place::local(out)),
            false,
        );
        self.owned_result(Some(out), ret)
    }

    /// Append the operands of one argument (`v`, crossing as `t`) and their VIR types.
    fn host_arg(&mut self, v: Arg, t: HostTy, argv: &mut Vec<Operand>, params: &mut Vec<Ty>) {
        match (t, v) {
            (HostTy::Bytes { .. }, v) => {
                let (ptr, n) = self.buffer(Some(v));
                let n = self.rvalue_temp(Ty::U32, Rvalue::Cast(n, Ty::U32));
                argv.extend([ptr, n]);
                params.extend([Ty::Ptr, Ty::U32]);
            }
            (HostTy::Str, Arg::Value(place, _)) => {
                argv.push(self.addr(place));
                params.push(Ty::Ptr);
            }
            (HostTy::Ref { .. }, Arg::Value(place, _)) => {
                let slot = self.hook_call(REF_HANDLE, Operand::Copy(place), Ty::U32);
                argv.push(slot);
                params.push(Ty::U32);
            }
            (HostTy::Int(i), Arg::Scalar(v)) => {
                argv.push(v);
                params.push(super::types::int_ty(i));
            }
            (HostTy::F64, Arg::Scalar(v)) => {
                argv.push(v);
                params.push(Ty::F64);
            }
            (HostTy::Bool, Arg::Scalar(v)) => {
                argv.push(v);
                params.push(Ty::Bool);
            }
            _ => ice("host call: an argument of another form"),
        }
    }

    /// The call itself and its result: a string or bytes through an out-pointer
    /// ([`Self::finish_call`]), a `HostRef` made from the slot the host returned.
    fn host_result(
        &mut self,
        mut import: HostImport,
        t: HostTy,
        argv: Vec<Operand>,
        mut params: Vec<Ty>,
        ret: TyId,
    ) -> Operand {
        match t {
            HostTy::Str | HostTy::Bytes { .. } => {
                // `readonly u8[]` is `u8[]` as a result: new bytes either way.
                if import.ret == HostValue::ReadonlyBytes {
                    import.ret = HostValue::Bytes;
                }
                params.push(Ty::Ptr);
                let id = self.cx.host_extern(import, params, Ty::Unit);
                self.out_result(id, argv, ret)
            }
            HostTy::Void => {
                let id = self.cx.host_extern(import, params, Ty::Unit);
                self.call(vir::Callee::Extern(id), argv, None, false);
                unit()
            }
            HostTy::Ref { nullable } => {
                let id = self.cx.host_extern(import, params, Ty::U32);
                let slot = self.temp(Ty::U32);
                self.call(
                    vir::Callee::Extern(id),
                    argv,
                    Some(Place::local(slot)),
                    false,
                );
                let hook = if nullable { REF_OR_NULL } else { REF_OF };
                let r = self.hook_call(hook, Operand::Copy(Place::local(slot)), Ty::Ptr);
                let Operand::Copy(p) = r else {
                    ice("a hook's result is a temporary")
                };
                self.owned_result(Some(p.local), ret)
            }
            _ => {
                let vt = self.cx.ty(ret);
                let id = self.cx.host_extern(import, params, vt);
                let out = self.temp(vt);
                self.call(
                    vir::Callee::Extern(id),
                    argv,
                    Some(Place::local(out)),
                    false,
                );
                Operand::Copy(Place::local(out))
            }
        }
    }

    /// The call of `id` with a string or byte array result, which the runtime writes to the
    /// trailing out-pointer in its own layout (an array the program keeps in a box is adopted
    /// from it, as for a runtime function's: foreign.rs).
    fn out_result(&mut self, id: vir::ExternId, mut argv: Vec<Operand>, ret: TyId) -> Operand {
        let rt = self.sub(ret);
        let fty = self.cx.foreign_ty(rt);
        let out = self.temp(fty);
        argv.push(self.addr(Place::local(out)));
        self.call(vir::Callee::Extern(id), argv, None, false);
        if !self.cx.foreign_differs(rt) {
            return self.owned_result(Some(out), rt);
        }
        let vt = self.cx.ty(rt);
        let native = self.temp(vt);
        self.adopt_foreign(&Place::local(out), rt, &Place::local(native));
        self.owned_result(Some(native), rt)
    }

    /// A call of std's `HostRef` hook `qualified` with the one argument `arg`, into a temporary
    /// of type `ret`.
    fn hook_call(&mut self, qualified: &str, arg: Operand, ret: Ty) -> Operand {
        let d = hook_def(self.cx, qualified);
        let f = self.cx.func_for_direct(d, vec![], self.mode);
        let out = self.temp(ret);
        self.call(
            vir::Callee::Func(f),
            vec![arg],
            Some(Place::local(out)),
            false,
        );
        Operand::Copy(Place::local(out))
    }

    /// A value standing for the result of a call that is an error on this target (the program
    /// does not compile).
    fn unavailable_result(&mut self, ret: TyId) -> Operand {
        match self.cx.ty(ret) {
            Ty::Unit => unit(),
            vt => Operand::Copy(Place::local(self.temp(vt))),
        }
    }
}

impl Cx<'_> {
    /// Is `def` an async host function (whose call returns the promise of std's hook)?
    pub(super) fn is_async_host(&self, def: DefId) -> bool {
        matches!(self.hir.def(def), hir::Def::ExternFn(x) if x.host.is_some() && x.is_async)
    }
}

/// The definition of std's host hook `qualified` (velt/host is checked whole when loaded).
fn hook_def(cx: &Cx<'_>, qualified: &str) -> DefId {
    cx.prelude_def(qualified)
        .unwrap_or_else(|| ice(format_args!("no `{qualified}` in velt/host")))
}
