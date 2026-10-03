//! JSX for the scope walker: component tags are value names (`<Card` and `</Card>` name the
//! function `Card`, `<ui.Card` a namespace member), and attribute values, spreads and children are
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
        // The closing tag names the component too (`</Card>`).
        for name in el.name.iter().chain(&el.closing_name) {
            match name {
                JsxName::Ident(id) if is_component(&id.name) => self.value_name(id),
                JsxName::Member(parts) => self.path(parts),
                _ => {}
            }
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

/// Does the tag `tag` (as written: `div`, `my-el`, `svg:rect`, `Card`, `ui.Card`) name a
/// component rather than an intrinsic element? Mirrors sema's rule (`intrinsic_tag`,
/// docs/internals/contracts/jsx.md): a dotted name is a component, a namespaced one is not, and
/// a simple name is one unless it starts with a lower-case letter or contains `-`.
pub(crate) fn is_component(tag: &str) -> bool {
    if tag.contains(':') {
        return false;
    }
    tag.contains('.') || !(tag.starts_with(|c: char| c.is_ascii_lowercase()) || tag.contains('-'))
}
