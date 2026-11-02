//! `Array.prototype[key]` read through `any` (#942, #823): `(Array.prototype as any)[method]`,
//! which libraries wrap and call with an explicit `this`
//! (`original.apply(this, arguments)`). A member read of the prelude's `ObjectPrototype` naming
//! `Array.prototype` asks std's hook: in a program that has loaded `Proxy`,
//! `arrayPrototypeMember(key)` (std/lang/proxy/array_apply.vlt), whose array method is an
//! `ArrayMethod` that `apply` and `call` run on the `this` they pass (on a proxy through its
//! traps, on an array as its own method); else, in a program with function expressions taking
//! `this`, `arrayPrototypeThisFn(key)` (std/lang/this_fn/array_methods.vlt), a `ThisFn` running
//! the method on the array it is called on. Any other prototype or key, and other programs,
//! read `undefined` as before.

use velt_sema::hir::{PassMode, TyId};

use super::any_member::ST_OK;
use super::FnLower;
use crate::vir::{self, Operand, Place, Rvalue, Ty};

/// The hook that reads a member of `Array.prototype` in a program with `Proxy`.
const PROTOTYPE_MEMBER: &str = velt_common::std_hook!(array_prototype_member);
/// The hook that reads one in a program with function expressions taking `this`.
const PROTOTYPE_THIS_FN: &str = velt_common::std_hook!(array_prototype_this_fn);

/// The name of the prelude's prototype class (std/prelude/object_prototype.vlt).
const OBJECT_PROTOTYPE: &str = "::ObjectPrototype";

impl FnLower<'_, '_> {
    /// In the `AnyGet` slot of the type `ty`, once no field or method matched the key: when `ty`
    /// is `ObjectPrototype` and the prototype at `data` is `Array.prototype`, the hook's value
    /// into `out` (module docs). Continues in the block for anything else.
    pub(super) fn array_prototype_member(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        out: &Place,
        ty: TyId,
    ) {
        let Some((d, _)) = self.plain_adt(ty) else {
            return;
        };
        let a = self.cx.adt_def(d);
        let prototype = a.name.starts_with(velt_common::std_dir!(prelude))
            && a.name.ends_with(OBJECT_PROTOTYPE);
        let Some(i) = a
            .fields
            .iter()
            .position(|f| f.name == "array")
            .filter(|_| prototype)
        else {
            return;
        };
        let hook = self.cx.prelude_def(PROTOTYPE_MEMBER);
        let Some(h) = hook.or_else(|| self.cx.prelude_def(PROTOTYPE_THIS_FN)) else {
            return;
        };
        let vp = self.any_value_place(data, ty);
        let fp = self.field_place(&vp, ty, i as u32);
        let is_array = self.rvalue_temp(Ty::Bool, Rvalue::Use(Operand::Copy(fp)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_array, yes, no);
        self.switch_to(yes);
        let k = match self.cx.fn_def(h).params[0].mode {
            PassMode::Owned => {
                let str_ty = self.cx.str_ty();
                let kp = self.deref_param(key, str_ty);
                let ks = self.share_value(Operand::Copy(kp), str_ty);
                let t = self.copy_to_temp(ks, Ty::Agg(vir::STR_AGG));
                self.addr(Place::local(t))
            }
            _ => Operand::Copy(Place::local(key)),
        };
        let r = self.call_std_hook(h, vec![], vec![k]);
        self.assign(out.clone(), Rvalue::Use(r));
        self.ret_status(ST_OK);
        self.switch_to(no);
    }
}
