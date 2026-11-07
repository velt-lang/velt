//! The props object of an element with an automatic runtime: the one place it is built
//! ([`FnCx::element_props`]), so that another way of passing props has one place to change.
//!
//! As `tsc` emits it, it is a new object per element: the attributes and spread sources in source
//! order, then `children`, a later name replacing an earlier one in its place. The compiler's part
//! is what `tsc` checks: each attribute against `JSX.IntrinsicElements[tag]` or the component's
//! props and `JSX.IntrinsicAttributes` (missing and unknown names, contextual types, type
//! arguments of a generic component). A value is then stored as it was written, not as its
//! declared type holds it, so an `undefined` stays one where the field also takes `null`.
//!
//! A spread source's fields are all copied, also the ones the target does not declare; an optional
//! one only when present. A `Record` or an `any` is copied at run time, as the record literal does
//! (`record_literal`).

use velt_common::{std_key, Span};
use velt_syntax::ast;

use super::super::record_call::RecordKey;
use super::attrs::{attr_name, str_lit_ast};
use super::automatic::Factory;
use super::automatic_decl::Declared;
use super::children::{child_span, real_children};
use super::component::synthetic;
use super::key::is_key;
use super::props::{is_arrow_attr, is_arrow_child};
use super::provider::Provider;
use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, StmtKind as S, TyId, TyKind, UseMode};
use crate::ide::record::Target;

/// The key `tsc` and esbuild put the children under, whatever `ElementChildrenAttribute` names.
const CHILDREN: &str = "children";

/// What a value set into the props is checked against.
enum FieldCheck {
    /// That declared attribute, or with none, the declared index signature.
    Declared(Option<usize>),
    /// Nothing: a later attribute replaces it.
    Overridden,
}

/// A value to set into the props, in the order to evaluate it.
enum Part {
    /// `props.name = value`; `field` is its declared attribute.
    Set {
        name: String,
        field: Option<usize>,
        value: hir::Expr,
        /// Written on the element, not taken from a spread source.
        written: bool,
    },
    /// The same when `cond` holds (an optional field of a spread source).
    SetIf {
        cond: hir::Expr,
        name: String,
        field: Option<usize>,
        value: hir::Expr,
    },
    /// Statements: a spread source's temporary, a run-time copy, an evaluated key.
    Stmts(Vec<hir::Stmt>),
}

/// The props object of an element: statements, the read of the object.
pub(super) struct BuiltProps {
    pub stmts: Vec<hir::Stmt>,
    pub read: hir::Expr,
    /// The component's type arguments.
    pub type_args: Vec<TyId>,
    /// Those the props gave, which must meet the constraints (the others are `unknown`).
    pub inferred: Vec<Option<TyId>>,
}

impl FnCx<'_, '_> {
    /// The props object of `el` for factory `f`, checked against `decl` (type parameters
    /// `slot_names`, `tag` in messages). `key`: statements evaluated at that attribute's place.
    #[allow(clippy::too_many_arguments)] // the element, its runtime, and what it is checked against
    pub(super) fn element_props(
        &mut self,
        p: &Provider,
        f: &Factory,
        el: &ast::JsxElement,
        mut decl: Declared,
        slot_names: &[String],
        tag: &str,
        key: Option<(usize, Vec<hir::Stmt>)>,
        drop_key: bool,
    ) -> BuiltProps {
        let str_ = self.cx.ty.str_;
        let rec_args = vec![str_, f.value];
        let ty = self.cx.ty.intern(TyKind::Adt(f.record, rec_args.clone()));
        let span = el.span;
        let t = self.new_local("<props>", ty, false, span, LocalKind::Temp);
        let mut slots = vec![None; slot_names.len()];
        if let Some(at) = super::props::type_args_span(el) {
            let n = slots.len();
            self.explicit_type_args(&mut slots, n, &el.type_args, (&[], &[]), at);
        }
        let mut parts: Vec<(usize, Part)> = vec![];
        let mut given: Vec<(String, TyId)> = vec![];
        let mut written: Vec<String> = vec![];
        let mut any_spread = false;
        let attrs: Vec<(usize, &ast::JsxAttr)> = (el.attrs.iter().enumerate())
            .filter(|(_, a)| !is_key(a))
            .collect();
        // Arrow functions last, as in TypeScript: their parameter types come from the other props
        // and the children (unless a child is an arrow function too).
        let arrow_child = el.children.iter().any(is_arrow_child);
        for arrows in [false, true] {
            for (seq, a) in attrs.iter().filter(|(_, a)| is_arrow_attr(a) == arrows) {
                match a {
                    ast::JsxAttr::Named { name, value, span } => {
                        let (n, name_span) = attr_name(name);
                        if written.contains(&n) {
                            self.cx.err(
                                "JSX elements cannot have multiple attributes with the same name.",
                                name_span,
                            );
                        }
                        written.push(n.clone());
                        let i = decl.fields.iter().position(|d| d.name == n);
                        // As in `tsc`, a name with a hyphen is not held to an index signature.
                        let i = i.or_else(|| match n.contains('-') {
                            true => None,
                            false => decl.pattern_field(&n),
                        });
                        let exp = match i {
                            Some(i) => {
                                if let Some((d, index)) = decl.fields[i].at {
                                    self.cx.rec_ref(name_span, Target::Field(d, index));
                                }
                                self.cx.subst_known(decl.fields[i].ty, &slots)
                            }
                            // As in `tsc`, only a name with a hyphen may be undeclared.
                            None if n.contains('-') => f.value,
                            None => match decl.rest {
                                Some(t) => t,
                                None => {
                                    let names: Vec<String> =
                                        decl.fields.iter().map(|d| d.name.clone()).collect();
                                    self.no_property(&n, &decl.shown, &names, name_span, None);
                                    self.attr_value(p, value, None, *span);
                                    continue;
                                }
                            },
                        };
                        // A written `undefined` or `null` is checked against the field, then
                        // stored as the nothing it is, not as the field holds it.
                        let nothing = matches!(
                            value,
                            Some(ast::JsxAttrValue::Expr { expr, .. })
                                if matches!(
                                    expr.kind,
                                    ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
                                )
                        );
                        let i = match nothing {
                            true => {
                                self.attr_value(p, value, Some(exp), *span);
                                None
                            }
                            false => i,
                        };
                        let exp = if nothing { f.value } else { exp };
                        let (h, _) = self.attr_value(p, value, Some(exp), *span);
                        if let Some(i) = i {
                            self.cx.match_ty(decl.fields[i].ty, h.ty, &mut slots);
                        }
                        given.push((n.clone(), h.ty));
                        let part = Part::Set {
                            name: n,
                            field: i,
                            value: h,
                            written: true,
                        };
                        parts.push((*seq, part));
                    }
                    ast::JsxAttr::Spread { expr, .. } => {
                        any_spread |= self.spread_into(
                            expr,
                            (t, ty),
                            *seq,
                            &decl,
                            &mut slots,
                            &mut given,
                            &mut parts,
                        );
                    }
                }
            }
            if arrows == arrow_child {
                if let Some(h) = self.auto_children(p, f, el, &decl, &slots, tag) {
                    let at = decl.fields.iter().position(|d| {
                        let core = self.cx.ty.core(d.ty).unwrap_or(d.ty);
                        d.name == p.children_field
                            && !matches!(self.cx.ty.kind(core), TyKind::Tuple(_))
                    });
                    if let Some(i) = at {
                        self.cx.match_ty(decl.fields[i].ty, h.ty, &mut slots);
                    }
                    given.push((p.children_field.clone(), h.ty));
                    let part = Part::Set {
                        name: CHILDREN.to_string(),
                        field: at,
                        value: h,
                        written: true,
                    };
                    parts.push((el.attrs.len(), part));
                }
            }
        }
        self.children_specified_twice(p, el, &written);
        if !any_spread {
            let name_span = el.name.as_ref().map_or(span, |n| n.span());
            self.missing_declared(&decl, &given, name_span);
        }
        let inferred = slots.clone();
        // A type parameter no prop gives a value is `unknown`, as in TypeScript.
        let unknown = self.cx.any.map(|defs| TyKind::Dyn(defs.unknown, vec![]));
        if let Some(u) = unknown.map(|k| self.cx.ty.intern(k)) {
            slots
                .iter_mut()
                .filter(|s| s.is_none())
                .for_each(|s| *s = Some(u));
        }
        let type_args = self.solve_props_slots(slot_names, tag, el.span, &slots);
        if let Some((at, stmts)) = key {
            parts.push((at, Part::Stmts(stmts)));
        }
        // Stable: within one attribute, a spread source's temporary stays before its fields.
        parts.sort_by_key(|(seq, _)| *seq);
        let overridden = self.overrides(&parts);
        let new = self.mk(
            H::New {
                def: f.record,
                type_args: rec_args,
                args: vec![],
            },
            ty,
            span,
        );
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local: t,
                init: Some(new),
            },
            span,
        }];
        let what = format!("in the props of type '{}'", decl.shown);
        for (i, (_, part)) in parts.into_iter().enumerate() {
            let field_of = |field: Option<usize>| match overridden[i] {
                true => FieldCheck::Overridden,
                false => FieldCheck::Declared(field),
            };
            match part {
                Part::Stmts(s) => stmts.extend(s),
                Part::Set {
                    name, field, value, ..
                } => {
                    let set = self.set_prop_checked(
                        (t, ty),
                        f,
                        name,
                        value,
                        field_of(field),
                        &decl,
                        &type_args,
                        &what,
                    );
                    let at = set.span;
                    stmts.push(hir::Stmt {
                        kind: S::Expr(set),
                        span: at,
                    });
                }
                Part::SetIf {
                    cond,
                    name,
                    field,
                    value,
                } => {
                    let set = self.set_prop_checked(
                        (t, ty),
                        f,
                        name,
                        value,
                        field_of(field),
                        &decl,
                        &type_args,
                        &what,
                    );
                    let at = set.span;
                    let then = hir::Block {
                        stmts: vec![hir::Stmt {
                            kind: S::Expr(set),
                            span: at,
                        }],
                        value: None,
                        span: at,
                    };
                    stmts.push(hir::Stmt {
                        kind: S::If {
                            cond,
                            then,
                            els: None,
                        },
                        span: at,
                    });
                }
            }
        }
        if drop_key {
            let recv = self.mk(H::Local(t, UseMode::Borrow), ty, span);
            let key = ast::Ident {
                name: "key".to_string(),
                span,
            };
            let name = RecordKey::Name(&key);
            let call = self.record_call_keyed(recv, std_key!(record_delete), name, &[], span);
            stmts.push(hir::Stmt {
                kind: S::Expr(call),
                span,
            });
        }
        let read = self.mk(H::Local(t, UseMode::Move), ty, span);
        BuiltProps {
            stmts,
            read,
            type_args,
            inferred,
        }
    }

    /// TS2710: `children` both as an attribute and in the body, where the attribute is checked.
    fn children_specified_twice(&mut self, p: &Provider, el: &ast::JsxElement, written: &[String]) {
        let checked = (p.automatic.as_ref()).is_some_and(|a| a.children_checked);
        if checked && written.contains(&p.children_field) && !real_children(&el.children).is_empty()
        {
            self.cx.err(
                format!(
                    "'{0}' are specified twice. The attribute named '{0}' will be overwritten.",
                    p.children_field
                ),
                el.span,
            );
        }
    }

    /// Per part: is it a spread field that a later written attribute replaces? (`tsc` checks only
    /// the value that wins.) A written attribute that a later spread field always replaces is
    /// TS2783.
    fn overrides(&mut self, parts: &[(usize, Part)]) -> Vec<bool> {
        let mut out = vec![false; parts.len()];
        for (i, (_, part)) in parts.iter().enumerate() {
            let (name, written, span) = match part {
                Part::Set {
                    name,
                    written,
                    value,
                    ..
                } => (name, *written, value.span),
                Part::SetIf { name, value, .. } => (name, false, value.span),
                Part::Stmts(_) => continue,
            };
            let mut later = parts[i + 1..].iter().filter_map(|(_, p)| match p {
                Part::Set {
                    name: n, written, ..
                } if n == name => Some(*written),
                _ => None,
            });
            match written {
                true if later.any(|w| !w) => self.cx.err(
                    format!(
                        "'{name}' is specified more than once, so this usage will be overwritten."
                    ),
                    span,
                ),
                false if later.any(|w| w) => out[i] = true,
                _ => {}
            }
        }
        out
    }

    /// `props.name = h`, with `h` checked against what `decl` says and stored as written.
    #[allow(clippy::too_many_arguments)]
    fn set_prop_checked(
        &mut self,
        (t, ty): (hir::LocalId, TyId),
        f: &Factory,
        name: String,
        h: hir::Expr,
        check: FieldCheck,
        decl: &Declared,
        type_args: &[TyId],
        what: &str,
    ) -> hir::Expr {
        let at = h.span;
        let mut h = h;
        // Names the type lists by an index signature are checked against it.
        if let (FieldCheck::Declared(None), Some(rest)) = (&check, decl.rest) {
            if self.cx.dynamic(rest).is_none() && !name.contains('-') {
                h = self.checked_as_written(h, rest, what);
            }
        }
        let field = match check {
            FieldCheck::Declared(field) => field,
            FieldCheck::Overridden => None,
        };
        if let Some(d) = field.map(|i| &decl.fields[i]) {
            let target = self.cx.subst(d.ty, type_args);
            h = self.checked_as_written(h, target, what);
            if let Some(also) = d.also {
                let probe = self.mk(H::Lit(hir::Lit::Unit), h.ty, at);
                if self.try_coerce(probe, also).is_err() {
                    self.not_assignable(also, &h, what);
                }
            }
        }
        let value = self.jsx_coerce(h, f.value, "in the props the JSX runtime's factory takes");
        let recv = self.mk(H::Local(t, UseMode::Borrow), ty, at);
        let key = ast::Ident { name, span: at };
        self.record_set_checked(recv, RecordKey::Name(&key), value, at)
    }

    /// `h` checked against the declared `target`. A value that may be nothing is kept as it is,
    /// so its `undefined` or `null` is the one written; another is converted (a function to the
    /// declared parameters, a literal to the declared number type).
    fn checked_as_written(&mut self, h: hir::Expr, target: TyId, what: &str) -> hir::Expr {
        if self.cx.ty.opt_payload(h.ty).is_none() {
            return self.jsx_coerce(h, target, what);
        }
        let probe = self.mk(H::Lit(hir::Lit::Unit), h.ty, h.span);
        if self.try_coerce(probe, target).is_err() {
            self.not_assignable(target, &h, what);
        }
        h
    }

    /// Spread source `e`, attribute `seq`, into `parts`. Is it one that may give every field (an
    /// `any`: `tsc` then requires nothing)?
    #[allow(clippy::too_many_arguments)]
    fn spread_into(
        &mut self,
        e: &ast::Expr,
        (t, ty): (hir::LocalId, TyId),
        seq: usize,
        decl: &Declared,
        slots: &mut [Option<TyId>],
        given: &mut Vec<(String, TyId)>,
        parts: &mut Vec<(usize, Part)>,
    ) -> bool {
        let trial = self.trial_type(e);
        let core = trial.map(|t| self.cx.ty.core(t).unwrap_or(t));
        let view = core.map(|c| self.cx.record_view(c).map_or(c, |(base, _)| base));
        let dynamic = core.is_some_and(|c| self.spreads_dynamic(c));
        if dynamic || view.is_some_and(|v| self.record_args(v).is_some()) {
            let mut stmts = vec![];
            let str_ = self.cx.ty.str_;
            let props = [ast::ObjectProp::Spread(e.clone())];
            let value_any = self
                .record_args(ty)
                .is_some_and(|(_, v)| self.cx.dynamic(v) == Some(Dynamic::Any));
            match dynamic && !value_any {
                true => self.spread_any_converted((t, ty), e, &mut stmts),
                false => {
                    self.record_literal_entries((t, ty), str_, &props, vec![], &mut stmts, e.span);
                }
            }
            parts.push((seq, Part::Stmts(stmts)));
            return dynamic;
        }
        let mut lets = vec![];
        let fields = self.spread_fields(e, None, &mut lets, true);
        parts.push((seq, Part::Stmts(lets)));
        for (name, h, src) in fields {
            let i = decl.fields.iter().position(|d| d.name == name);
            if let Some(i) = i {
                self.cx.match_ty(decl.fields[i].ty, h.ty, slots);
            }
            if !src.optional {
                given.push((name.clone(), h.ty));
            }
            let part = match self.source_guard(&src, h.span) {
                Some(cond) => Part::SetIf {
                    cond,
                    name,
                    field: i,
                    value: h,
                },
                None => Part::Set {
                    name,
                    field: i,
                    value: h,
                    written: false,
                },
            };
            parts.push((seq, part));
        }
        false
    }

    /// The value of `children`: none, the only child, or an array of all of them, a spread child
    /// flattened into it. Checked against the declared field; `None` without children.
    fn auto_children(
        &mut self,
        p: &Provider,
        f: &Factory,
        el: &ast::JsxElement,
        decl: &Declared,
        slots: &[Option<TyId>],
        tag: &str,
    ) -> Option<hir::Expr> {
        let kids = real_children(&el.children);
        let (first, last) = (kids.first()?, kids.last()?);
        let span = child_span(first).to(child_span(last));
        let field = decl.fields.iter().find(|d| d.name == p.children_field);
        let exp = match field {
            Some(d) => self.cx.subst_known(d.ty, slots),
            None => {
                let checked = (p.automatic.as_ref()).is_some_and(|a| a.children_checked);
                if checked && decl.rest.is_none() {
                    let note = format!(
                        "<{tag}> does not accept children: its props type has no `{}` field",
                        p.children_field
                    );
                    self.no_property(&p.children_field, &decl.shown, &[], span, Some(note));
                }
                decl.rest.unwrap_or(f.value)
            }
        };
        if let ([only], false) = (&kids[..], matches!(kids[0], ast::JsxChild::Spread { .. })) {
            return Some(self.child_value(p, only, exp));
        }
        if !self.spread_children_are_arrays(&kids) {
            return Some(self.error_expr(span));
        }
        let core = self.cx.ty.core(exp).unwrap_or(exp);
        if let TyKind::Tuple(elems) = self.cx.ty.kind(core).clone() {
            return Some(self.tuple_children(p, &kids, &elems, f, tag, span));
        }
        let elem = match self.cx.dynamic(core) {
            Some(Dynamic::Any | Dynamic::Unknown) => Some(core),
            Some(_) => self.any_ty(),
            None => self.array_form(exp).and_then(|a| self.cx.ty.array_elem(a)),
        };
        let Some(elem) = elem else {
            let shown = self.cx.display(exp);
            self.cx.err(
                format!("This JSX tag's 'children' prop expects a single child of type '{shown}', but multiple children were provided."),
                span,
            );
            return Some(self.error_expr(span));
        };
        let known = !self.cx.ty.has_error(elem);
        if kids
            .iter()
            .any(|c| matches!(c, ast::JsxChild::Spread { .. }))
        {
            let elems = kids.iter().map(|c| child_ast(c)).collect();
            let arr = synthetic(ast::ExprKind::Array(elems), span);
            let exp = known.then(|| self.cx.ty.array(elem));
            return Some(self.expr(&arr, exp, Want::Move));
        }
        let mut hs: Vec<hir::Expr> = kids.iter().map(|c| self.child_value(p, c, elem)).collect();
        let elem = match known {
            true => elem,
            false => (hs.iter().map(|h| h.ty))
                .find(|t| !self.cx.ty.has_error(*t))
                .map_or(elem, |t| self.cx.widened(t)),
        };
        let what = format!("in the children of <{tag}>");
        hs = hs
            .into_iter()
            .map(|h| self.jsx_coerce(h, elem, &what))
            .collect();
        let ty = self.cx.ty.array(elem);
        Some(self.mk(H::ArrayLit(hs), ty, span))
    }

    /// Children into a field of tuple type: each checked against its element, as an array, which
    /// is what the runtime gets in JavaScript.
    fn tuple_children(
        &mut self,
        p: &Provider,
        kids: &[&ast::JsxChild],
        elems: &[TyId],
        f: &Factory,
        tag: &str,
        span: Span,
    ) -> hir::Expr {
        if kids.len() != elems.len()
            || kids
                .iter()
                .any(|c| matches!(c, ast::JsxChild::Spread { .. }))
        {
            self.cx.err(
                format!(
                    "<{tag}> takes {} children, as its children type is a tuple",
                    elems.len()
                ),
                span,
            );
            return self.error_expr(span);
        }
        let what = format!("in the children of <{tag}>");
        let mut hs = vec![];
        for (c, t) in kids.iter().zip(elems) {
            let h = self.child_value(p, c, *t);
            let h = self.jsx_coerce(h, *t, &what);
            hs.push(self.jsx_coerce(h, f.value, "in the props the JSX runtime's factory takes"));
        }
        let ty = self.cx.ty.array(f.value);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// TS2609: a spread child must be an array (or an `any`, which is converted to one).
    fn spread_children_are_arrays(&mut self, kids: &[&ast::JsxChild]) -> bool {
        let mut ok = true;
        for c in kids {
            let ast::JsxChild::Spread { expr, span } = c else {
                continue;
            };
            let Some(t) = self.trial_type(expr) else {
                continue;
            };
            let core = self.cx.ty.core(t).unwrap_or(t);
            let array = self.cx.ty.array_elem(core).is_some()
                || matches!(self.cx.ty.kind(core), TyKind::Tuple(_))
                || self.cx.ty.is_bottom(core)
                || self.cx.dynamic(core) == Some(Dynamic::Any);
            if !array {
                self.cx
                    .err("JSX spread child must be an array type.", *span);
                ok = false;
            }
        }
        ok
    }

    /// TS2741 for every required attribute that was not given.
    fn missing_declared(&mut self, decl: &Declared, given: &[(String, TyId)], span: Span) {
        let shown: Vec<String> = given
            .iter()
            .map(|(n, t)| format!("{}: {}", crate::types::display_key(n), self.cx.display(*t)))
            .collect();
        let shown = match shown.is_empty() {
            true => "{}".to_string(),
            false => format!("{{ {} }}", shown.join("; ")),
        };
        for d in decl.fields.iter().filter(|d| d.required) {
            if !given.iter().any(|(n, _)| *n == d.name) {
                let (name, of) = (&d.name, &decl.shown);
                self.cx.err(
                    format!("Property '{name}' is missing in type '{shown}' but required in type '{of}'."),
                    span,
                );
            }
        }
    }
}

/// Child `c` as an element of an array literal.
fn child_ast(c: &ast::JsxChild) -> ast::Expr {
    match c {
        ast::JsxChild::Text { value, span } => str_lit_ast(value, *span),
        ast::JsxChild::Expr { expr: Some(e), .. } => e.clone(),
        ast::JsxChild::Spread { expr, span } => {
            synthetic(ast::ExprKind::Spread(Box::new(expr.clone())), *span)
        }
        ast::JsxChild::Expr { expr: None, span } => {
            synthetic(ast::ExprKind::Lit(ast::Lit::Undefined), *span)
        }
        ast::JsxChild::Element(el) => synthetic(ast::ExprKind::Jsx(Box::new(el.clone())), el.span),
    }
}
