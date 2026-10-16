//! `new Proxy(target, handler)` and `Reflect.*` (issue #823; the prelude's
//! std/prelude/zz_proxy.vlt, hir_encodings.md "Proxy and Reflect").
//!
//! A proxy is a dynamic value: the `Proxy` object `new Proxy(...)` builds is converted to `any`
//! at once, so every operation on it goes through the descriptor of the `Proxy` class, whose
//! slots call the handler's traps (velt_vir `lower/any_proxy.rs`). TypeScript gives the
//! expression the target's type `T`; a typed proxy (the tagged pointer of #823's design) comes
//! later, so a proxy converted straight to a typed value is a compile error that says so.
//!
//! The prelude's own dynamic operations do not count as the program using `any` members
//! (`FnCx::note_dyn_members`); constructing a `Proxy` or calling `Reflect` in the program's
//! own code does, here.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, DefId, ExprKind as H};

impl FnCx<'_, '_> {
    /// Is `d` the prelude's `Proxy` class, used outside the standard library?
    fn user_proxy(&self, d: DefId) -> bool {
        Some(d) == self.cx.prelude_adt("Proxy") && !self.cx.scopes[self.module].is_std
    }

    /// `new Proxy<T>(...)`: `Proxy` takes no type arguments in Velt (the result is an `any`);
    /// the class name without them.
    pub(crate) fn proxy_class_name(&mut self, class: &ast::TypeExpr) -> Option<ast::TypeExpr> {
        let ast::TypeExprKind::Named { path, args } = &class.kind else {
            return None;
        };
        if path.len() != 1 || path[0].name != "Proxy" || args.is_empty() {
            return None;
        }
        let d = self.cx.prelude_adt("Proxy")?;
        match self.lookup_type_path(path) {
            Some(crate::ctx::Item::Def(x)) if x == d => {}
            _ => return None,
        }
        Some(ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: path.clone(),
                args: vec![],
            },
            span: class.span,
        })
    }

    /// The `Proxy` object `h` (`new Proxy(...)` of class `d`) as an `any`; `h` itself for
    /// another class.
    pub(crate) fn proxy_new(
        &mut self,
        d: DefId,
        h: hir::Expr,
        exp: Option<hir::TyId>,
        span: Span,
    ) -> hir::Expr {
        if !self.user_proxy(d) {
            return h;
        }
        let Some(defs) = self.cx.any else {
            return h;
        };
        if let Some(t) = exp {
            let typed = !self.cx.maybe_dynamic(t)
                && !self.cx.ty.is_bottom(t)
                && !matches!(self.cx.ty.kind(t), hir::TyKind::Param(_));
            if typed {
                let tn = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error(
                        format!("a proxy cannot be used as a `{tn}` yet: it is an `any` (#823)"),
                        span,
                    )
                    .with_note("give the variable or parameter the type `any`; typed proxies, which TypeScript gives the target's type, come later"),
                );
                return self.error_expr(span);
            }
        }
        let Some(impl_index) = self.cx.any_impl() else {
            return h;
        };
        self.cx.note_dyn_members();
        let any = self.cx.ty.intern(hir::TyKind::Dyn(defs.any, vec![]));
        let kind = H::ToDyn {
            expr: Box::new(h),
            impl_index,
        };
        self.mk(kind, any, span)
    }

    /// A call of a static method of class `d`: `Reflect.*` in the program's own code makes it
    /// use `any` members (the prelude's bodies read and write through `any`).
    pub(crate) fn note_reflect_call(&mut self, d: DefId) {
        if Some(d) == self.cx.prelude_adt("Reflect") && !self.cx.scopes[self.module].is_std {
            self.cx.any_impl();
            self.cx.note_dyn_members();
        }
    }
}
