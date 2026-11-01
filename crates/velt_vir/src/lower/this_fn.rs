//! Calls of a function whose `this` is bound at the call: std/lang/this_fn/index.vlt's `ThisFn`, the
//! value of a function expression with a `this: any` parameter (sema `this_fn_expr.rs`). Held
//! as an `any` (a `Function`), it is a function: `typeof` is `"function"` (`Cx::is_function_object`),
//! a plain call `f(x)` (its `AnyApply` slot, any_glue.rs) is `[THIS_FN_APPLY](undefined, [x])`, and a method call
//! through a proxy or an `any` object (`proxy_call_body`, `any_call_body`) is
//! `[THIS_FN_APPLY](receiver, args)` with that object as `this`.
//! Its `call`, `apply` and `bind` are methods of the class. A program without such a function
//! expression doesn't load the class, and its code is unchanged.

use velt_sema::hir::{self, TyId, TyKind};

use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::glue::VtableKey;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty};

/// The qualified name of std/lang/this_fn/index.vlt's class.
pub(super) const STD_THIS_FN: &str = velt_common::std_hook!(this_fn);

impl super::Cx<'_> {
    /// Is `t` std/lang/this_fn/index.vlt's `ThisFn` (module docs)?
    pub(super) fn is_this_fn(&self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = self.types.kind(t) else {
            return false;
        };
        matches!(self.hir.def(*d), hir::Def::Adt(a) if a.name == STD_THIS_FN)
    }
}

impl FnLower<'_, '_> {
    /// The type of `ThisFn`, when the program has it.
    fn this_fn_ty(&mut self) -> Option<TyId> {
        let d = self
            .cx
            .hir
            .defs
            .iter()
            .position(|d| matches!(d, hir::Def::Adt(a) if a.name == STD_THIS_FN))?;
        Some(self.cx.intern(TyKind::Adt(hir::DefId(d as u32), vec![])))
    }

    /// The `argc` arguments at `argv` (owned `any`s, one after the other) moved into an owned
    /// `any[]`, with its type.
    pub(super) fn argv_array(&mut self, argv: vir::Local, argc: vir::Local) -> (Place, TyId) {
        let any = self.cx.any_ty();
        let n = Operand::Copy(Place::local(argc));
        let arr = Place::local(self.inline_array_with_len(n.clone(), any));
        let (size, _) = self.stride(any);
        let bytes = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, n, cint(size as i128, Ty::U64)),
        );
        let first = self.elem_place(&arr, cint(0, Ty::U64), any);
        let dst = self.addr(first);
        self.mem_copy_dyn(dst, Operand::Copy(Place::local(argv)), bytes, false);
        let arr_ty = self.cx.intern(TyKind::Array(any));
        let arr = if self.cx.boxed(arr_ty) {
            let v = self.box_value(Operand::Copy(arr), arr_ty);
            Place::local(self.copy_to_temp(v, Ty::Ptr))
        } else {
            arr
        };
        (arr, arr_ty)
    }

    /// `[THIS_FN_APPLY](this, args)` on the `ThisFn` at `data` (of type `ty`) with the owned `this`
    /// and the arguments at `argv`, its result into `*out`; returns the status.
    fn this_fn_invoke(
        &mut self,
        data: vir::Local,
        ty: TyId,
        this: Place,
        (argv, argc, out): (vir::Local, vir::Local, vir::Local),
    ) -> i128 {
        let any = self.cx.any_ty();
        let (arr, arr_ty) = self.argv_array(argv, argc);
        let outp = self.any_out(out);
        let args = vec![(this, any), (arr, arr_ty)];
        match self.proxy_invoke(velt_common::std_key!(this_fn_apply), data, ty, args, &outp) {
            true => ST_OK,
            false => ST_UNSUPPORTED,
        }
    }

    /// In the `AnyCall` slot of a type `ty`, when it is `ThisFn`: `f.call(thisArg, ...args)` is
    /// `[THIS_FN_APPLY](thisArg, args)` and `f.bind(thisArg, ...args)` is `[THIS_FN_BIND](thisArg, args)` (a
    /// method's rest parameter is not packed through `any`). Continues in the block for the
    /// other keys.
    pub(super) fn this_fn_call_keys(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        if !self.cx.is_this_fn(ty) {
            return;
        }
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let arr_ty = self.cx.intern(TyKind::Array(any));
        for (name, method) in [
            ("call", velt_common::std_key!(this_fn_apply)),
            ("bind", velt_common::std_key!(this_fn_bind)),
        ] {
            let next = self.on_key(key, name);
            // `thisArg`: the first argument (moved out), else `undefined`.
            let this = Place::local(self.temp(avt));
            let some = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Gt,
                    Operand::Copy(Place::local(argc)),
                    cint(0, Ty::U64),
                ),
            );
            let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(some, yes, no);
            self.switch_to(yes);
            let first = self.any_arg_place(argv, cint(0, Ty::U64));
            self.assign(this.clone(), Rvalue::Use(Operand::Copy(first)));
            self.goto(join);
            self.switch_to(no);
            let imp = self.cx.hir.any_impl.unwrap_or(0);
            let u = self.dynamic_undefined(imp, any);
            self.assign(this.clone(), Rvalue::Use(u));
            self.goto(join);
            self.switch_to(join);
            let (arr, _) = self.any_rest_arg(argv, argc, 1, arr_ty);
            let outp = self.any_out(out);
            let args = vec![(this, any), (arr, arr_ty)];
            let st = match self.proxy_invoke(method, data, ty, args, &outp) {
                true => ST_OK,
                false => ST_UNSUPPORTED,
            };
            self.ret_status(st);
            self.switch_to(next);
        }
    }

    /// The `AnyApply` slot of `ThisFn` (`ty`): a plain call, with `undefined` as `this`.
    pub(super) fn this_fn_apply_body(
        &mut self,
        (data, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let u = self.dynamic_undefined(imp, any);
        let this = Place::local(self.copy_to_temp(u, avt));
        let st = self.this_fn_invoke(data, ty, this, (argv, argc, out));
        self.ret_status(st);
    }

    /// In an `AnyCall` slot, after reading the member `f` (owned, of the dynamic type `fty`;
    /// `any` parts `fd`, `fdesc`) for
    /// a call on the object `data` (of type `pty`; a proxy's `get` trap answered it): when `f`
    /// is a `ThisFn`, its call with the object as `this`. Continues in the block for any
    /// other `f`.
    pub(super) fn this_fn_method_call(
        &mut self,
        (f, fty, fd, fdesc): (&Place, TyId, &Operand, &Operand),
        (data, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local),
        pty: TyId,
    ) {
        let Some(t) = self.this_fn_ty() else {
            return;
        };
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let tdesc = self.vtable_addr(VtableKey::Impl(imp, t));
        let is_t = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, fdesc.clone(), tdesc));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_t, yes, no);
        self.switch_to(yes);
        let fl = self.copy_to_temp(fd.clone(), Ty::Ptr);
        let r = self.proxy_receiver(data, pty);
        let st = self.this_fn_invoke(fl, t, r, (argv, argc, out));
        self.drop_glue(f.clone(), fty);
        self.ret_status(st);
        self.switch_to(no);
    }
}
