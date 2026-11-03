//! `key`: removed from an element's attributes / props and passed to the runtime separately as
//! a `string | null` (a number key becomes its decimal string; no key is `null`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::provider::Provider;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic};

/// The attribute name the runtime receives separately.
pub(super) const KEY: &str = "key";

/// Is `a` the `key` attribute?
pub(super) fn is_key(a: &ast::JsxAttr) -> bool {
    matches!(a, ast::JsxAttr::Named { name: ast::JsxAttrName::Ident(id), .. } if id.name == KEY)
}

impl FnCx<'_, '_> {
    /// The `key` argument for element `el`: its `key` attribute as a `string | null`.
    pub(super) fn jsx_key(&mut self, p: &Provider, el: &ast::JsxElement) -> hir::Expr {
        if p.automatic.is_some() {
            return self.automatic_key(p, el);
        }
        let opt_str = self.cx.ty.option(self.cx.ty.str_);
        let key = el.attrs.iter().find(|a| is_key(a));
        let Some(ast::JsxAttr::Named { value, span, .. }) = key else {
            return self.mk(H::Lit(hir::Lit::Null), opt_str, el.span);
        };
        let h = match value {
            Some(ast::JsxAttrValue::Str { value, span }) => self.str_lit(value, *span),
            Some(ast::JsxAttrValue::Expr { expr, .. }) => self.expr(expr, None, Want::Move),
            Some(ast::JsxAttrValue::Element(inner)) => self.jsx_element(p, inner),
            None => self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, *span),
        };
        self.key_string(h, opt_str)
    }

    /// The `key` argument of an automatic runtime's `jsx` (its third parameter's type): the
    /// attribute's value as written, else `undefined`, as `tsc` passes it.
    fn automatic_key(&mut self, p: &Provider, el: &ast::JsxElement) -> hir::Expr {
        let params = &self.cx.fn_info(p.jsx).params;
        let Some(kty) = params.get(2).map(|q| q.ty) else {
            return self.unit_expr(el.span);
        };
        let Some(ast::JsxAttr::Named { value, span, .. }) = el.attrs.iter().find(|a| is_key(a))
        else {
            if self.cx.ty.nothing_of(kty).is_none() {
                let shown = self.cx.display(kty);
                self.cx.err(
                    format!(
                        "this element needs a `key`: the JSX provider '{}' takes it as `{shown}`",
                        p.source
                    ),
                    el.span,
                );
                return self.error_expr(el.span);
            }
            return self.mk(H::Lit(hir::Lit::Null), kty, el.span);
        };
        let h = match value {
            Some(ast::JsxAttrValue::Str { value, span }) => self.str_lit(value, *span),
            Some(ast::JsxAttrValue::Expr { expr, .. }) => self.expr(expr, Some(kty), Want::Move),
            Some(ast::JsxAttrValue::Element(inner)) => self.jsx_element(p, inner),
            None => self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, *span),
        };
        let what = format!("as the `key` the JSX provider '{}' takes", p.source);
        self.jsx_coerce(h, kty, &what)
    }

    /// A `string`, number or `string | null` key value as a `string | null`.
    fn key_string(&mut self, h: hir::Expr, opt_str: hir::TyId) -> hir::Expr {
        let span = h.span;
        let str_ = self.cx.ty.str_;
        if self.cx.ty.is_numeric(h.ty) {
            let s = self.intrinsic(Intrinsic::ToString, vec![h], str_, span);
            return self.mk(H::WrapSome(Box::new(s)), opt_str, span);
        }
        match self.try_coerce(h, opt_str) {
            Ok(h) => h,
            Err(h) => {
                let f = self.cx.display(h.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("Type '{f}' is not assignable to type 'string | number'."),
                        span,
                    )
                    .with_note("a JSX `key` is a string or a number"),
                );
                self.error_expr(span)
            }
        }
    }
}
