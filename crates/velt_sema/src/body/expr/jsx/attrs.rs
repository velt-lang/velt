//! Attributes of intrinsic elements: each is checked against the tag's field of
//! `JSX.IntrinsicElements` (unknown names are errors, except names with `-` or `:` and every
//! attribute of a custom element `<my-tag>`), then converted to `JSX.AttrValue`. Spread sources
//! `{...obj}` contribute their fields; a later attribute overrides an earlier spread field in place.

use velt_common::Span;
use velt_syntax::ast;

use super::key::is_key;
use super::provider::Provider;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, TyId};
use crate::ide::record::Target;

/// An intrinsic attribute as the runtime receives it.
pub(super) struct Attr {
    pub name: String,
    /// The value converted to `JSX.AttrValue`.
    pub value: hir::Expr,
    /// The value when it is known at compile time (the precompile lowering writes it out).
    pub fixed: Option<Fixed>,
    /// Written as `name=…` (not taken from a spread source).
    explicit: bool,
    /// The value's own type (for messages).
    written: TyId,
}

/// A compile-time attribute value.
pub(super) enum Fixed {
    /// `name="text"` (entities already decoded).
    Text(String),
    /// A bare `name` (`true`).
    Bare,
}

/// The attribute object type of a checked tag: its fields, and its name for messages.
struct TagAttrs {
    def: DefId,
    fields: Vec<(String, TyId)>,
    shown: String,
}

impl FnCx<'_, '_> {
    /// The attributes of intrinsic element `el` (tag `tag`), spread sources bound in `lets`;
    /// `None` if the tag is not in `JSX.IntrinsicElements` (reported).
    pub(super) fn intrinsic_attrs(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        tag: &str,
        tag_span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<Vec<Attr>> {
        let tag_attrs = self.tag_attrs(p, tag, tag_span).ok()?;
        let mut out: Vec<Attr> = vec![];
        for a in el.attrs.iter().filter(|a| !is_key(a)) {
            match a {
                ast::JsxAttr::Spread { expr, span } => {
                    for (name, h, _) in self.spread_source(expr, lets) {
                        let Ok(fty) = self.attr_field(p, tag_attrs.as_ref(), &name, *span) else {
                            continue;
                        };
                        let written = h.ty;
                        let value = self.attr_converted(p, h, fty, &name, tag);
                        let attr = Attr {
                            name,
                            value,
                            fixed: None,
                            explicit: false,
                            written,
                        };
                        add(&mut out, self, attr, *span);
                    }
                }
                ast::JsxAttr::Named { name, value, span } => {
                    let (n, name_span) = attr_name(name);
                    let Ok(fty) = self.attr_field(p, tag_attrs.as_ref(), &n, name_span) else {
                        self.attr_value(p, value, None, *span);
                        continue;
                    };
                    let exp = fty.unwrap_or(p.attr_value);
                    let (h, fixed) = self.attr_value(p, value, Some(exp), *span);
                    let written = h.ty;
                    let value = self.attr_converted(p, h, fty, &n, tag);
                    let attr = Attr {
                        name: n,
                        value,
                        fixed,
                        explicit: true,
                        written,
                    };
                    add(&mut out, self, attr, name_span);
                }
            }
        }
        if let Some(t) = &tag_attrs {
            self.missing_attrs(t, &out, tag_span);
        }
        Some(out)
    }

    /// TS2741 for every attribute the tag requires (a field that is not `T | null`) but `el`
    /// does not give.
    fn missing_attrs(&mut self, tag: &TagAttrs, given: &[Attr], span: Span) {
        for (name, ty) in &tag.fields {
            if self.cx.ty.opt_payload(*ty).is_some() || given.iter().any(|a| a.name == *name) {
                continue;
            }
            let given: Vec<String> = given
                .iter()
                .map(|a| format!("{}: {}", a.name, self.cx.display(a.written)))
                .collect();
            let given = if given.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", given.join("; "))
            };
            self.cx.err(
                format!(
                    "Property '{name}' is missing in type '{given}' but required in type '{}'.",
                    tag.shown
                ),
                span,
            );
        }
    }

    /// `JSX.IntrinsicElements[tag]`; `Ok(None)` for tags whose attributes are not checked
    /// (custom elements, namespaced tags).
    fn tag_attrs(&mut self, p: &Provider, tag: &str, span: Span) -> Result<Option<TagAttrs>, ()> {
        if tag.contains('-') || tag.contains(':') {
            return Ok(None);
        }
        let Some((d, tags)) = self.object_fields(p.intrinsics) else {
            self.cx.err(
                format!(
                    "`JSX.IntrinsicElements` of the JSX provider '{}' must be an object type",
                    p.source
                ),
                span,
            );
            return Err(());
        };
        let Some(i) = tags.iter().position(|(n, _)| n == tag) else {
            let names: Vec<String> = tags.into_iter().map(|(n, _)| n).collect();
            self.no_property(tag, "JSX.IntrinsicElements", &names, span, None);
            return Err(());
        };
        self.cx.rec_ref(span, Target::Field(d, i as u32));
        let Some((def, fields)) = self.object_fields(tags[i].1) else {
            self.cx.err(
                format!("`JSX.IntrinsicElements[\"{tag}\"]` must be an object type of attributes"),
                span,
            );
            return Err(());
        };
        let shown = format!("JSX.IntrinsicElements[\"{tag}\"]");
        Ok(Some(TagAttrs { def, fields, shown }))
    }

    /// The fields (name, type) of object type `t` (an anonymous object type or a struct).
    pub(super) fn object_fields(&mut self, t: TyId) -> Option<(DefId, Vec<(String, TyId)>)> {
        let (d, args) = self.adt_of(t)?;
        if self.is_class_def(d) {
            return None;
        }
        let fields: Vec<(String, TyId)> = self
            .cx
            .adt(d)?
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let fields = fields
            .into_iter()
            .map(|(n, t)| (n, self.cx.subst(t, &args)))
            .collect();
        Some((d, fields))
    }

    /// The declared type of attribute `name` (`Ok(None)`: not checked); unknown names are
    /// reported.
    fn attr_field(
        &mut self,
        p: &Provider,
        tag: Option<&TagAttrs>,
        name: &str,
        span: Span,
    ) -> Result<Option<TyId>, ()> {
        let Some(tag) = tag.filter(|_| !name.contains('-') && !name.contains(':')) else {
            return Ok(None);
        };
        if let Some(i) = tag.fields.iter().position(|(n, _)| n == name) {
            self.cx.rec_ref(span, Target::Field(tag.def, i as u32));
            return Ok(Some(tag.fields[i].1));
        }
        let names: Vec<String> = tag.fields.iter().map(|(n, _)| n.clone()).collect();
        let note = is_event_handler(name).then(|| {
            format!(
                "the JSX provider '{}' declares no event handlers: server-rendered HTML cannot run them; attach them with a JSX provider that has client-side support",
                p.source
            )
        });
        self.no_property(name, &tag.shown, &names, span, note);
        Err(())
    }

    /// Value of attribute `value` checked against `exp` (when given), and its compile-time form.
    pub(super) fn attr_value(
        &mut self,
        p: &Provider,
        value: &Option<ast::JsxAttrValue>,
        exp: Option<TyId>,
        span: Span,
    ) -> (hir::Expr, Option<Fixed>) {
        match value {
            None => {
                let t = self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, span);
                (t, Some(Fixed::Bare))
            }
            Some(ast::JsxAttrValue::Str { value, span }) => {
                let lit = str_lit_ast(value, *span);
                let h = self.expr(&lit, exp, Want::Move);
                (h, Some(Fixed::Text(value.clone())))
            }
            Some(ast::JsxAttrValue::Expr { expr, .. }) => (self.expr(expr, exp, Want::Move), None),
            Some(ast::JsxAttrValue::Element(el)) => (self.jsx_element(p, el), None),
        }
    }

    /// `h` converted to the attribute's declared type `fty` (if checked), then to
    /// `JSX.AttrValue`.
    fn attr_converted(
        &mut self,
        p: &Provider,
        h: hir::Expr,
        fty: Option<TyId>,
        name: &str,
        tag: &str,
    ) -> hir::Expr {
        let h = match fty {
            Some(t) => self.jsx_coerce(h, t, &format!("in attribute '{name}' of <{tag}>")),
            None => h,
        };
        let note = format!(
            "attribute values are passed to the JSX provider '{}' as `JSX.AttrValue`",
            p.source
        );
        self.jsx_coerce(h, p.attr_value, &note)
    }
}

/// Record attribute `attr`; a later one replaces an earlier spread field of the same name in
/// place, two written ones with the same name are an error.
fn add(out: &mut Vec<Attr>, fx: &mut FnCx, attr: Attr, span: Span) {
    match out.iter_mut().find(|a| a.name == attr.name) {
        Some(prev) if prev.explicit && attr.explicit => fx.cx.err(
            "JSX elements cannot have multiple attributes with the same name.",
            span,
        ),
        Some(prev) => *prev = attr,
        None => out.push(attr),
    }
}

/// The written name of an attribute and the span to report it at.
pub(super) fn attr_name(name: &ast::JsxAttrName) -> (String, Span) {
    let span = match name {
        ast::JsxAttrName::Ident(id) => id.span,
        ast::JsxAttrName::Namespaced(ns, id) => ns.span.to(id.span),
    };
    (name.to_source(), span)
}

/// A string literal expression with the source span of the JSX text it comes from.
pub(super) fn str_lit_ast(value: &str, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Lit(ast::Lit::Str(value.to_string())),
        span,
    }
}

/// `onClick`, `onSubmit`, …: the shape of a DOM event handler attribute.
fn is_event_handler(name: &str) -> bool {
    name.strip_prefix("on")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_handler_names() {
        assert!(is_event_handler("onClick"));
        assert!(is_event_handler("onSubmit"));
        assert!(!is_event_handler("one"));
        assert!(!is_event_handler("on"));
        assert!(!is_event_handler("class"));
    }
}
