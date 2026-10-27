//! Discriminated unions in expressions (`crate::discriminants`): reading a field every member
//! has (`s.kind`, a common `s.id`), testing the discriminant (`s.kind === "circle"`, a match on
//! the tag) and choosing the member an object literal builds (`{ kind: "circle", r: 1.0 }`
//! where a `Shape` is expected).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::narrow::{literal_of, nothing_word};
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, UseMode};
use crate::literals::{display_lit, lit_matches};

impl FnCx<'_, '_> {
    /// `obj.prop` on a union value whose (possible) members all have field `prop`: a match on
    /// the member reading the field; its type is the union of the fields' types (a literal
    /// field reads as its constant, a non-Copy field as a copy — `.clone()`; narrow the value to
    /// borrow it instead). `Err(obj)` when a member has no such field, except for `value` on an
    /// `IteratorResult<T>`, which reads as `T | undefined` (`undefined` when done), as in
    /// TypeScript (`g().next().value`).
    pub(crate) fn union_field(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((def, _)) = self.cx.union_def(obj.ty) else {
            return Err(obj);
        };
        let members = self.cx.union_members(obj.ty).unwrap_or_default();
        let live = self
            .narrowed_variants(&obj)
            .unwrap_or_else(|| (0..members.len() as u32).collect());
        let mut fields = vec![];
        let mut without = vec![];
        let owner = self.name_owner(&prop.name);
        for &v in &live {
            let m = members[v as usize];
            match self.cx.field_seen_from(m, &prop.name, owner) {
                Some((i, fty)) => fields.push((v, m, Some(i), fty)),
                // An interface member reads the field through its getter slot.
                None => match self
                    .cx
                    .iface_member(m)
                    .and_then(|_| self.cx.member_field_ty(m, &prop.name))
                {
                    Some(fty) => fields.push((v, m, None, fty)),
                    None => without.push((v, m)),
                },
            }
        }
        // `r.value` on an `IteratorResult<T>`: `T | undefined`, `undefined` when done.
        let value_or_undefined = !without.is_empty()
            && prop.name == "value"
            && fields.iter().all(|f| f.3 != self.cx.ty.unit)
            && self.cx.is_iterator_result(obj.ty);
        if !without.is_empty() && !value_or_undefined {
            return Err(obj);
        }
        // Every member's field must be usable here (`private`, `#x`), as on a single value. A
        // `private` field is reported once (its message does not name the class); each `#x`
        // names the class that declares it.
        let mut private_reported = false;
        for &(_, m, i, _) in &fields {
            let Some(i) = i else {
                continue;
            };
            if !prop.name.starts_with(ast::PRIVATE_NAME_PREFIX) {
                let denied = self
                    .field_private_to(m, i)
                    .is_some_and(|o| !self.private_allowed(o));
                if denied && std::mem::replace(&mut private_reported, true) {
                    continue;
                }
            }
            self.check_field_private(m, i, prop);
        }
        let owns = fields.iter().any(|f| self.cx.owns_resource(f.3));
        if want == Want::BorrowMut || owns {
            let un = self.cx.display(obj.ty);
            let what = if owns { "copy" } else { "assign" };
            self.cx.error(
                Diagnostic::error(
                    format!("cannot {what} `{}` of a union value `{un}`", prop.name),
                    prop.span,
                )
                .with_note("narrow it to one member first (`switch`, `if (x.kind === ...)`)"),
            );
            return Ok(self.error_expr(span));
        }
        let ftys: Vec<TyId> = fields.iter().map(|f| f.3).collect();
        let ty = self.cx.union_with(
            &ftys,
            value_or_undefined.then_some(hir::Nothing::Undefined),
            span,
        );
        let consume = !is_place(&obj);
        let mut arms = vec![];
        for (v, m) in without {
            let wild = self.pat(P::Wildcard, m, span);
            // An `IteratorResult<any>` (`Generator<any>`): the dynamic `undefined`, as in JS
            // (`T | undefined` of a dynamic `T` is that `T`, which has no `Lit::Null`).
            let none = match self.cx.dynamic(ty) {
                Some(_) => self.dynamic_nothing(hir::Nothing::Undefined, ty, span),
                None => self.mk(H::Lit(hir::Lit::Null), ty, span),
            };
            arms.push(hir::Arm {
                pat: self.pat(
                    P::Variant {
                        def,
                        variant: v,
                        args: vec![wild],
                    },
                    obj.ty,
                    span,
                ),
                guard: None,
                body: none,
            });
        }
        for (v, m, i, fty) in fields {
            let (sub, body) = match i {
                Some(i) => self.member_field(m, i, fty, consume, span),
                None => self.member_iface_field(m, prop, fty, consume, span),
            };
            // A member's `null` field read as part of an `unknown` (`value: null | T` at
            // `T = unknown`) is the dynamic `null`.
            let alone = self.cx.ty.opt_payload(fty) == Some(self.cx.ty.never);
            let body = match self.cx.ty.nothing_of(fty) {
                Some(n) if alone && self.cx.dynamic(ty).is_some() => {
                    self.dynamic_nothing(n, ty, span)
                }
                _ => self.coerce(body, ty),
            };
            arms.push(hir::Arm {
                pat: self.pat(
                    P::Variant {
                        def,
                        variant: v,
                        args: vec![sub],
                    },
                    obj.ty,
                    span,
                ),
                guard: None,
                body,
            });
        }
        let what = "reading a field";
        let view = self.view_arm(obj.ty, consume, span, what, &mut |s, v| {
            s.view_field(v, prop, ty, span)
        });
        arms.extend(view);
        if live.len() < members.len() {
            arms.push(self.unreachable_arm(obj.ty, span));
        }
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        Ok(self.mk(kind, ty, span))
    }

    /// Pattern for member `m` and the read of its field `i` of type `fty` (a copy).
    fn member_field(
        &mut self,
        m: TyId,
        i: u32,
        fty: TyId,
        consume: bool,
        span: Span,
    ) -> (hir::Pat, hir::Expr) {
        if self.cx.lit_value(fty).is_some() {
            return (self.pat(P::Wildcard, m, span), self.lit_const(fty, span));
        }
        let mode = match (self.cx.is_copy(m), consume) {
            (true, _) => UseMode::Copy,
            (false, true) => UseMode::Move,
            (false, false) => UseMode::Borrow,
        };
        let b = self.new_local("<member>", m, false, span, LocalKind::Bind);
        let base = self.mk(H::Local(b, UseMode::Borrow), m, span);
        let copy = self.cx.is_copy(fty);
        let field = H::Field {
            base: Box::new(base),
            index: i,
            mode: if copy { UseMode::Copy } else { UseMode::Borrow },
        };
        let mut value = self.mk(field, fty, span);
        if !copy {
            value = self.intrinsic(hir::Intrinsic::Share, vec![value], fty, span);
        }
        (self.pat(P::Binding(b, mode), m, span), value)
    }

    /// Pattern for interface member `m` and the read of its field `prop` of type `fty`, through
    /// the field's getter slot (a literal field reads as its constant).
    fn member_iface_field(
        &mut self,
        m: TyId,
        prop: &ast::Ident,
        fty: TyId,
        consume: bool,
        span: Span,
    ) -> (hir::Pat, hir::Expr) {
        if self.cx.lit_value(fty).is_some() {
            return (self.pat(P::Wildcard, m, span), self.lit_const(fty, span));
        }
        let mode = match consume {
            true => UseMode::Move,
            false => UseMode::Borrow,
        };
        let b = self.new_local("<member>", m, false, span, LocalKind::Bind);
        let base = self.mk(H::Local(b, UseMode::Borrow), m, span);
        let value = match self.iface_field(base, prop, Want::Borrow, span) {
            Ok(h) => h,
            Err(_) => self.error_expr(span),
        };
        (self.pat(P::Binding(b, mode), m, span), value)
    }

    /// `x.kind == lit` on a discriminated union `x`: a match on `x`'s tag. `Err(Some(h))`: `h`
    /// is the checked `x.kind` (not a discriminant); `Err(None)`: not a member access.
    pub(super) fn discriminant_test(
        &mut self,
        e: &ast::Expr,
        lit: &ast::SignedLit,
        negate: bool,
        span: Span,
    ) -> Result<hir::Expr, Option<hir::Expr>> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &e.kind
        else {
            return Err(None);
        };
        if self.is_type_name(object) {
            return Err(None);
        }
        let obj = self.expr(object, None, Want::Borrow);
        let Some(values) = self.cx.discriminant_values(obj.ty, &prop.name) else {
            return Err(Some(self.member_of(obj, prop, Want::Borrow, e.span)));
        };
        self.rec_discriminant(prop, obj.ty);
        let (def, u) = match self.cx.union_def(obj.ty) {
            Some((d, _)) => (d, obj.ty),
            None => return Err(Some(obj)),
        };
        let members = self.cx.union_members(u).unwrap_or_default();
        let live = self
            .narrowed_variants(&obj)
            .unwrap_or_else(|| (0..members.len() as u32).collect());
        let hits: Vec<u32> = live
            .iter()
            .copied()
            .filter(|v| lit_matches(&values[*v as usize], lit))
            .collect();
        if hits.is_empty() {
            // Members with one value (an intersection split over a union) show it once.
            let mut possible: Vec<String> = vec![];
            for v in &live {
                let shown = display_lit(&values[*v as usize]);
                if !possible.contains(&shown) {
                    possible.push(shown);
                }
            }
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "this comparison is always false: `{}` is not a possible value of `{}`",
                        show_lit(lit),
                        crate::body::switch::cases::source_text(e)
                    ),
                    span,
                )
                .with_note(format!("possible values: {}", possible.join(", "))),
            );
            return Ok(self.error_expr(span));
        }
        let mut pats: Vec<hir::Pat> = hits
            .iter()
            .map(|v| self.variant_pat(def, *v, members[*v as usize], u, span))
            .collect();
        let pat = match pats.len() {
            1 => pats.pop().expect("ICE: one pattern"),
            _ => self.pat(P::Or(pats), u, span),
        };
        let (uty, consume) = (obj.ty, !is_place(&obj));
        let mut test = self.bool_match(obj, Some(pat), span);
        let what = "testing a discriminant";
        let view = self.view_arm(uty, consume, span, what, &mut |s, v| {
            let f = s.iface_field(v, prop, Want::Borrow, span).ok()?;
            s.view_lit_test(f, lit, span)
        });
        if let H::Match { arms, .. } = &mut test.kind {
            Self::push_view_arm(arms, view);
        }
        Ok(self.negated(test, negate, span))
    }

    /// [`union_member_for`](Self::union_member_for) when `exp` hints a union, else `Ok(None)`.
    pub(super) fn union_literal_member(
        &mut self,
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
    ) -> Result<Option<TyId>, ()> {
        match self.hint(exp).filter(|t| self.cx.union_def(*t).is_some()) {
            Some(u) => self.union_member_for(props, u),
            None => Ok(None),
        }
    }

    /// The member of union `u` an object literal builds: the one whose discriminants match the
    /// literal's (`kind: "circle"`), else the one with exactly the literal's fields, else the
    /// union's only `Record` member. `Ok(None)`
    /// leaves the literal to the ordinary checks; `Err` after reporting a wrong discriminant.
    pub(super) fn union_member_for(
        &mut self,
        props: &[ast::ObjectProp],
        u: TyId,
    ) -> Result<Option<TyId>, ()> {
        let objects: Vec<TyId> = self
            .cx
            .union_members(u)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| {
                self.adt_of(*m).is_some_and(|(d, _)| !self.is_class_def(d))
                    || self.cx.iface_member(*m).is_some()
            })
            .collect();
        let mut cands = objects.clone();
        let mut wrong = None;
        for p in props {
            let ast::ObjectProp::KeyValue(k, v) = p else {
                continue;
            };
            let Some(l) = literal_of(v) else {
                continue;
            };
            let lit_field = |cx: &mut crate::ctx::Ctx, m: TyId| {
                cx.member_field_ty(m, &k.name).and_then(|t| cx.lit_value(t))
            };
            if !objects.iter().any(|m| lit_field(self.cx, *m).is_some()) {
                continue;
            }
            cands.retain(|m| match self.cx.member_field_ty(*m, &k.name) {
                Some(t) => self.cx.lit_value(t).is_none_or(|x| lit_matches(&x, &l)),
                None => false,
            });
            if cands.is_empty() && wrong.is_none() {
                wrong = Some((k.clone(), l, v.span));
            }
        }
        // A generic union at `T = any` / `unknown` (`T | Mod<T>`, which TypeScript reads as
        // that `any`) takes a literal of no member's shape as that member's (`{ a: 1 }`), also
        // one whose discriminant no member has (`{ kind: "no" }` of an `Ok | unknown`).
        let dynamic = self
            .cx
            .union_members(u)
            .unwrap_or_default()
            .into_iter()
            .find(|m| {
                matches!(
                    self.cx.dynamic(*m),
                    Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
                )
            });
        if wrong.is_some() && dynamic.is_some() {
            return Ok(dynamic);
        }
        if let Some((k, l, at)) = wrong {
            let mut shown = vec![];
            for m in &objects {
                if let Some(v) = self
                    .cx
                    .member_field_ty(*m, &k.name)
                    .and_then(|t| self.cx.lit_value(t))
                {
                    shown.push(display_lit(&v));
                }
            }
            let un = self.cx.display(u);
            self.cx.error(
                Diagnostic::error(
                    format!("`{}` is not a valid `{}` for `{un}`", show_lit(&l), k.name),
                    at,
                )
                .with_note(format!("expected one of {}", shown.join(", "))),
            );
            return Err(());
        }
        // With a `Record` member as well, a struct member takes only a literal of its shape.
        if cands.len() == 1 && self.only_record_member(u).is_none() && dynamic.is_none() {
            return Ok(cands.pop());
        }
        let names: Vec<&str> = props
            .iter()
            .filter_map(|p| match p {
                ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => {
                    Some(k.name.as_str())
                }
                ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => None,
            })
            .collect();
        let mut exact: Vec<TyId> = cands
            .into_iter()
            .filter(|m| {
                self.field_names(*m)
                    .is_some_and(|fs| same_names(&fs, &names))
            })
            .collect();
        // Beside a dynamic member, a struct member takes only a literal its fields can hold
        // (`{ default: "s" }` of a `Mod<number> | unknown` is the `unknown`'s).
        if dynamic.is_some() {
            exact.retain(|m| self.literal_fields_fit(props, *m));
        }
        if exact.len() == 1 {
            return Ok(Some(exact[0]));
        }
        Ok(self.only_record_member(u).or(dynamic))
    }

    /// Do the fields of member `m` hold the values of `props`: literals (`"s"`, `1`, `true`,
    /// `null` only where the field holds it) and variables or field paths by their types?
    /// Other values are not looked at.
    fn literal_fields_fit(&mut self, props: &[ast::ObjectProp], m: TyId) -> bool {
        for p in props {
            let named;
            let (k, v) = match p {
                ast::ObjectProp::KeyValue(k, v) => (k, v),
                ast::ObjectProp::Shorthand(k) => {
                    named = ast::Expr {
                        id: ast::NodeId(u32::MAX),
                        kind: ast::ExprKind::Ident(k.clone()),
                        span: k.span,
                    };
                    (k, &named)
                }
                _ => continue,
            };
            if let Some(word) = nothing_word(v) {
                let Some(ft) = self.cx.member_field_ty(m, &k.name) else {
                    return false;
                };
                let held = self.cx.ty.nothing_of(ft).is_some_and(|h| match word {
                    "null" => h.has_null(),
                    _ => h.has_undefined(),
                });
                if !held && !self.open_field(ft) {
                    return false;
                }
                continue;
            }
            let ty = &self.cx.ty;
            let lt = match literal_of(v).map(|l| l.lit) {
                Some(ast::Lit::Str(_)) => ty.str_,
                Some(ast::Lit::Int { .. } | ast::Lit::Float { .. }) => ty.f64,
                Some(ast::Lit::Bool(_)) => ty.bool_,
                Some(ast::Lit::Null | ast::Lit::Undefined) => continue,
                None => match self.peek_ty(v) {
                    Some(t) => t,
                    None => continue,
                },
            };
            let Some(ft) = self.cx.member_field_ty(m, &k.name) else {
                return false;
            };
            let ft = self.cx.widened(ft);
            let numeric = lt == self.cx.ty.f64 && self.cx.ty.is_numeric(ft);
            if !self.open_field(ft) && !numeric && !self.converts_to(lt, ft) {
                return false;
            }
        }
        true
    }

    /// Does a field of type `ft` take any value at some instantiation (`any`, `unknown`, or a
    /// type with a type parameter in it)?
    fn open_field(&mut self, ft: TyId) -> bool {
        let mut params = vec![];
        crate::types::collect_params(&self.cx.ty, ft, &mut params);
        self.cx.dynamic(ft).is_some() || !params.is_empty()
    }

    /// The `Record` member of union `u` when it has exactly one: an object literal no struct
    /// member takes builds it (`Headers | Record<string, string>` given `{ "x-id": "1" }`).
    fn only_record_member(&mut self, u: TyId) -> Option<TyId> {
        let records: Vec<TyId> = self
            .cx
            .union_members(u)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| self.record_args(*m).is_some())
            .collect();
        (records.len() == 1).then(|| records[0])
    }

    /// The members an object literal of type `t` names: its fields, and an interface's
    /// methods too.
    fn field_names(&self, t: TyId) -> Option<Vec<String>> {
        if let Some((d, _)) = self.cx.iface_member(t) {
            let i = self.cx.iface(d)?;
            let fields = i.fields.iter().map(|f| f.name.clone());
            return Some(
                fields
                    .chain(i.methods.iter().map(|m| m.name.clone()))
                    .collect(),
            );
        }
        let (d, _) = self.adt_of(t)?;
        Some(
            self.cx
                .adt(d)?
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect(),
        )
    }
}

fn same_names(fields: &[String], names: &[&str]) -> bool {
    fields.len() == names.len() && fields.iter().all(|f| names.contains(&f.as_str()))
}

/// A literal as written (`"circle"`, `-1`, `true`).
pub(crate) fn show_lit(l: &ast::SignedLit) -> String {
    let sign = if l.negative { "-" } else { "" };
    match &l.lit {
        ast::Lit::Str(s) => format!("{s:?}"),
        ast::Lit::Int { value, .. } => format!("{sign}{value}"),
        ast::Lit::Float { value, .. } => format!("{sign}{value}"),
        ast::Lit::Bool(b) => b.to_string(),
        ast::Lit::Null => "null".into(),
        ast::Lit::Undefined => "undefined".into(),
    }
}
