//! An object literal's method that implements an overloaded interface method
//! (`crate::iface_overloads`): as in TypeScript, it must fit every signature, so its result is
//! `any` or a type every signature's result takes (TS2322 / TS2416). The method that takes every
//! form only fixes the parameters; its result is `any` when the signatures' results differ.

use velt_common::{Diagnostic, Span};

use super::iface_object::MethodSig;
use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};
use crate::overloads::sig_of;

impl FnCx<'_, '_> {
    /// Implementation `f` (a function value) of method `name` of an interface with `methods`,
    /// or an error value, reported, when its result doesn't fit one of `name`'s signatures
    /// (`name#k`).
    pub(super) fn overload_impl_results(
        &mut self,
        methods: &[MethodSig],
        name: &str,
        f: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let TyKind::FnPtr { ret, .. } = *self.cx.ty.kind(f.ty) else {
            return f;
        };
        if self.cx.dynamic(ret).is_some() || self.cx.ty.has_error(ret) {
            return f;
        }
        let sigs = (methods.iter()).filter(|m| sig_of(&m.name) == Some(name));
        let sigs: Vec<TyId> = sigs.map(|m| m.ret).collect();
        for want in sigs {
            if self.converts_to(ret, want) {
                continue;
            }
            let (r, w) = (self.cx.display(ret), self.cx.display(want));
            self.cx.error(
                Diagnostic::error(
                    format!("method `{name}` does not fit every signature of the interface's `{name}`: its result `{r}` does not convert to `{w}`"),
                    span,
                )
                .with_note("an implementation of an overloaded method returns a type every signature's result takes, or `any`, as in TypeScript (TS2416)"),
            );
            return self.error_expr(span);
        }
        f
    }
}
