//! `o.m?.(args)` where `m` is a method of `o`'s class or interface: a method is never nullish,
//! so JavaScript calls it as `o.m(args)` does, and so does Velt (`chunks.return?.(undefined)`
//! on an `AsyncGenerator`, whose `return` is a method). An optional field holding a function
//! (`o.f?.(x)`) keeps the optional call.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::TyKind;

impl FnCx<'_, '_> {
    /// Is `callee` `o.m` (or `o?.m`) with `m` an instance method (not a getter) of the class or
    /// interface `o` holds?
    pub(super) fn is_method_member(&mut self, callee: &ast::Expr) -> bool {
        let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
            return false;
        };
        let Some(t) = self.peek_ty(object) else {
            return false;
        };
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(d, args) => {
                let found = crate::collect::lookup_method(self.cx, d, &args, &prop.name);
                let Some(crate::collect::Found::Class { m, .. }) = found else {
                    return matches!(found, Some(crate::collect::Found::Default { .. }));
                };
                !m.is_static && self.cx.try_fn(m.def).is_some_and(|f| !f.is_getter)
            }
            TyKind::Dyn(d, _) => self.cx.iface(d).is_some_and(|i| {
                i.methods
                    .iter()
                    .any(|m| m.name == prop.name && !m.is_getter)
            }),
            _ => false,
        }
    }
}
