//! JSX (docs/contracts/jsx.md): elements and fragments desugar into calls of the module's JSX
//! runtime (`<jsxImportSource>/jsx-runtime`, bound as the namespace `JSX`), built from already
//! checked HIR, so no HIR construct is JSX-specific. Generated nodes keep the spans of the JSX
//! they come from (editor queries on tags, attributes and children work as on calls).
//!
//! - fragment `<>…</>` → `Fragment(children, null)`;
//! - intrinsic `<div a={x}>…</div>` → `jsx("div", ["a"], [x], children, key)` ([`attrs`]), or a
//!   `jsxTemplate` call when the runtime supports precompilation ([`precompile`]);
//! - component `<Card a={x}>…</Card>` → `jsxComponent(Card, { a: x, children }, key, "m#Card")`
//!   or `jsxAsyncComponent` ([`component`], [`props`]);
//! - children → [`children`]; `key` → [`key`].

mod attrs;
mod children;
mod component;
mod component_call;
mod escape;
mod invoke;
mod key;
mod list_fold;
mod messages;
mod precompile;
mod props;
mod provider;
mod sole_child;
mod text_run;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H};
pub(crate) use component_call::{check_prop_copies, Adapter};
pub(crate) use provider::Provider;

impl FnCx<'_, '_> {
    /// A JSX element or fragment expression.
    pub(crate) fn jsx(&mut self, el: &ast::JsxElement) -> hir::Expr {
        let Some(p) = self.jsx_provider(el.span) else {
            self.jsx_unchecked(el);
            return self.error_expr(el.span);
        };
        self.jsx_element(&p, el)
    }

    /// Lower `el` (an expression, an attribute value or a child) through runtime `p`.
    pub(super) fn jsx_element(&mut self, p: &Provider, el: &ast::JsxElement) -> hir::Expr {
        let mark = self.cx.rec_mark();
        let h = match &el.name {
            None => self.jsx_fragment(p, el),
            Some(name) => match intrinsic_tag(name) {
                Some(tag) => {
                    self.intrinsic_type_args(el, &tag);
                    self.void_children(p, el, &tag);
                    self.jsx_intrinsic(p, el, &tag, name.span())
                }
                None => self.jsx_component(p, el, name),
            },
        };
        if self.cx.recording() {
            self.cx.rec_ty(el.span, h.ty);
            // A mismatched closing tag (`<A></B>`, reported by the parser) names nothing.
            if let (Some(open), Some(close)) = (&el.name, &el.closing_name) {
                if open.to_source() == close.to_source() {
                    self.mirror_closing_name(mark, open, close);
                }
            }
        }
        h
    }

    /// The closing tag's name denotes what the opening one does (`</Card>`, `</ui.Card>`,
    /// `</div>`): for definition, references and rename.
    fn mirror_closing_name(&mut self, mark: usize, open: &ast::JsxName, close: &ast::JsxName) {
        self.cx.rec_mirror(mark, open.span(), close.span());
        if let (ast::JsxName::Member(opens), ast::JsxName::Member(closes)) = (open, close) {
            for (o, c) in opens.iter().zip(closes) {
                self.cx.rec_mirror(mark, o.span, c.span);
            }
        }
    }

    /// Type arguments on an intrinsic element (`<div<T>>`) are an error.
    fn intrinsic_type_args(&mut self, el: &ast::JsxElement, tag: &str) {
        let Some(span) = props::type_args_span(el) else {
            return;
        };
        self.cx.error(
            Diagnostic::error(
                format!("expected 0 type argument(s), found {}", el.type_args.len()),
                span,
            )
            .with_note(format!(
                "<{tag}> is an intrinsic element, not a generic component"
            )),
        );
    }

    /// Children of a void element (`<br>x</br>`, one the provider lists in `jsxVoidElements`)
    /// are an error: the element has no end tag, so the children could not be rendered.
    fn void_children(&mut self, p: &Provider, el: &ast::JsxElement, tag: &str) {
        let kids = children::real_children(&el.children);
        let (Some(first), Some(last)) = (kids.first(), kids.last()) else {
            return;
        };
        let declared = p.void_elements.as_ref();
        if !declared.is_some_and(|list| list.iter().any(|v| v == tag)) {
            return;
        }
        let span = children::child_span(first).to(children::child_span(last));
        self.cx.error(
            Diagnostic::error(format!("<{tag}> is a void element and cannot have children"), span)
                .with_note(format!(
                    "the JSX provider '{}' writes <{tag}> without an end tag, so there is nowhere to put children: remove them",
                    p.source
                )),
        );
    }

    fn jsx_fragment(&mut self, p: &Provider, el: &ast::JsxElement) -> hir::Expr {
        let children = self.child_array(p, &el.children, el.span);
        let key = self.jsx_key(p, el);
        self.jsx_call(p, p.fragment, "Fragment", vec![children, key], el.span)
    }

    fn jsx_intrinsic(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        tag: &str,
        tag_span: Span,
    ) -> hir::Expr {
        if let Some(pc) = p
            .precompile
            .filter(|_| precompile::precompilable(p, el, tag))
        {
            return self.jsx_template(p, pc, el, tag, tag_span);
        }
        let mut lets = vec![];
        let Some(attrs) = self.intrinsic_attrs(p, el, tag, tag_span, &mut lets) else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        let key = self.jsx_key(p, el);
        let (names, values): (Vec<hir::Expr>, Vec<hir::Expr>) = attrs
            .into_iter()
            .map(|a| (self.str_lit(&a.name, a.value.span), a.value))
            .unzip();
        let str_ = self.cx.ty.str_;
        let names_ty = self.cx.ty.array(str_);
        let names = self.mk(H::ArrayLit(names), names_ty, el.span);
        let values_ty = self.cx.ty.array(p.attr_value);
        let values = self.mk(H::ArrayLit(values), values_ty, el.span);
        let children = self.child_array(p, &el.children, el.span);
        let tag = self.str_lit(tag, tag_span);
        let args = vec![tag, names, values, children, key];
        let call = self.jsx_call(p, p.jsx, "jsx", args, el.span);
        self.with_lets(lets, call)
    }

    /// Check the attribute values and children of an element whose tag could not be used.
    pub(super) fn jsx_loose(&mut self, p: &Provider, el: &ast::JsxElement) {
        for a in &el.attrs {
            match a {
                ast::JsxAttr::Spread { expr, .. } => {
                    self.expr(expr, None, Want::Borrow);
                }
                ast::JsxAttr::Named { value, span, .. } => {
                    self.attr_value(p, value, None, *span);
                }
            }
        }
        for c in children::real_children(&el.children) {
            self.child_value(p, c, p.child);
        }
    }

    /// Check the expressions inside `el` when the module has no usable runtime.
    fn jsx_unchecked(&mut self, el: &ast::JsxElement) {
        let mut exprs = vec![];
        let mut elements = vec![];
        for a in &el.attrs {
            match a {
                ast::JsxAttr::Spread { expr, .. }
                | ast::JsxAttr::Named {
                    value: Some(ast::JsxAttrValue::Expr { expr, .. }),
                    ..
                } => exprs.push(expr),
                ast::JsxAttr::Named {
                    value: Some(ast::JsxAttrValue::Element(inner)),
                    ..
                } => elements.push(inner),
                ast::JsxAttr::Named { .. } => {}
            }
        }
        for c in &el.children {
            match c {
                ast::JsxChild::Expr { expr: Some(e), .. }
                | ast::JsxChild::Spread { expr: e, .. } => exprs.push(e),
                ast::JsxChild::Element(inner) => elements.push(inner),
                ast::JsxChild::Text { .. } | ast::JsxChild::Expr { expr: None, .. } => {}
            }
        }
        for e in exprs {
            self.expr(e, None, Want::Borrow);
        }
        for inner in elements {
            self.jsx_unchecked(inner);
        }
    }
}

/// The tag of an intrinsic element (a lower-case or dashed name, or a namespaced name like
/// `svg:rect`); `None` for a component (a capitalized or dotted name).
pub(super) fn intrinsic_tag(name: &ast::JsxName) -> Option<String> {
    match name {
        ast::JsxName::Ident(id) => {
            let lower = id
                .name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase());
            (lower || id.name.contains('-')).then(|| id.name.clone())
        }
        ast::JsxName::Namespaced(..) => Some(name.to_source()),
        ast::JsxName::Member(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use velt_common::Span;
    use velt_syntax::ast::{Ident, JsxName};

    use super::intrinsic_tag;

    fn id(name: &str) -> Ident {
        Ident {
            name: name.into(),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn tag_kinds() {
        assert_eq!(
            intrinsic_tag(&JsxName::Ident(id("div"))).as_deref(),
            Some("div")
        );
        assert_eq!(
            intrinsic_tag(&JsxName::Ident(id("My-widget"))).as_deref(),
            Some("My-widget")
        );
        assert_eq!(
            intrinsic_tag(&JsxName::Namespaced(id("svg"), id("rect"))).as_deref(),
            Some("svg:rect")
        );
        assert_eq!(intrinsic_tag(&JsxName::Ident(id("Card"))), None);
        assert_eq!(
            intrinsic_tag(&JsxName::Member(vec![id("ui"), id("card")])),
            None
        );
    }
}
