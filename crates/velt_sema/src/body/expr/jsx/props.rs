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
}

impl FnCx<'_, '_> {
    /// The props object of component element `el` (spread sources bound in `lets`) and the
    /// component's type arguments; `None` after an error that leaves no props.
    pub(super) fn component_props(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        c: &Component,
        tag: &str,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<(hir::Expr, Vec<TyId>)> {
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
            self.explicit_type_args(&mut slots, n, &el.type_args, &[], span);
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
        let shown = self.cx.display_in(c.props, &names);
        let mut filled = Filled {
            values: fields.iter().map(|_| None).collect(),
            fields,
            given: vec![],
            written: vec![],
            slots,
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
        Some((
            self.finish_struct(d, slots, values, None, el.span),
            type_args,
        ))
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
                // Fields the props type does not have are left out, as in an object spread.
                for (name, h) in self.spread_source(expr, lets) {
                    if let Some(i) = filled.fields.iter().position(|(n, _)| *n == name) {
                        self.set_prop(filled, i, name, h);
                    }
                }
            }
            ast::JsxAttr::Named { name, value, span } => {
                let (n, name_span) = attr_name(name);
                let Some(i) = filled.fields.iter().position(|(f, _)| *f == n) else {
                    let names: Vec<String> = filled.fields.iter().map(|(f, _)| f.clone()).collect();
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
