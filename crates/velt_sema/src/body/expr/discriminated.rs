//! Discriminated unions in expressions (`crate::discriminants`): reading a field every member
//! has (`s.kind`, a common `s.id`), testing the discriminant (`s.kind === "circle"`, a match on
//! the tag) and choosing the member an object literal builds (`{ kind: "circle", r: 1.0 }`
//! where a `Shape` is expected).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::narrow::literal_of;
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, UseMode};
use crate::literals::{display_lit, lit_matches};

impl FnCx<'_, '_> {
    /// `obj.prop` on a union value whose (possible) members all have field `prop`: a match on
    /// the member reading the field; its type is the union of the fields' types (a literal
    /// field reads as its constant, a non-Copy field as a copy — `.clone()`; narrow the value to
    /// borrow it instead). `Err(obj)` when a member has no such field.
    pub(super) fn union_field(
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
        for &v in &live {
            let m = members[v as usize];
            match self.cx.field_of(m, &prop.name) {
                Some((i, fty)) => fields.push((v, m, i, fty)),
                None => return Err(obj),
            }
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
        let ty = self.cx.union_of(&ftys, false, span);
        let consume = !is_place(&obj);
        let mut arms = vec![];
        for (v, m, i, fty) in fields {
            let (sub, body) = self.member_field(m, i, fty, consume, span);
            let body = self.coerce(body, ty);
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
            let possible: Vec<String> = live
                .iter()
                .map(|v| display_lit(&values[*v as usize]))
                .collect();
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
        let test = self.bool_match(obj, Some(pat), span);
        Ok(self.negated(test, negate, span))
    }

    /// The member of union `u` an object literal builds: the one whose discriminants match the
    /// literal's (`kind: "circle"`), else the one with exactly the literal's fields. `Ok(None)`
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
            .filter(|m| self.adt_of(*m).is_some_and(|(d, _)| !self.is_class_def(d)))
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
                cx.field_of(m, &k.name).and_then(|(_, t)| cx.lit_value(t))
            };
            if !objects.iter().any(|m| lit_field(self.cx, *m).is_some()) {
                continue;
            }
            cands.retain(|m| match self.cx.field_of(*m, &k.name) {
                Some((_, t)) => self.cx.lit_value(t).is_none_or(|x| lit_matches(&x, &l)),
                None => false,
            });
            if cands.is_empty() && wrong.is_none() {
                wrong = Some((k.clone(), l, v.span));
            }
        }
        if let Some((k, l, at)) = wrong {
            let mut shown = vec![];
            for m in &objects {
                if let Some(v) = self
                    .cx
                    .field_of(*m, &k.name)
                    .and_then(|(_, t)| self.cx.lit_value(t))
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
        if cands.len() == 1 {
            return Ok(cands.pop());
        }
        let names: Vec<&str> = props
            .iter()
            .filter_map(|p| match p {
                ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => {
                    Some(k.name.as_str())
                }
                ast::ObjectProp::Spread(_) => None,
            })
            .collect();
        let exact: Vec<TyId> = cands
            .into_iter()
            .filter(|m| {
                self.field_names(*m)
                    .is_some_and(|fs| same_names(&fs, &names))
            })
            .collect();
        Ok((exact.len() == 1).then(|| exact[0]))
    }

    fn field_names(&self, t: TyId) -> Option<Vec<String>> {
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
    }
}
