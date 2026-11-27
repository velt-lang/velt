//! `message` of an error read through `any`, in a program where a class extending `Error`
//! declares `get message()` (velt_sema `error_message.rs`): the hook `errorMessage(e)`, the own
//! message unless the error was made without one, else the getter's, as a typed read is.

use velt_sema::hir::TyId;

use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::FnLower;
use crate::vir::{Operand, Place};

/// The member name of `Error`'s accessor the getters override.
const KEY: &str = velt_common::std_key!(error_message);

impl FnLower<'_, '_> {
    /// Is `message` of a value of class type `ty` read through `errorMessage`: `ty` extends the
    /// prelude's `Error`, and `Error`'s accessor has a vtable slot, which it has only where a
    /// class overrides it?
    pub(super) fn error_message_hooked(&mut self, ty: TyId) -> bool {
        let Some(root) = self.error_root(ty) else {
            return false;
        };
        let vtable = self.cx.adt_def(root).vtable.clone();
        vtable
            .iter()
            .any(|&m| self.cx.fn_def(m).name.ends_with(KEY))
    }

    /// Boxes `errorMessage(e)` of the error at `value` (of class type `ty`) into `out`.
    pub(super) fn any_get_error_message(&mut self, value: &Place, out: &Place) {
        let e = ty::Error::arg(Operand::Copy(value.clone()));
        let text = hooks::error_message(self, Construct::ErrorMessage, e).emit_std(self);
        let str_ty = self.cx.str_ty();
        self.any_box_with(text, str_ty, out, false);
    }
}

impl FnLower<'_, '_> {
    /// `console.log`'s text of the `message` of the error at `value` (a class whose reads are
    /// [`hooked`](Self::error_message_hooked)), at depth `child`: `errorMessage(e)`, so an error
    /// made without a message shows the getter's text, as `e.message` gives it.
    pub(super) fn format_error_message(&mut self, buf: &Operand, value: &Place, child: &Operand) {
        // The glue has no scope of its own, which the call needs; its result is this code's,
        // dropped once printed.
        self.push_scope(super::ScopeKind::Block);
        let e = ty::Error::arg(Operand::Copy(value.clone()));
        let text = hooks::error_message(self, Construct::ErrorMessage, e).emit_std(self);
        let str_ty = self.cx.str_ty();
        let tp = self.place_of(text, str_ty);
        self.format_nested(buf, &tp, str_ty, child);
        self.drop_glue(tp, str_ty);
        self.pop_scope();
    }
}
