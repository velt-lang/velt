//! The props object of a component element: attributes, spread sources (merged like an object
//! spread: a later name wins) and the children field, checked against the component's props
//! type `P` with TypeScript's messages for unknown and missing properties. For a generic
//! component (`function List<T>(props: { items: T[] })`) the type arguments are written on the
//! tag (`<List<number> …>`) or inferred from the props like a call's: typed values and children
//! first, then arrow functions.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::super::args::as_arrow;
use super::attrs::attr_name;
use super::children::{child_span, real_children};
use super::component::Component;
use super::key::is_key;
use super::provider::Provider;
use crate::body::FnCx;
use crate::hir::{self, DefId, TyId};
use crate::ide::record::Target;

/// The props being built.
struct Filled {
    /// Field types, mentioning the component's type parameters (`slots`).
    fields: Vec<(String, TyId)>,
    /// Values by field index, checked but not yet converted to their field type.
    values: Vec<Option<hir::Expr>>,
    /// What was written (for the "missing" message).
    given: Vec<(String, TyId)>,
    /// Fields given by a written attribute (a second one is an error).
    written: Vec<usize>,
    /// The component's type arguments inferred so far.
    slots: Vec<Option<TyId>>,
    /// `JSX.IntrinsicAttributes` (its def and fields besides `key`): attributes are checked
    /// against these too, as TypeScript checks `Props & IntrinsicAttributes`.
    common: Option<(DefId, Vec<CommonField>)>,
    /// The attributes that matched only `common`, in source order.
    extra: Vec<Extra>,
}

/// A field of `JSX.IntrinsicAttributes`.
struct CommonField {
    /// Its index in the declared type (for hover and go to definition).
    index: u32,
    name: String,
    /// What a value must be: the declared type, without the `| null` of an optional field
    /// (TypeScript says `not assignable to type 'string'` for `"client:media"?: string`).
    check: TyId,
    optional: bool,
}

/// An attribute from `JSX.IntrinsicAttributes` for the provider.
struct Extra {
    name: String,
    span: Span,
    /// Converted to `JSX.AttrValue`.
    value: hir::Expr,
    /// Given by a spread source: a later attribute of the same name replaces it.
    spread: bool,
}

/// A component element's props, and the attributes it was given from `JSX.IntrinsicAttributes`.
pub(super) struct ElementProps {
    pub props: hir::Expr,
    pub type_args: Vec<TyId>,
    pub extra: Vec<(String, Span, hir::Expr)>,
}

impl FnCx<'_, '_> {
    /// The props object of component element `el` (spread sources bound in `lets`), the
    /// component's type arguments, and its `JSX.IntrinsicAttributes` attributes; `None` after an
    /// error that leaves no props.
    pub(super) fn component_props(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        c: &Component,
        tag: &str,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<ElementProps> {
        let name_span = el.name.as_ref().map_or(el.span, |n| n.span());
        let Some((d, fields)) = self.object_fields(c.props) else {
            let shown = self.cx.display(c.props);
            self.cx.err(
                format!("the props of component <{tag}> must have an object type, found `{shown}`"),
                name_span,
            );
            return None;
        };
        let mut slots = vec![None; c.slot_names.len()];
        if let Some(span) = type_args_span(el) {
            let n = slots.len();
            self.explicit_type_args(&mut slots, n, &el.type_args, span);
        }
        // Shown with the type arguments as written, else the component's parameter names.
        let written = (el.type_args.len() == slots.len()).then_some(&el.type_args);
        let names: Vec<String> = c
            .slot_names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                written.map_or_else(|| n.clone(), |ts| crate::written_types::written(&ts[i]))
            })
            .collect();
        let common = self.common_attrs(p);
        let shown = self.cx.display_in(c.props, &names);
        // As TypeScript shows the type an element's attributes are checked against.
        let shown = match &common {
            Some(_) => format!("IntrinsicAttributes & {shown}"),
            None => shown,
        };
        let mut filled = Filled {
            values: fields.iter().map(|_| None).collect(),
            fields,
            given: vec![],
            written: vec![],
            slots,
            common,
            extra: vec![],
        };
        let attrs: Vec<&ast::JsxAttr> = el.attrs.iter().filter(|a| !is_key(a)).collect();
        // Arrow functions last, as in TypeScript: their parameter types come from the other
        // props and the children (unless a child is an arrow function too).
        let arrow_child = el.children.iter().any(is_arrow_child);
        for arrows in [false, true] {
            for a in attrs.iter().filter(|a| is_arrow_attr(a) == arrows) {
                self.prop_attr(p, a, d, &shown, lets, &mut filled);
            }
            if arrows == arrow_child {
                self.children_field(p, el, &shown, tag, &mut filled);
            }
        }
        self.missing_props(d, &shown, name_span, &mut filled);
        self.missing_common(name_span, &filled);
        let type_args = self.solve_props_slots(c, tag, name_span, &filled.slots);
        let what = format!("in the props of type '{shown}'");
        let mut values = vec![];
        for (i, v) in filled.values.into_iter().enumerate() {
            let target = self.cx.subst(filled.fields[i].1, &type_args);
            values.push(v.map(|h| self.jsx_coerce(h, target, &what)));
        }
        let args = self.adt_of(c.props).map(|(_, a)| a).unwrap_or_default();
        let slots = args
            .into_iter()
            .map(|a| Some(self.cx.subst(a, &type_args)))
            .collect();
        let extra = filled
            .extra
            .into_iter()
            .map(|e| (e.name, e.span, e.value))
            .collect();
        Some(ElementProps {
            props: self.finish_struct(d, slots, values, None, el.span),
            type_args,
            extra,
        })
    }

    /// The fields of the provider's `JSX.IntrinsicAttributes` besides `key`, with its def.
    fn common_attrs(&mut self, p: &Provider) -> Option<(DefId, Vec<CommonField>)> {
        let attrs = p.component_attrs.as_ref()?;
        let (d, fields) = self.object_fields(attrs.ty)?;
        let optional: Vec<bool> = self
            .cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.optional).collect())
            .unwrap_or_default();
        let fields = fields
            .into_iter()
            .enumerate()
            .filter(|(_, (n, _))| n != "key")
            .map(|(i, (name, ty))| {
                let optional = optional.get(i).copied().unwrap_or(false);
                let check = match optional {
                    true => self.cx.ty.opt_payload(ty).unwrap_or(ty),
                    false => ty,
                };
                CommonField {
                    index: i as u32,
                    name,
                    check,
                    optional,
                }
            })
            .collect();
        Some((d, fields))
    }

    /// The `JSX.IntrinsicAttributes` field named `n`: (def, index, type a value must have).
    fn common_field(filled: &Filled, n: &str) -> Option<(DefId, u32, TyId)> {
        let (d, fields) = filled.common.as_ref()?;
        let f = fields.iter().find(|f| f.name == n)?;
        Some((*d, f.index, f.check))
    }

    /// Value `h` of attribute `n` (from a written attribute or a spread source) for the provider:
    /// checked against its `JSX.IntrinsicAttributes` type and converted to `JSX.AttrValue`.
    fn push_extra(
        &mut self,
        p: &Provider,
        filled: &mut Filled,
        n: &str,
        span: Span,
        h: hir::Expr,
        spread: bool,
    ) {
        let Some((_, _, check)) = Self::common_field(filled, n) else {
            return;
        };
        let given = h.ty;
        let h = self.jsx_coerce(h, check, COMMON_WHAT);
        let note = format!(
            "attributes of `JSX.IntrinsicAttributes` are passed to the JSX provider '{}' as `JSX.AttrValue`",
            p.source
        );
        let value = self.jsx_coerce(h, p.attr_value, &note);
        // A spread source's value is replaced by a later one of the same name, as in an object.
        if let Some(i) = filled.extra.iter().position(|e| e.name == n && e.spread) {
            filled.extra.remove(i);
        } else if filled.extra.iter().any(|e| e.name == n) && !spread {
            self.cx.err(
                "JSX elements cannot have multiple attributes with the same name.",
                span,
            );
        }
        filled.given.push((n.to_string(), given));
        filled.extra.push(Extra {
            name: n.to_string(),
            span,
            value,
            spread,
        });
    }

    /// A prop that `JSX.IntrinsicAttributes` also declares: its value must fit both types, as
    /// TypeScript intersects them.
    fn check_common_too(&mut self, filled: &Filled, n: &str, ty: TyId, span: Span) {
        let Some((_, _, check)) = Self::common_field(filled, n) else {
            return;
        };
        let probe = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), ty, span);
        if self.try_coerce(probe, check).is_err() {
            let probe = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), ty, span);
            self.not_assignable(check, &probe, COMMON_WHAT);
        }
    }

    /// The component's type arguments; unknown ones are errors.
    fn solve_props_slots(
        &mut self,
        c: &Component,
        tag: &str,
        span: Span,
        slots: &[Option<TyId>],
    ) -> Vec<TyId> {
        let mut out = vec![];
        for (s, name) in slots.iter().zip(&c.slot_names) {
            out.push(s.unwrap_or_else(|| {
                self.cx.error(
                    Diagnostic::error(
                        format!("cannot infer type parameter `{name}` of component <{tag}>"),
                        span,
                    )
                    .with_note("give a prop that mentions it a value of a known type"),
                );
                self.cx.ty.error
            }));
        }
        out
    }

    /// Record checked value `h` for field `i` (inferring type arguments from it).
    fn set_prop(&mut self, filled: &mut Filled, i: usize, name: String, h: hir::Expr) {
        self.cx
            .match_ty(filled.fields[i].1, h.ty, &mut filled.slots);
        filled.given.push((name, h.ty));
        filled.values[i] = Some(h);
    }

    /// One attribute or spread source of a component element.
    fn prop_attr(
        &mut self,
        p: &Provider,
        a: &ast::JsxAttr,
        d: DefId,
        shown: &str,
        lets: &mut Vec<hir::Stmt>,
        filled: &mut Filled,
    ) {
        match a {
            ast::JsxAttr::Spread { expr, .. } => {
                // Fields the props type does not have go to the provider when
                // `JSX.IntrinsicAttributes` declares them, else are left out, as in an object
                // spread.
                for (name, h) in self.spread_source(expr, lets) {
                    if let Some(i) = filled.fields.iter().position(|(n, _)| *n == name) {
                        self.check_common_too(filled, &name, h.ty, h.span);
                        self.set_prop(filled, i, name, h);
                    } else {
                        let span = h.span;
                        self.push_extra(p, filled, &name, span, h, true);
                    }
                }
            }
            ast::JsxAttr::Named { name, value, span } => {
                let (n, name_span) = attr_name(name);
                let Some(i) = filled.fields.iter().position(|(f, _)| *f == n) else {
                    if let Some((d, index, check)) = Self::common_field(filled, &n) {
                        self.cx.rec_ref(name_span, Target::Field(d, index));
                        let (h, _) = self.attr_value(p, value, Some(check), *span);
                        self.push_extra(p, filled, &n, name_span, h, false);
                        return;
                    }
                    let mut names: Vec<String> =
                        filled.fields.iter().map(|(f, _)| f.clone()).collect();
                    if let Some((_, common)) = &filled.common {
                        names.extend(common.iter().map(|f| f.name.clone()));
                    }
                    self.no_property(&n, shown, &names, name_span, None);
                    self.attr_value(p, value, None, *span);
                    return;
                };
                self.cx.rec_ref(name_span, Target::Field(d, i as u32));
                if filled.written.contains(&i) {
                    self.cx.err(
                        "JSX elements cannot have multiple attributes with the same name.",
                        name_span,
                    );
                }
                filled.written.push(i);
                let exp = self.cx.subst_known(filled.fields[i].1, &filled.slots);
                let (h, _) = self.attr_value(p, value, Some(exp), *span);
                self.check_common_too(filled, &n, h.ty, h.span);
                self.set_prop(filled, i, n, h);
            }
        }
    }

    /// The element's children into the props' children field.
    fn children_field(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        shown: &str,
        tag: &str,
        filled: &mut Filled,
    ) {
        let kids = real_children(&el.children);
        let (Some(first), Some(last)) = (kids.first(), kids.last()) else {
            return;
        };
        let span = child_span(first).to(child_span(last));
        let Some(i) = filled
            .fields
            .iter()
            .position(|(n, _)| *n == p.children_field)
        else {
            let note = format!(
                "<{tag}> does not accept children: its props type has no `{}` field",
                p.children_field
            );
            self.no_property(&p.children_field, shown, &[], span, Some(note));
            for c in kids {
                self.child_value(p, c, p.child);
            }
            return;
        };
        let exp = self.cx.subst_known(filled.fields[i].1, &filled.slots);
        let h = self.children_prop(p, &kids, exp, tag);
        self.set_prop(filled, i, p.children_field.clone(), h);
    }

    /// TS2741 for a required field of `JSX.IntrinsicAttributes` that was not given (every
    /// component element must have it).
    fn missing_common(&mut self, span: Span, filled: &Filled) {
        let Some((_, common)) = &filled.common else {
            return;
        };
        let given: Vec<String> = filled
            .given
            .iter()
            .map(|(n, t)| format!("{}: {}", crate::ctx::display_key(n), self.cx.display(*t)))
            .collect();
        let given = if given.is_empty() {
            "{}".to_string()
        } else {
            format!("{{ {} }}", given.join("; "))
        };
        for f in common {
            // Given as an attribute, by a spread source, or as a prop of the same name.
            if f.optional || filled.given.iter().any(|(n, _)| *n == f.name) {
                continue;
            }
            let name = &f.name;
            self.cx.err(
                format!("Property '{name}' is missing in type '{given}' but required in type 'IntrinsicAttributes'."),
                span,
            );
        }
    }

    /// TS2741 for every required props field that was not given.
    fn missing_props(&mut self, d: DefId, shown: &str, span: Span, filled: &mut Filled) {
        let given: Vec<String> = filled
            .given
            .iter()
            .map(|(n, t)| format!("{n}: {}", self.cx.display(*t)))
            .collect();
        let given = if given.is_empty() {
            "{}".to_string()
        } else {
            format!("{{ {} }}", given.join("; "))
        };
        for i in 0..filled.fields.len() {
            let (name, ty) = filled.fields[i].clone();
            let has_default = self
                .cx
                .adt(d)
                .and_then(|a| a.fields.get(i))
                .is_some_and(|f| f.default.is_some());
            if filled.values[i].is_some() || has_default || self.cx.ty.opt_payload(ty).is_some() {
                continue;
            }
            self.cx.err(
                format!("Property '{name}' is missing in type '{given}' but required in type '{shown}'."),
                span,
            );
            filled.values[i] = Some(self.error_expr(span));
        }
    }
}

/// `{(x) => …}` among the children.
fn is_arrow_child(c: &ast::JsxChild) -> bool {
    matches!(c, ast::JsxChild::Expr { expr: Some(e), .. } if as_arrow(e).is_some())
}

/// Where the explicit type arguments of `el` are written (`None` without any).
pub(super) fn type_args_span(el: &ast::JsxElement) -> Option<Span> {
    let (first, last) = (el.type_args.first()?, el.type_args.last()?);
    Some(first.span.to(last.span))
}

/// `name={(x) => …}`: checked after the other props.
fn is_arrow_attr(a: &ast::JsxAttr) -> bool {
    matches!(
        a,
        ast::JsxAttr::Named {
            value: Some(ast::JsxAttrValue::Expr { expr, .. }),
            ..
        } if as_arrow(expr).is_some()
    )
}

/// Where a `JSX.IntrinsicAttributes` mismatch is reported from.
const COMMON_WHAT: &str = "in the props of type 'IntrinsicAttributes'";
