//! An object literal asserted to a discriminated union whose discriminant the literal gives as
//! a value, not a literal (#2146): `{ value: v.value, hasValue: v.hasValue } as
//! ValuePresence<T>`, where the members differ by `hasValue: true` / `hasValue: false`.
//!
//! TypeScript checks the assertion against the union as a whole, and the object is the one
//! the literal makes. Velt builds a union member, so the member is chosen at run time by the
//! discriminant's value: the literal's properties are evaluated once, in order, then the member
//! whose discriminant equals that value is built from them, each other property asserted to
//! that member's field type as `x as T` asserts (a `T | null` as the `T` of the member with a
//! value; `null` for the member whose field is `null`, checked to be `null`). A value that
//! names no member is a `TypeError`, as other failed assertions are: building a member would
//! change the discriminant Node keeps. A member whose literal the discriminant's type can't
//! hold is not tried (`{ kind: p() }` for `p(): "square"`). When the members name every value
//! the discriminant's type has (`true` and `false` of a `boolean`), the last member needs no
//! test.

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;
use crate::body::narrow::literal_of;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, LitValue, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `expr as target` for an object literal and a discriminated union (module docs), or
    /// `None` when that is not the shape (the ordinary assertion checks it).
    pub(super) fn discriminated_cast(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Object(props) = &super::object_builtins::strip_parens(expr).kind else {
            return None;
        };
        self.cx.union_def(target)?;
        let mut keys = vec![];
        for p in props {
            match p {
                ast::ObjectProp::KeyValue(k, v) => keys.push((k.clone(), Some(v))),
                ast::ObjectProp::Shorthand(k) => keys.push((k.clone(), None)),
                _ => return None,
            }
        }
        // The discriminant: a property each member has with a literal type, all different,
        // given as a value that is not a literal (a literal picks the member statically).
        let (disc, values) = keys.iter().find_map(|(k, v)| {
            if v.is_some_and(|v| literal_of(v).is_some()) {
                return None;
            }
            let values = self.cx.discriminant_values(target, &k.name)?;
            let distinct =
                (values.iter().enumerate()).all(|(i, a)| values[..i].iter().all(|b| b != a));
            let simple = (values.iter()).all(|v| matches!(v, LitValue::Str(_) | LitValue::Bool(_)));
            (distinct && simple && values.len() > 1).then(|| (k.name.clone(), values))
        })?;
        let members = self.cx.union_members(target)?;
        // Each member has each property: a member built without one would drop a property
        // Node keeps on the object (`JSON.stringify` shows it).
        for m in &members {
            for (k, _) in &keys {
                self.cx.member_field_ty(*m, &k.name)?;
            }
        }
        self.push_scope();
        let mut stmts = vec![];
        let mut temps = vec![];
        for (k, v) in &keys {
            // A property every member types alike is checked as that type (an arrow's
            // parameters, an empty array); the others as they are written.
            let hint = match k.name == disc {
                true => None,
                false => self.common_field_ty(&members, &k.name),
            };
            let e = match v {
                Some(v) => (*v).clone(),
                None => synth(ast::ExprKind::Ident(k.clone()), k.span),
            };
            let h = self.expr(&e, hint, Want::Move);
            let ty = h.ty;
            let id = self.temp_ident("#cast", h, &mut stmts);
            temps.push((k.clone(), id, ty));
        }
        let (disc_id, disc_ty) =
            (temps.iter().find(|(k, ..)| k.name == disc)).map(|(_, id, ty)| (id.clone(), *ty))?;
        // The members this discriminant's type can name; none is the assertion that fails.
        let kept: Vec<usize> = (0..members.len())
            .filter(|&i| self.may_hold_literal(disc_ty, &values[i]))
            .collect();
        if kept.is_empty() {
            self.pop_scope();
            let (s, d) = (self.cx.display(disc_ty), self.cx.display(target));
            let msg = format!("cannot cast: `{disc}` of type `{s}` names no member of `{d}`");
            self.cx.err(msg, span);
            return Some(self.error_expr(span));
        }
        // A value no member names stops, as a failed assertion does (module docs).
        let mut out = match self.names_every_value(disc_ty, &values) {
            true => None,
            false => {
                let name = self.cx.display(target);
                let msg = format!(
                    "TypeError: type assertion failed: `{disc}` names no member of `{name}`"
                );
                let msg = self.str_lit(&msg, span);
                let never = self.cx.ty.never;
                Some(self.intrinsic(Intrinsic::Panic, vec![msg], never, span))
            }
        };
        for i in kept.into_iter().rev() {
            let lit = lit_expr(&values[i], span);
            let built = self.discriminated_member(members[i], target, &temps, (&disc, &lit), span);
            out = Some(match out {
                None => built,
                Some(els) => {
                    let test = ast::ExprKind::Binary {
                        op: ast::BinaryOp::StrictEq,
                        lhs: Box::new(disc_id.clone()),
                        rhs: Box::new(lit),
                    };
                    let cond = self.cond(&synth(test, span));
                    let kind = H::If {
                        cond: Box::new(cond),
                        then: Box::new(built),
                        els: Box::new(els),
                    };
                    self.mk(kind, target, span)
                }
            });
        }
        self.pop_scope();
        Some(self.with_temps_value(stmts, out?))
    }

    /// Member `m` of union `target` built from the evaluated properties `temps`, its
    /// discriminant the literal `lit`, each other property asserted to `m`'s field type.
    fn discriminated_member(
        &mut self,
        m: TyId,
        target: TyId,
        temps: &[(ast::Ident, ast::Expr, TyId)],
        (disc, lit): (&str, &ast::Expr),
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        let mut props = vec![];
        for (k, id, ty) in temps {
            if k.name == disc {
                props.push(ast::ObjectProp::KeyValue(k.clone(), lit.clone()));
                continue;
            }
            let value = match self.cx.member_field_ty(m, &k.name) {
                Some(fty) if *ty == fty => id.clone(),
                // `value: null`: checked to be `null` (a `TypeError` otherwise), as a `T | null`
                // is asserted to the `T` of the other member.
                Some(fty) if fty == self.cx.ty.null_alone() => {
                    let h = self.asserted_null(id, *ty, fty, k.span);
                    self.temp_ident("#cast", h, &mut stmts)
                }
                Some(fty) => {
                    let h = self.expr(id, None, Want::Move);
                    let h = match self.assert_value(h, fty, k.span) {
                        Ok(h) => h,
                        Err(h) => {
                            let (s, d) = (self.cx.display(h.ty), self.cx.display(fty));
                            self.cx.err(format!("cannot cast `{s}` as `{d}`"), k.span);
                            self.error_expr(k.span)
                        }
                    };
                    self.temp_ident("#cast", h, &mut stmts)
                }
                // Each member has each property (`discriminated_cast`).
                None => id.clone(),
            };
            props.push(ast::ObjectProp::KeyValue(k.clone(), value));
        }
        let lit_obj = synth(ast::ExprKind::Object(props), span);
        let h = self.expr(&lit_obj, Some(target), Want::Move);
        let h = self.coerce(h, target);
        self.with_temps_value(stmts, h)
    }

    /// The value of property `id` (of type `ty`) as the `null` of a member's `value: null`
    /// field (`fty`): checked to be `null` at run time.
    fn asserted_null(&mut self, id: &ast::Expr, ty: TyId, fty: TyId, span: Span) -> hir::Expr {
        if self.cx.ty.nothing_of(ty).is_none() {
            let h = self.expr(id, None, Want::Move);
            return match self.assert_value(h, fty, span) {
                Ok(h) => h,
                Err(h) => {
                    let (s, d) = (self.cx.display(h.ty), self.cx.display(fty));
                    self.cx.err(format!("cannot cast `{s}` as `{d}`"), span);
                    self.error_expr(span)
                }
            };
        }
        let test = ast::ExprKind::Binary {
            op: ast::BinaryOp::StrictEq,
            lhs: Box::new(id.clone()),
            rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Null), span)),
        };
        let cond = self.cond(&synth(test, span));
        let null = self.mk(H::Lit(hir::Lit::Null), fty, span);
        let msg = self.str_lit(
            "TypeError: type assertion failed: the value is not null",
            span,
        );
        let never = self.cx.ty.never;
        let fail = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(null),
            els: Box::new(fail),
        };
        self.mk(kind, fty, span)
    }

    /// Can a value of type `t` be the literal `v`? Only a literal type rules it out.
    pub(super) fn may_hold_literal(&mut self, t: TyId, v: &LitValue) -> bool {
        let core = self.cx.ty.core(t).unwrap_or(t);
        if let Some(x) = self.cx.lit_value(core) {
            return x == *v;
        }
        match self.cx.union_members(core) {
            Some(ms) => ms.into_iter().any(|m| self.may_hold_literal(m, v)),
            None => true,
        }
    }

    /// Do `values` (the members' discriminants) name every value a discriminant of type `t`
    /// has: `true` and `false` of a `boolean`, or each literal of a literal type?
    pub(super) fn names_every_value(&mut self, t: TyId, values: &[LitValue]) -> bool {
        let all: Vec<LitValue> = match self.cx.ty.kind(t) {
            TyKind::Bool => vec![LitValue::Bool(true), LitValue::Bool(false)],
            _ => match self.cx.lit_value(t) {
                Some(v) => vec![v],
                None => {
                    let Some(ms) = self.cx.union_members(t) else {
                        return false;
                    };
                    let mut vs = vec![];
                    for m in ms {
                        match self.cx.lit_value(m) {
                            Some(v) => vs.push(v),
                            None => return false,
                        }
                    }
                    vs
                }
            },
        };
        all.iter().all(|v| values.contains(v))
    }

    /// The type of field `name` when every member of `members` has it with that one type.
    fn common_field_ty(&mut self, members: &[TyId], name: &str) -> Option<TyId> {
        let mut found = None;
        for m in members {
            let t = self.cx.member_field_ty(*m, name)?;
            if found.is_some_and(|f| f != t) {
                return None;
            }
            found = Some(t);
        }
        found
    }
}

/// The literal expression of discriminant value `v` (a string or a boolean).
pub(super) fn lit_expr(v: &LitValue, span: Span) -> ast::Expr {
    let lit = match v {
        LitValue::Str(s) => ast::Lit::Str(s.clone()),
        LitValue::Bool(b) => ast::Lit::Bool(*b),
        // Only strings and booleans are discriminants here (`discriminated_cast`).
        LitValue::Int(..) | LitValue::Float(..) => ast::Lit::Bool(false),
    };
    synth(ast::ExprKind::Lit(lit), span)
}
