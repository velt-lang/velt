//! Aggregate values: array and tuple literals, struct literals (`P { .. }` and `{ .. }` typed by
//! context), anonymous objects (synthesized `AdtKind::Anon` defs, one per shape), `new C(..)`.
//!
//! Struct literal fields are emitted in declaration order (omitted fields take their default;
//! an omitted `T | null` field of an anonymous object type is `null`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ops::untyped;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::FieldInfo;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(crate) fn array_lit(
        &mut self,
        elems: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let exp = self.hint(exp);
        // A union expected (`string | i64[]`): its one array or tuple member types the literal.
        let exp = exp.map(|t| {
            self.union_member(t, |s, m| {
                s.cx.ty.array_elem(m).is_some() || matches!(s.cx.ty.kind(m), TyKind::Tuple(_))
            })
            .unwrap_or(t)
        });
        if let Some(TyKind::Tuple(ts)) = exp.map(|t| self.cx.ty.kind(t).clone()) {
            return self.tuple_lit(elems, &ts, exp.expect("ICE: tuple"), span);
        }
        let raw_elem = exp.and_then(|t| self.cx.ty.array_elem(t).or(self.cx.iterable_elem(t)));
        let exp_elem = raw_elem.filter(|t| !self.cx.ty.has_error(*t));
        // A tuple element type that is only partly known (`[K, V][]` while inferring `K` and
        // `V`) still says the elements are tuples: the first one is checked against it.
        let tuple_shape = raw_elem.filter(|t| matches!(self.cx.ty.kind(*t), TyKind::Tuple(_)));
        // A function type unknown only in what it throws (from a generic callee's expected
        // result) still types the parameters of an arrow element.
        let first_hint = tuple_shape.or_else(|| raw_elem.and_then(|t| self.error_types_unknown(t)));
        if elems
            .iter()
            .any(|e| matches!(e.kind, ast::ExprKind::Spread(_)))
        {
            return self.spread_array(elems, exp_elem, span);
        }
        if elems.is_empty() {
            // Nothing says what it holds (`JSON.stringify([])`, `[].length`): `never[]`, as in TS.
            let e = exp_elem.unwrap_or(self.cx.ty.never);
            let ty = self.cx.ty.array(e);
            return self.mk(H::ArrayLit(vec![]), ty, span);
        }
        let mut out: Vec<Option<hir::Expr>> = elems.iter().map(|_| None).collect();
        let elem = match exp_elem {
            Some(e) => e,
            None => {
                let first = elems.iter().position(|e| !untyped(e)).unwrap_or(0);
                let h = self.expr(&elems[first], first_hint, Want::Move);
                // `[c.kind, "x"]` is a `string[]`: literal types widen for inference.
                let t = self.cx.widened(h.ty);
                out[first] = Some(h);
                t
            }
        };
        let elem = match exp_elem {
            None if self.cx.class_of(elem).is_some() => self.common_base(elems, &mut out, elem),
            None if self.cx.ty.promise_payload(elem).is_some() => {
                self.common_rejection(elems, &mut out, elem)
            }
            _ => elem,
        };
        let mut hs = vec![];
        for (i, e) in elems.iter().enumerate() {
            let h = match out[i].take() {
                Some(h) => self.coerce(h, elem),
                None => self.expr_coerce(e, elem, Want::Move),
            };
            hs.push(h);
        }
        let ty = self.cx.ty.array(elem);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// Element type of an array literal whose first element is of class `first`: the class of
    /// the elements that every other element's class extends (`[new B(), new A()]` is `A[]`,
    /// as TS infers the common base). Checks the elements into `out`.
    fn common_base(
        &mut self,
        elems: &[ast::Expr],
        out: &mut [Option<hir::Expr>],
        first: TyId,
    ) -> TyId {
        let mut elem = first;
        for (i, e) in elems.iter().enumerate() {
            if out[i].is_none() {
                out[i] = Some(self.expr(e, Some(elem), Want::Move));
            }
            let t = out[i].as_ref().map_or(elem, |h| h.ty);
            if t != elem {
                if let Some((base, _)) = self.cx.class_of(t) {
                    if self.cx.is_instance_of(elem, base) {
                        elem = t;
                    }
                }
            }
        }
        elem
    }

    /// Element type of an array literal of promises whose first element is `first`: promises
    /// of the same value that reject with different errors are promises rejecting with the
    /// union of them (`[fa(), fb()]` is `Promise<string, A | B>[]`, and `Promise.all` of it
    /// rejects with `A | B`), as TS has no error types to tell them apart. Checks the elements
    /// into `out`.
    fn common_rejection(
        &mut self,
        elems: &[ast::Expr],
        out: &mut [Option<hir::Expr>],
        first: TyId,
    ) -> TyId {
        let Some(value) = self.cx.ty.promise_payload(first) else {
            return first;
        };
        let mut errors = vec![];
        for (i, e) in elems.iter().enumerate() {
            if out[i].is_none() {
                out[i] = Some(self.expr(e, Some(first), Want::Move));
            }
            let t = out[i].as_ref().map_or(first, |h| h.ty);
            match self.cx.ty.kind(t) {
                TyKind::Promise(v, err) if *v == value => errors.push(*err),
                // Another value type: the element doesn't convert, which `coerce` reports.
                _ => return first,
            }
        }
        let never = self.cx.ty.never;
        let errors: Vec<TyId> = errors.into_iter().filter(|e| *e != never).collect();
        let err = self.cx.error_union(&errors).unwrap_or(never);
        self.cx.ty.promise_rejecting(value, err)
    }

    fn tuple_lit(&mut self, elems: &[ast::Expr], ts: &[TyId], ty: TyId, span: Span) -> hir::Expr {
        if elems.len() != ts.len() {
            let tn = self.cx.display(ty);
            self.cx.err(
                format!("expected a tuple of type `{tn}` ({} elements)", ts.len()),
                span,
            );
            self.check_args_loose(elems);
            return self.error_expr(span);
        }
        if !self.cx.ty.has_error(ty) {
            let hs = elems
                .iter()
                .zip(ts)
                .map(|(e, t)| self.expr_coerce(e, *t, Want::Move))
                .collect();
            return self.mk(H::Tuple(hs), ty, span);
        }
        // Partly known (an element type still being inferred): the unknown elements get the
        // types they have on their own, widened like an array literal's.
        let mut hs = vec![];
        let mut tys = vec![];
        for (e, &t) in elems.iter().zip(ts) {
            let h = if self.cx.ty.has_error(t) {
                let h = self.expr(e, None, Want::Move);
                let w = self.cx.widened(h.ty);
                self.coerce(h, w)
            } else {
                self.expr_coerce(e, t, Want::Move)
            };
            tys.push(h.ty);
            hs.push(h);
        }
        let ty = self.cx.ty.intern(TyKind::Tuple(tys));
        self.mk(H::Tuple(hs), ty, span)
    }

    /// `(name, value)` pairs of an object literal.
    fn props<'e>(
        &mut self,
        props: &'e [ast::ObjectProp],
    ) -> Vec<(ast::Ident, Option<&'e ast::Expr>)> {
        let mut out: Vec<(ast::Ident, Option<&ast::Expr>)> = vec![];
        for p in props {
            let (name, value) = match p {
                ast::ObjectProp::KeyValue(k, v) => (k.clone(), Some(v)),
                ast::ObjectProp::Shorthand(k) => (k.clone(), None),
                ast::ObjectProp::Spread(e) => {
                    self.cx
                        .err("object spread (`...x`) is not supported yet", e.span);
                    continue;
                }
                // Literals with methods are checked by `object_method.rs`.
                ast::ObjectProp::Method(_) => continue,
            };
            if out.iter().any(|(n, _)| n.name == name.name) {
                self.cx.err(
                    format!("duplicate field `{}` in object literal", name.name),
                    name.span,
                );
                continue;
            }
            out.push((name, value));
        }
        out
    }

    pub(super) fn prop_value(
        &mut self,
        name: &ast::Ident,
        value: Option<&ast::Expr>,
        exp: Option<TyId>,
    ) -> hir::Expr {
        match value {
            Some(v) => self.expr(v, exp, Want::Move),
            None => self.ident_expr(name, exp, Want::Move),
        }
    }

    pub(crate) fn object_lit(
        &mut self,
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Method(_)))
        {
            return self.method_object(props, span);
        }
        let exp = match self.hint(exp).filter(|t| self.cx.union_def(*t).is_some()) {
            Some(u) => match self.union_member_for(props, u) {
                Ok(Some(m)) => Some(m),
                Ok(None) => exp,
                Err(()) => return self.error_expr(span),
            },
            None => exp,
        };
        let target = self.hint(exp).and_then(|t| self.adt_of(t));
        if let Some((d, args)) = target {
            if let Some((k, v)) = self.hint(exp).and_then(|t| self.record_args(t)) {
                return self.record_literal(d, k, v, props, span);
            }
            if self.is_class_def(d) {
                let cn = self.cx.adt(d).map(|a| a.name.clone()).unwrap_or_default();
                self.cx.error(
                    Diagnostic::error(
                        format!("expected an instance of class `{cn}`, found an object literal"),
                        span,
                    )
                    .with_note(self.cx.creation_note(d)),
                );
                return self.error_expr(span);
            }
            // Unknown type arguments (a generic callee's `T` not inferred yet) come from the fields;
            // one unknown only in its error types (a function type) is still a hint for them.
            let error = self.cx.ty.error;
            let hints: Vec<Option<TyId>> =
                args.iter().map(|a| self.error_types_unknown(*a)).collect();
            let slots = args
                .iter()
                .zip(&hints)
                .map(|(a, h)| (*a != error && h.is_none()).then_some(*a))
                .collect();
            if super::spread::has_spread(props) {
                return self.spread_object(props, Some((d, slots)), None, span);
            }
            return self.fill_struct(d, slots, &hints, props, None, span);
        }
        if super::spread::has_spread(props) {
            return self.spread_object(props, None, None, span);
        }
        let ps = self.props(props);
        let mut fields = vec![];
        let mut hs = vec![];
        for (name, value) in &ps {
            let h = self.prop_value(name, *value, None);
            fields.push((name.name.clone(), h.ty));
            hs.push(h);
        }
        let (def, type_args) = self.anon_def(&fields);
        let ty = self.cx.ty.intern(TyKind::Adt(def, type_args.clone()));
        let kind = H::AdtLit {
            def,
            type_args,
            fields: hs,
        };
        self.mk(kind, ty, span)
    }

    /// The anonymous object def of this shape (generic over the type params it mentions).
    pub(super) fn anon_def(&mut self, fields: &[(String, TyId)]) -> (DefId, Vec<TyId>) {
        self.cx.anon_def(fields, self.module)
    }

    pub(crate) fn struct_lit(
        &mut self,
        name: &ast::TypeExpr,
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some((d, slots)) = self.struct_name(name) else {
            self.props(props).iter().for_each(|(n, v)| {
                self.prop_value(n, *v, None);
            });
            return self.error_expr(span);
        };
        if super::spread::has_spread(props) {
            return self.spread_object(props, Some((d, slots)), self.hint(exp), span);
        }
        self.fill_struct(d, slots, &[], props, self.hint(exp), span)
    }

    /// `P` / `P<T>` in `P { .. }`: the struct def and its (possibly unknown) type args.
    fn struct_name(&mut self, t: &ast::TypeExpr) -> Option<(DefId, Vec<Option<TyId>>)> {
        let ast::TypeExprKind::Named { path, args } = &t.kind else {
            self.cx.err("expected a struct name", t.span);
            return None;
        };
        let name = path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let item = self.lookup_type_path(path);
        let Some(Item::Def(d)) = item else {
            self.cx
                .err(format!("cannot find struct `{name}` in this scope"), t.span);
            return None;
        };
        let Some(a) = self.cx.adt(d) else {
            self.cx.err(format!("`{name}` is not a struct"), t.span);
            return None;
        };
        if a.kind == AdtKind::Class {
            let note = self.cx.creation_note(d);
            self.cx
                .error(Diagnostic::error(format!("`{name}` is a class"), t.span).with_note(note));
            return None;
        }
        let n = a.generics.len();
        if !args.is_empty() && args.len() != n {
            self.cx.err(
                format!("struct `{name}` takes {n} type argument(s)"),
                t.span,
            );
            return None;
        }
        let slots = if args.is_empty() {
            vec![None; n]
        } else {
            args.iter().map(|a| Some(self.resolve(a))).collect()
        };
        Some((d, slots))
    }

    fn fill_struct(
        &mut self,
        d: DefId,
        mut slots: Vec<Option<TyId>>,
        hints: &[Option<TyId>],
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let a = self.cx.adt(d).expect("ICE: struct");
        let fields = a.fields.clone();
        let sname = self.literal_name(d);
        let values = self.struct_values(d, &sname, &fields, (&mut slots, hints), props);
        self.finish_struct(d, slots, values, exp, span)
    }

    /// The name of `d` for messages about a literal of it: the alias an anonymous object type is
    /// declared as (`AB` for `type AB = A & B`), else `d`'s own name.
    fn literal_name(&mut self, d: DefId) -> String {
        let a = self.cx.adt(d).expect("ICE: struct");
        let name = a.name.clone();
        if a.kind != AdtKind::Anon || !a.generics.names.is_empty() {
            return name;
        }
        let t = self.cx.ty.intern(TyKind::Adt(d, vec![]));
        self.cx.alias_names.get(&t).cloned().unwrap_or(name)
    }

    /// A struct literal from its values by field index (omitted ones take their default).
    pub(super) fn finish_struct(
        &mut self,
        d: DefId,
        mut slots: Vec<Option<TyId>>,
        mut values: Vec<Option<hir::Expr>>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        crate::body::field_defaults(self.cx, d);
        let a = self.cx.adt(d).expect("ICE: struct");
        let (fields, kind) = (a.fields.clone(), a.kind);
        let anon_kind = kind == AdtKind::Anon;
        let sname = self.literal_name(d);
        // Object types (anonymous ones and field-only interfaces) may leave out nullable fields.
        let anon = anon_kind || self.cx.field_only_of.contains_key(&d);
        if let Some(e) = exp {
            let pat = crate::collect::self_type(self.cx, d, slots.len());
            self.cx.match_ty(pat, e, &mut slots);
        }
        let names = self
            .cx
            .adt(d)
            .map(|a| a.generics.names.clone())
            .unwrap_or_default();
        let type_args: Vec<TyId> = slots
            .iter()
            .zip(names)
            .map(|(s, pn)| {
                s.unwrap_or_else(|| {
                    self.cx.err(
                        format!("cannot infer type parameter `{pn}` of `{sname}`"),
                        span,
                    );
                    self.cx.ty.error
                })
            })
            .collect();
        let mut hs = vec![];
        for (i, f) in fields.iter().enumerate() {
            let fty = self.cx.subst(f.ty, &type_args);
            let h = match (values[i].take(), &f.default) {
                (Some(h), _) => self.coerce(h, fty),
                // `a?: T | null` left out is absent, not a present `null`.
                (None, _) if crate::anon::has_presence(&self.cx.ty, kind, f) => {
                    self.intrinsic(hir::Intrinsic::FieldAbsent, vec![], fty, span)
                }
                (None, Some(dflt)) => {
                    let mut h = dflt.clone();
                    crate::visit::map_expr_types(&mut h, &mut |t| self.cx.subst(t, &type_args));
                    for s in &f.default_throws {
                        let s = s.used_at(span, |t| self.cx.subst(t, &type_args));
                        self.throw_src(s);
                    }
                    h
                }
                // `{ b?: T }` is `{ b: T | null }`: an object literal may leave such a field out.
                (None, None) if anon && self.cx.ty.opt_payload(fty).is_some() => {
                    self.mk(H::Lit(hir::Lit::Null), fty, span)
                }
                (None, None) => {
                    self.cx.err(
                        format!("missing field `{}` in `{sname}` literal", f.name),
                        span,
                    );
                    self.error_expr(span)
                }
            };
            hs.push(h);
        }
        let ty = self.cx.ty.intern(TyKind::Adt(d, type_args.clone()));
        let kind = H::AdtLit {
            def: d,
            type_args,
            fields: hs,
        };
        self.mk(kind, ty, span)
    }

    /// The literal's values by field index, inferring type args from them.
    fn struct_values(
        &mut self,
        d: DefId,
        sname: &str,
        fields: &[FieldInfo],
        (slots, hints): (&mut [Option<TyId>], &[Option<TyId>]),
        props: &[ast::ObjectProp],
    ) -> Vec<Option<hir::Expr>> {
        let ps = self.props(props);
        let mut values: Vec<Option<hir::Expr>> = fields.iter().map(|_| None).collect();
        for (pname, value) in &ps {
            let Some(i) = fields.iter().position(|f| f.name == pname.name) else {
                self.cx.error(
                    Diagnostic::error(
                        format!("no field `{}` on type `{sname}`", pname.name),
                        pname.span,
                    )
                    .with_note(
                        "objects have a fixed shape; use a `Map<string, V>` for dynamic keys",
                    ),
                );
                self.prop_value(pname, *value, None);
                continue;
            };
            self.check_private(fields[i].private_to, &pname.name, pname.span);
            self.cx
                .rec_ref(pname.span, crate::ide::record::Target::Field(d, i as u32));
            let known: Vec<Option<TyId>> = slots
                .iter()
                .enumerate()
                .map(|(k, s)| s.or(hints.get(k).copied().flatten()))
                .collect();
            let expected = self.cx.subst_known(fields[i].ty, &known);
            let h = self.prop_value(pname, *value, Some(expected));
            self.cx.match_ty(fields[i].ty, h.ty, slots);
            values[i] = Some(h);
        }
        values
    }

    /// `t` when only its error types are unknown (`Error`): a hint, though not a type yet.
    fn error_types_unknown(&self, t: TyId) -> Option<TyId> {
        (self.cx.ty.has_error(t) && !self.cx.ty.has_error_outside_error_types(t)).then_some(t)
    }
}
