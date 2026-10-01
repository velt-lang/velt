//! Children (docs/contracts/jsx.md "Children"): text becomes a `string`, `{expr}` and `{...xs}`
//! are one child each, `{}` / `{/* */}` vanish. Intrinsic elements and fragments pass them to the
//! runtime as a `JSX.Child[]`; a component gets them in its children props field: one child as
//! itself, several as an array.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::attrs::str_lit_ast;
use super::provider::Provider;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

/// The children that produce a value (not `{}` or `{/* comment */}`).
pub(super) fn real_children(children: &[ast::JsxChild]) -> Vec<&ast::JsxChild> {
    children
        .iter()
        .filter(|c| !matches!(c, ast::JsxChild::Expr { expr: None, .. }))
        .collect()
}

/// Source span of a child.
pub(super) fn child_span(c: &ast::JsxChild) -> Span {
    match c {
        ast::JsxChild::Text { span, .. }
        | ast::JsxChild::Expr { span, .. }
        | ast::JsxChild::Spread { span, .. } => *span,
        ast::JsxChild::Element(el) => el.span,
    }
}

impl FnCx<'_, '_> {
    /// The children of an intrinsic element or fragment as a `JSX.Child[]`.
    pub(super) fn child_array(
        &mut self,
        p: &Provider,
        children: &[ast::JsxChild],
        span: Span,
    ) -> hir::Expr {
        let mut hs = vec![];
        for c in real_children(children) {
            let h = self.child_value(p, c, p.child);
            hs.push(self.as_child(p, h));
        }
        let ty = self.cx.ty.array(p.child);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// `h` converted to `JSX.Child`.
    pub(super) fn as_child(&mut self, p: &Provider, h: hir::Expr) -> hir::Expr {
        let note = format!(
            "children are passed to the JSX provider '{}' as `JSX.Child`",
            p.source
        );
        self.jsx_coerce(h, p.child, &note)
    }

    /// Child `c` (not an empty `{}`) checked against `exp`.
    pub(super) fn child_value(&mut self, p: &Provider, c: &ast::JsxChild, exp: TyId) -> hir::Expr {
        match c {
            ast::JsxChild::Text { value, span } => {
                self.expr(&str_lit_ast(value, *span), Some(exp), Want::Move)
            }
            ast::JsxChild::Expr { expr: Some(e), .. } | ast::JsxChild::Spread { expr: e, .. } => {
                self.expr(e, Some(exp), Want::Move)
            }
            ast::JsxChild::Expr { expr: None, span } => self.unit_expr(*span),
            ast::JsxChild::Element(el) => self.jsx_element(p, el),
        }
    }

    /// The children props field value (declared type `fty`) of component `<name>`: the only
    /// child itself, or an array of all of them.
    pub(super) fn children_prop(
        &mut self,
        p: &Provider,
        kids: &[&ast::JsxChild],
        fty: TyId,
        name: &str,
    ) -> hir::Expr {
        let span = match (kids.first(), kids.last()) {
            (Some(a), Some(b)) => child_span(a).to(child_span(b)),
            _ => Span::DUMMY,
        };
        let what = format!("in the children of <{name}>");
        if let [only] = kids {
            let h = self.child_value(p, only, fty);
            return self.jsx_coerce(h, fty, &what);
        }
        let Some(arr) = self.array_form(fty) else {
            let shown = self.cx.display(fty);
            self.cx.error(
                Diagnostic::error(
                    format!("This JSX tag's 'children' prop expects a single child of type '{shown}', but multiple children were provided."),
                    span,
                )
                .with_note("declare the children field as an array type to accept several children"),
            );
            for c in kids {
                self.child_value(p, c, p.child);
            }
            return self.error_expr(span);
        };
        let elem = self.cx.ty.array_elem(arr).unwrap_or(self.cx.ty.error);
        let mut hs = vec![];
        for c in kids {
            let h = self.child_value(p, c, elem);
            hs.push(self.jsx_coerce(h, elem, &what));
        }
        let h = self.mk(H::ArrayLit(hs), arr, span);
        self.jsx_coerce(h, fty, &what)
    }

    /// The array type `t` is or contains (`T[]`, `T[] | null`, a union with an array member).
    fn array_form(&mut self, t: TyId) -> Option<TyId> {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        if matches!(self.cx.ty.kind(t), TyKind::Array(_)) {
            return Some(t);
        }
        self.cx
            .union_members(t)?
            .into_iter()
            .find(|m| matches!(self.cx.ty.kind(*m), TyKind::Array(_)))
    }
}
