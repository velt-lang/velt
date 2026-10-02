//! JSX for the scope walker: component tags are value names (`<Card` names the function
//! `Card`, `<ui.Card` a namespace member), and attribute values, spreads and children are
//! walked like any expression. Intrinsic tags and attribute names are not names in scope; sema
//! resolves them to the fields of `JSX.IntrinsicElements`.

use velt_syntax::ast::{JsxAttr, JsxAttrValue, JsxChild, JsxElement, JsxName};

use super::Walker;

impl<'a> Walker<'a> {
    /// Walk `el` if it contains the cursor.
    pub(super) fn jsx(&mut self, el: &'a JsxElement) {
        if !self.contains(el.span) {
            return;
        }
        match &el.name {
            Some(JsxName::Ident(id)) if is_component(&id.name) => self.value_name(id),
            Some(JsxName::Member(parts)) => self.path(parts),
            _ => {}
        }
        for attr in &el.attrs {
            match attr {
                JsxAttr::Spread { expr, .. }
                | JsxAttr::Named {
                    value: Some(JsxAttrValue::Expr { expr, .. }),
                    ..
                } => self.expr(expr),
                JsxAttr::Named {
                    value: Some(JsxAttrValue::Element(inner)),
                    ..
                } => self.jsx(inner),
                JsxAttr::Named { .. } => {}
            }
        }
        for child in &el.children {
            match child {
                JsxChild::Expr {
                    expr: Some(expr), ..
                }
                | JsxChild::Spread { expr, .. } => self.expr(expr),
                JsxChild::Element(inner) => self.jsx(inner),
                JsxChild::Expr { expr: None, .. } | JsxChild::Text { .. } => {}
            }
        }
    }
}

/// A tag naming a component rather than an intrinsic element (docs/internals/contracts/jsx.md:
/// a capitalised simple name).
pub(crate) fn is_component(tag: &str) -> bool {
    tag.starts_with(|c: char| c.is_ascii_uppercase())
}
