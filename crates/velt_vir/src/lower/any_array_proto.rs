//! `Array.prototype[key]` read through `any` (#942, #823): `(Array.prototype as any)[method]`,
//! which libraries wrap and call with an explicit `this`
//! (`original.apply(this, arguments)`). In a program that has loaded `Proxy`, a member read of
//! the prelude's `ObjectPrototype` naming `Array.prototype` asks std's hook
//! `arrayPrototypeMember(key)` (std/lang/proxy/array_apply.velt): an array method is an
//! `ArrayMethod`, whose `apply`, `call` and `bind` run it on the `this` they pass, through the
//! traps of a proxy of an array, or of a proxy without traps around an array. Any other
//! prototype or key, and a program without `Proxy`, read `undefined` as before.

use velt_sema::hir::TyId;

use super::any_member::ST_OK;
use super::hook_calls::{self as hooks, ty, HookTy};
use super::FnLower;
use crate::vir::{self, Operand, Place, Rvalue, Ty};

/// The hook that reads a member of `Array.prototype` in a program with `Proxy` (its call:
/// `hook_calls.rs`).
const PROTOTYPE_MEMBER: &str = velt_common::std_hook!(array_prototype_member);

/// The name of the prelude's prototype class (std/prelude/object_prototype.velt).
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
        // A program without `Proxy` has no such hook, and no such key to read.
        if self.cx.prelude_def(PROTOTYPE_MEMBER).is_none() {
            return;
        }
        let vp = self.any_value_place(data, ty);
        let fp = self.field_place(&vp, ty, i as u32);
        let is_array = self.rvalue_temp(Ty::Bool, Rvalue::Use(Operand::Copy(fp)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_array, yes, no);
        self.switch_to(yes);
        let str_ty = self.cx.str_ty();
        let kp = self.deref_param(key, str_ty);
        let r = hooks::array_prototype_member(self, ty::Str::shared(kp)).emit_std(self);
        self.assign(out.clone(), Rvalue::Use(r));
        self.ret_status(ST_OK);
        self.switch_to(no);
    }
}
