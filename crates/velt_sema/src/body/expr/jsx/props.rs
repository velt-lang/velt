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
    /// Where each value was written: the index of its attribute (`attrs.len()` for the
    /// children).
    seq: Vec<Option<usize>>,
    /// The temporaries of spread sources (and of values an optional spread field may keep),
    /// with the index of the attribute they belong to.
    staged: Vec<(usize, Vec<hir::Stmt>)>,
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
    /// The declared type (`string | null` for `"client:media"?: string`): what a value must be.
    ty: TyId,
    /// The type named in a mismatch: without the `| null` of an optional field, as TypeScript
    /// says `not assignable to type 'string'`.
    shown: TyId,
    optional: bool,
}

/// An attribute from `JSX.IntrinsicAttributes` for the provider.
struct Extra {
    name: String,
    span: Span,
    /// Converted to `JSX.AttrValue`, or the field's `T | null` when `absent_if_null`.
    value: hir::Expr,
    /// Given by a spread source: a later attribute of the same name replaces it.
    spread: bool,
    /// The index of the attribute that gave it.
    seq: usize,
    /// An optional field of a spread source (and no earlier value): passed only when it is not
    /// null, as JavaScript copies only the keys an object has.
    absent_if_null: bool,
}

/// An attribute from `JSX.IntrinsicAttributes` for the provider's `names` and `values`.
pub(super) struct ExtraAttr {
    pub name: String,
    pub span: Span,
    /// A `JSX.AttrValue`, or a `T | null` when `absent_if_null`.
    pub value: hir::Expr,
    /// Passed only when the value is not null (an optional field of a spread source).
    pub absent_if_null: bool,
}

/// A component element's props, and the attributes it was given from `JSX.IntrinsicAttributes`.
pub(super) struct ElementProps {
    pub props: hir::Expr,
    pub type_args: Vec<TyId>,
    /// The `key` (`string | null`).
    pub key: hir::Expr,
    /// In source order; their values are constants or temporaries when one is `absent_if_null`.
    pub extra: Vec<ExtraAttr>,
}

impl FnCx<'_, '_> {
    /// The props object of component element `el`, the component's type arguments, its key, and
    /// its `JSX.IntrinsicAttributes` attributes; `None` after an error that leaves no props.
    /// The values are evaluated in source order, as in TypeScript's `jsx(C, { ... }, key)`: the
    /// attributes, then the children, then the key (in its place after a spread, where `tsc`
    /// calls `createElement(C, { ...o, key })`). Where the call's own order differs, they are
    /// bound to temporaries in `lets`, which also holds the spread sources' temporaries.
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
        let common = self.common_attrs(p);
        let shown = self.cx.display_in(c.props, &names);
        // As TypeScript shows the type an element's attributes are checked against.
        let shown = match &common {
            Some(_) => format!("IntrinsicAttributes & {shown}"),
            None => shown,
        };
        let mut filled = Filled {
            values: fields.iter().map(|_| None).collect(),
            seq: fields.iter().map(|_| None).collect(),
            staged: vec![],
            fields,
            given: vec![],
            written: vec![],
            slots,
            common,
            extra: vec![],
        };
        let attrs: Vec<(usize, &ast::JsxAttr)> = el
            .attrs
            .iter()
            .enumerate()
            .filter(|(_, a)| !is_key(a))
            .collect();
        // Arrow functions last, as in TypeScript: their parameter types come from the other
        // props and the children (unless a child is an arrow function too).
        let arrow_child = el.children.iter().any(is_arrow_child);
        for arrows in [false, true] {
            for (seq, a) in attrs.iter().filter(|(_, a)| is_arrow_attr(a) == arrows) {
                self.prop_attr(p, a, *seq, d, &shown, &mut filled);
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
        for (i, v) in std::mem::take(&mut filled.values).into_iter().enumerate() {
            let target = self.cx.subst(filled.fields[i].1, &type_args);
            values.push(v.map(|h| self.jsx_coerce(h, target, &what)));
        }
        let mut key = self.jsx_key(p, el);
        // A key after a spread is evaluated in its place, else after everything else.
        let key_seq = match el.attrs.iter().position(is_key) {
            Some(k) if el.attrs[..k].iter().any(is_spread) => k,
            _ => el.attrs.len() + 1,
        };
        self.in_source_order(&mut filled, &mut values, (&mut key, key_seq), lets);
        let args = self.adt_of(c.props).map(|(_, a)| a).unwrap_or_default();
        let slots = args
            .into_iter()
            .map(|a| Some(self.cx.subst(a, &type_args)))
            .collect();
        let extra = filled
            .extra
            .into_iter()
            .map(|e| ExtraAttr {
                name: e.name,
                span: e.span,
                value: e.value,
                absent_if_null: e.absent_if_null,
            })
            .collect();
        Some(ElementProps {
            props: self.finish_struct(d, slots, values, None, el.span),
            type_args,
            key,
            extra,
        })
    }

    /// Bind the values to temporaries in `lets`, in source order, when the call would evaluate
    /// them in another order (the props in field order, then the key, then the attributes for
    /// the provider) or when the provider's arrays are built by statements (an attribute that
    /// may be absent); else `lets` gets just the spread sources' temporaries.
    fn in_source_order(
        &mut self,
        filled: &mut Filled,
        values: &mut [Option<hir::Expr>],
        (key, key_seq): (&mut hir::Expr, usize),
        lets: &mut Vec<hir::Stmt>,
    ) {
        // Stable: a spread source's temporary comes before the fields read from it.
        filled.staged.sort_by_key(|(s, _)| *s);
        // The source positions in the order the call evaluates them (constants left out).
        let mut order: Vec<usize> = filled
            .staged
            .iter()
            .filter(|(_, stmts)| !stmts.is_empty())
            .map(|(s, _)| *s)
            .collect();
        for (v, s) in values.iter().zip(&filled.seq) {
            if let (Some(v), Some(s)) = (v, s) {
                if !is_constant(v) {
                    order.push(*s);
                }
            }
        }
        if !is_constant(key) {
            order.push(key_seq);
        }
        for e in &filled.extra {
            if !is_constant(&e.value) {
                order.push(e.seq);
            }
        }
        let mut staged = std::mem::take(&mut filled.staged);
        if !order.is_sorted() || filled.extra.iter().any(|e| e.absent_if_null) {
            for (v, s) in values.iter_mut().zip(&filled.seq) {
                if let (Some(v), Some(s)) = (v, s) {
                    self.bind_staged(v, *s, &mut staged);
                }
            }
            self.bind_staged(key, key_seq, &mut staged);
            for e in &mut filled.extra {
                self.bind_staged(&mut e.value, e.seq, &mut staged);
            }
            staged.sort_by_key(|(s, _)| *s);
        }
        lets.extend(staged.into_iter().flat_map(|(_, stmts)| stmts));
    }

    /// Bind non-constant `v` (written at `seq`) to a temporary and read that instead.
    fn bind_staged(
        &mut self,
        v: &mut hir::Expr,
        seq: usize,
        staged: &mut Vec<(usize, Vec<hir::Stmt>)>,
    ) {
        if is_constant(v) {
            return;
        }
        let span = v.span;
        let h = std::mem::replace(v, self.error_expr(span));
        let mut stmts = vec![];
        *v = self.moved_temp(h, &mut stmts);
        staged.push((seq, stmts));
    }

    /// A temporary holding `h` (appended to `lets`), moved (or copied) out where it is read.
    fn moved_temp(&mut self, h: hir::Expr, lets: &mut Vec<hir::Stmt>) -> hir::Expr {
        let mut t = self.temp("<attr>", h, lets);
        let mode = match self.cx.is_copy(t.ty) {
            true => hir::UseMode::Copy,
            false => hir::UseMode::Move,
        };
        if let hir::ExprKind::Local(_, m) = &mut t.kind {
            *m = mode;
        }
        t
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
                let shown = match optional {
                    true => self.cx.ty.opt_payload(ty).unwrap_or(ty),
                    false => ty,
                };
                CommonField {
                    index: i as u32,
                    name,
                    ty,
                    shown,
                    optional,
                }
            })
            .collect();
        Some((d, fields))
    }

    /// The `JSX.IntrinsicAttributes` field named `n`: (def, index, declared type, type shown).
    fn common_field(filled: &Filled, n: &str) -> Option<(DefId, u32, TyId, TyId)> {
        let (d, fields) = filled.common.as_ref()?;
        let f = fields.iter().find(|f| f.name == n)?;
        Some((*d, f.index, f.ty, f.shown))
    }

    /// `h` converted to a field's declared type `ty`; a mismatch names `shown`, as `tsc` does.
    fn common_coerce(&mut self, h: hir::Expr, ty: TyId, shown: TyId) -> hir::Expr {
        match self.try_coerce(h, ty) {
            Ok(h) => h,
            Err(h) => {
                self.not_assignable(shown, &h, COMMON_WHAT);
                self.error_expr(h.span)
            }
        }
    }

    /// Value `h` of attribute `n` for the provider, checked against its
    /// `JSX.IntrinsicAttributes` type and converted to `JSX.AttrValue`. `at` is where it was
    /// given: (attribute index, `None` for a written attribute or whether the spread source's
    /// field is optional). A later value of a name replaces an earlier one in its place, as in
    /// an object; an optional spread field that is absent keeps the earlier one.
    fn push_extra(
        &mut self,
        p: &Provider,
        filled: &mut Filled,
        (n, span): (&str, Span),
        h: hir::Expr,
        (seq, spread): (usize, Option<bool>),
    ) {
        let Some((_, _, ty, shown)) = Self::common_field(filled, n) else {
            return;
        };
        let given = h.ty;
        let h = self.common_coerce(h, ty, shown);
        let note = format!(
            "attributes of `JSX.IntrinsicAttributes` are passed to the JSX provider '{}' as `JSX.AttrValue`",
            p.source
        );
        let earlier = filled.extra.iter().position(|e| e.name == n);
        if let Some(i) = earlier {
            if spread.is_none() && !filled.extra[i].spread {
                self.cx.err(
                    "JSX elements cannot have multiple attributes with the same name.",
                    span,
                );
            }
        }
        let maybe_absent = spread == Some(true) && self.cx.ty.opt_payload(h.ty).is_some();
        let (value, absent_if_null) = match earlier {
            // `{ "client:load": false, ...o }` stays `false` while `o["client:load"]` is
            // absent: the earlier value, evaluated in its place, is the default.
            Some(i) if maybe_absent => {
                let prev = std::mem::replace(&mut filled.extra[i].value, self.error_expr(span));
                let mut stmts = vec![];
                let prev = match is_constant(&prev) {
                    true => prev,
                    false => self.moved_temp(prev, &mut stmts),
                };
                filled.staged.push((filled.extra[i].seq, stmts));
                // Both may be absent: still a `T | null` (the same field's type).
                let still = filled.extra[i].absent_if_null;
                let over = self.nullish_exprs(h, prev, span);
                match still {
                    true => (over, true),
                    false => (self.jsx_coerce(over, p.attr_value, &note), false),
                }
            }
            None if maybe_absent => (h, true),
            _ => (self.jsx_coerce(h, p.attr_value, &note), false),
        };
        filled.given.push((n.to_string(), given));
        let extra = Extra {
            name: n.to_string(),
            span,
            value,
            spread: spread.is_some(),
            seq,
            absent_if_null,
        };
        match earlier {
            Some(i) => filled.extra[i] = extra,
            None => filled.extra.push(extra),
        }
    }

    /// A prop that `JSX.IntrinsicAttributes` also declares: its value must fit both types, as
    /// TypeScript intersects them.
    fn check_common_too(&mut self, filled: &Filled, n: &str, ty: TyId, span: Span) {
        let Some((_, _, declared, shown)) = Self::common_field(filled, n) else {
            return;
        };
        let probe = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), ty, span);
        if self.try_coerce(probe, declared).is_err() {
            let probe = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), ty, span);
            self.not_assignable(shown, &probe, COMMON_WHAT);
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

    /// Record checked value `h`, written at attribute `seq`, for field `i` (inferring type
    /// arguments from it).
    fn set_prop(
        &mut self,
        filled: &mut Filled,
        (i, seq): (usize, usize),
        name: String,
        h: hir::Expr,
    ) {
        self.cx
            .match_ty(filled.fields[i].1, h.ty, &mut filled.slots);
        filled.given.push((name, h.ty));
        filled.values[i] = Some(h);
        filled.seq[i] = Some(seq);
    }

    /// One attribute or spread source (the element's attribute `seq`) of a component element.
    fn prop_attr(
        &mut self,
        p: &Provider,
        a: &ast::JsxAttr,
        seq: usize,
        d: DefId,
        shown: &str,
        filled: &mut Filled,
    ) {
        match a {
            ast::JsxAttr::Spread { expr, .. } => {
                // Fields the props type does not have go to the provider when
                // `JSX.IntrinsicAttributes` declares them, else are left out, as in an object
                // spread.
                let mut lets = vec![];
                for (name, h, optional) in self.spread_source(expr, &mut lets) {
                    if let Some(i) = filled.fields.iter().position(|(n, _)| *n == name) {
                        self.check_common_too(filled, &name, h.ty, h.span);
                        self.set_prop(filled, (i, seq), name, h);
                    } else {
                        let span = h.span;
                        self.push_extra(p, filled, (&name, span), h, (seq, Some(optional)));
                    }
                }
                filled.staged.push((seq, lets));
            }
            ast::JsxAttr::Named { name, value, span } => {
                let (n, name_span) = attr_name(name);
                let Some(i) = filled.fields.iter().position(|(f, _)| *f == n) else {
                    if let Some((d, index, ty, _)) = Self::common_field(filled, &n) {
                        self.cx.rec_ref(name_span, Target::Field(d, index));
                        let (h, _) = self.attr_value(p, value, Some(ty), *span);
                        self.push_extra(p, filled, (&n, name_span), h, (seq, None));
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
                self.set_prop(filled, (i, seq), n, h);
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
        // After the attributes, as TypeScript puts `children` last.
        let seq = el.attrs.len();
        self.set_prop(filled, (i, seq), p.children_field.clone(), h);
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
            .map(|(n, t)| format!("{}: {}", crate::ctx::display_key(n), self.cx.display(*t)))
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

/// A value whose evaluation has no effect and that nothing else affects (a literal, a function),
/// so its place in the evaluation order does not matter.
fn is_constant(h: &hir::Expr) -> bool {
    use hir::ExprKind as H;
    match &h.kind {
        H::Lit(_) | H::FnRef(..) => true,
        H::Cast(e) | H::WrapSome(e) | H::Upcast(e) => is_constant(e),
        H::ToDyn { expr, .. } => is_constant(expr),
        H::Variant { args, .. } => args.iter().all(is_constant),
        _ => false,
    }
}

fn is_spread(a: &ast::JsxAttr) -> bool {
    matches!(a, ast::JsxAttr::Spread { .. })
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
