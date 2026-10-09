//! Calls to a class's hook methods from glue (`hir::AdtDef::to_string`, `to_json`, `inspect`):
//! `String(x)` calls `toString()`, `JSON.stringify` `toJSON()`, `console.log` `__inspect()`.
//! Sema only records a method that cannot throw and takes no arguments but `this` (and the key,
//! for `toJSON(key)`); it gives other shapes an adapter of that form, which dispatches through
//! the vtable and passes the defaults (sema's `hooks.rs`). So a call is a plain call with the
//! object pointer as `this`, and its result an owned value the caller drops.

use velt_sema::hir::{DefId, PassMode, TyId, TyKind};

use super::{ice, FnLower};
use crate::vir::{self, Operand, Place, Ty};

/// Which hook of a class.
#[derive(Clone, Copy)]
pub(super) enum Hook {
    ToString,
    ToJson,
    Inspect,
}

impl FnLower<'_, '_> {
    /// Class type `ty`'s method for `hook`, if it has one.
    pub(super) fn hook(&self, ty: TyId, hook: Hook) -> Option<DefId> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        if !self.cx.is_class(ty) {
            return None;
        }
        let a = self.cx.adt_def(d);
        match hook {
            Hook::ToString => a.to_string,
            Hook::ToJson => a.to_json,
            Hook::Inspect => a.inspect,
        }
    }

    /// The result type of hook method `m` called on a `cls` value.
    pub(super) fn hook_result(&mut self, m: DefId, cls: TyId) -> TyId {
        let targs = self.cx.method_targs(m, cls);
        let ret = self.cx.fn_def(m).ret;
        self.cx.subst(ret, &targs)
    }

    /// Call hook method `m` on the class object `obj` (of type `cls`), passing the string at
    /// `key` when it takes the key: the place of its owned result and the result's type. The
    /// caller drops the result (`drop_glue`) once it is used.
    pub(super) fn call_hook(
        &mut self,
        m: DefId,
        obj: Operand,
        cls: TyId,
        key: Option<&Place>,
    ) -> (Place, TyId) {
        let ret = self.hook_result(m, cls);
        let targs = self.cx.method_targs(m, cls);
        let modes: Vec<PassMode> = self.cx.fn_def(m).params.iter().map(|p| p.mode).collect();
        // A method that keeps `this` (`new BodyStream(this)`) takes it owned: give it a
        // reference of its own, as a call passing a borrowed value does.
        let obj = match modes.first() {
            Some(PassMode::Owned) => self.share_value(obj, cls),
            _ => obj,
        };
        let mut args = vec![obj];
        if let Some(&mode) = modes.get(1) {
            let key = key.unwrap_or_else(|| ice("toJSON hook called without its key"));
            let str_ = self.cx.str_ty();
            let a = match mode {
                PassMode::Owned => {
                    let v = self.share_value(Operand::Copy(key.clone()), str_);
                    let t = self.copy_to_temp(v, Ty::Agg(crate::vir::STR_AGG));
                    self.addr(Place::local(t))
                }
                _ => self.addr(key.clone()),
            };
            args.push(a);
        }
        let f = self.cx.func_for(m, targs);
        let abi = self.cx.ret_abi(ret, None);
        if let Some(out) = abi.out {
            let tmp = self.temp(out);
            let a = self.addr(Place::local(tmp));
            args.push(a);
            self.call(vir::Callee::Func(f), args, None, false);
            return (Place::local(tmp), ret);
        }
        let d = self.temp(abi.ret);
        let dest = (abi.ret != Ty::Unit).then(|| Place::local(d));
        self.call(vir::Callee::Func(f), args, dest, false);
        (Place::local(d), ret)
    }
}
