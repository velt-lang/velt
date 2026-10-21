//! A value that may be `undefined` passed for a parameter with a default (issue #930,
//! docs/reference/types.md "Null and undefined"): JavaScript applies the default where the value
//! is `undefined`. The default of a parameter whose type has no `undefined` is evaluated at the
//! call site (`defaults.rs`), so the argument is tested there:
//! `{ const <arg> = h; typeof <arg> === "undefined" ? <default> : <arg>! }`.

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, TyId, TyKind};

/// The temporary holding the argument (no source name can be this).
const ARG: &str = "<maybe undefined arg>";

impl FnCx<'_, '_> {
    /// The argument `h` for a parameter of type `target` with the call-site default `default`,
    /// when `h` may be `undefined` and `target` cannot hold it; `Err(h)` otherwise. A
    /// `T | null | undefined` argument for a `T | null` parameter keeps the error: `!` would
    /// drop its `null` too.
    pub(super) fn default_where_undefined(
        &mut self,
        h: hir::Expr,
        target: TyId,
        default: &hir::Expr,
        type_args: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        let undefined = self.cx.ty.undefined();
        let takes_default = self.cx.dynamic(target).is_none()
            && self
                .cx
                .ty
                .nothing_of(target)
                .is_none_or(|n| !n.has_undefined())
            && !self.is_undefined_lit(default);
        if takes_default && h.ty == undefined && matches!(h.kind, H::Lit(hir::Lit::Null)) {
            // `f(undefined)`: the default itself.
            let mut d = default.clone();
            crate::visit::map_expr_types(&mut d, &mut |t| self.cx.subst(t, type_args));
            d.span = h.span;
            return Ok(d);
        }
        let applies = self.cx.dynamic(target).is_none()
            && self.cx.dynamic(h.ty).is_none()
            && matches!(self.cx.ty.kind(h.ty), TyKind::Option(inner, Nothing::Undefined)
                if self.cx.ty.nothing_of(*inner).is_none())
            && self
                .cx
                .ty
                .nothing_of(target)
                .is_none_or(|n| !n.has_undefined())
            && !self.is_undefined_lit(default);
        if !applies {
            return Err(h);
        }
        let span = h.span;
        let mut stmts = vec![];
        self.push_scope();
        self.bind_temp(ARG, h, &mut stmts);
        let name = || {
            super::setters::synth(
                ast::ExprKind::Ident(ast::Ident {
                    name: ARG.into(),
                    span,
                }),
                span,
            )
        };
        let test = crate::body::defaults::undefined_test(name(), span);
        let cond = self.cond(&test);
        let mut d = default.clone();
        crate::visit::map_expr_types(&mut d, &mut |t| self.cx.subst(t, type_args));
        d.span = span;
        let value = super::setters::synth(ast::ExprKind::NonNull(Box::new(name())), span);
        let els = self.expr_coerce(&value, target, Want::Move);
        self.pop_scope();
        let pick = H::If {
            cond: Box::new(cond),
            then: Box::new(d),
            els: Box::new(els),
        };
        let pick = self.mk(pick, target, span);
        let b = super::accessor_rmw::block(stmts, Some(pick), span);
        Ok(self.mk(H::Block(b), target, span))
    }
}
