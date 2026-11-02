//! A member read on a union value asserted to one of its members, `(u as M).f`
//! (https://veltlang.com/docs/reference/types/ "Type assertions"): as in JavaScript, where `as` converts nothing.
//! The assertion `u as M` used as a value stays checked (`narrowing_casts.rs`); read for one
//! field, a value of another member gives its own property `f`, as Node reads it: an object
//! type's field `f`, an array's or string's `length`, `undefined` for a property the value does
//! not have, and a `TypeError` for `null` or `undefined` (`(u as M)?.f` is `undefined` there).
//!
//! Each member's read is known from its type, so the read is one match on the union with an
//! arm per member, and the value is never converted to `any` (which would make every array of
//! the union counted). A member whose property needs a run-time lookup (a class instance, which
//! may be of a subclass; a member of a prototype, such as an array's `map`) leaves the read to
//! the checked assertion.
//!
//! The read has the field's type `T`. A value read from another member converts to `T`,
//! checked (a `TypeError` where an array gives `undefined` for a `string` field), except where
//! the read is only tested (`CastTest`): in a condition (the operand of `!`, `&&` and `||`
//! there) each arm gives the `bool` of its value, and in a nullish or `typeof` test
//! (`=== undefined`, `== null`, `??`, `?.`, `typeof`) the read is a `T | undefined`.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Nothing, PatKind as P, TyId, TyKind, UseMode};
use crate::js_protos::{ARRAY_PROTO, NUMBER_PROTO, OBJECT_PROTO, STRING_PROTO};
use crate::types::collect_params;

use super::object_builtins::strip_parens;
use super::union_coerce::outer_mode;

/// How a field read of an assertion is used, where it is only tested.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CastTest {
    /// A condition: the read's truthiness.
    Truthy,
    /// A nullish test, `??`, `?.` or `typeof`: the value as it is, `undefined` included.
    AsIs,
    /// An operand of `<`, `>`, `<=`, `>=`, `-`, `*`, `/`, `%` or `**`: of the field's type, with
    /// a number field's `undefined` as `NaN`, which every such operator treats the same way.
    Number,
}

/// What a member of the union gives for the property read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Its own field.
    Field,
    /// An array's or string's `length`.
    Length,
    /// `undefined`: the value has no such property.
    Undefined,
}

/// Is `e` (parentheses stripped) a member read of an `as` assertion, `(x as M).f`?
pub(crate) fn is_cast_member(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Member { object, .. } => {
            matches!(strip_parens(object).kind, ast::ExprKind::Cast { .. })
        }
        _ => false,
    }
}

/// The spans of `lhs` and `rhs` (parentheses stripped) that are `(x as M).f` reads.
pub(super) fn number_operands(lhs: &ast::Expr, rhs: &ast::Expr) -> Vec<Span> {
    [lhs, rhs]
        .into_iter()
        .map(strip_parens)
        .filter(|e| is_cast_member(e))
        .map(|e| e.span)
        .collect()
}

impl FnCx<'_, '_> {
    /// `(x as M).prop` (or `?.prop`) read where `x` (a variable or a path of fields) is a union
    /// that has the object type `M`, with a field `prop`, as a member; `None` for any other
    /// member read.
    pub(super) fn cast_member_read(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        optional: bool,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if want == Want::BorrowMut {
            return None;
        }
        let ast::ExprKind::Cast { expr: inner, ty } = &strip_parens(object).kind else {
            return None;
        };
        let from = self.peek_ty(inner)?;
        // Checked again where it is used: its errors are reported there.
        let mark = self.cx.diags.len();
        let target = self.resolve(ty);
        self.cx.diags.truncate(mark);
        let union = self.cx.ty.core(from).unwrap_or(from);
        if let Some(h) = self.cast_field_read(inner, from, target, prop, optional, want, span) {
            return Some(h);
        }
        let reads = self.cast_member_reads(union, target, &prop.name)?;
        let mode = match self.cast_member_test {
            Some((s, m)) if s == span => Some(m),
            _ if self.cast_member_numbers.contains(&span) => Some(CastTest::Number),
            _ => None,
        };
        if optional {
            // The chain gives `undefined` anyway: a member's `undefined` is read as it is.
            let mode = mode.or(Some(CastTest::AsIs));
            let h = self.optional_chain(inner, span, |s, v| match v.ty == union {
                true => s.cast_member_match(v, target, &reads, prop, mode, span),
                false => s.error_expr(span),
            });
            return Some(h);
        }
        let h = self.expr(inner, None, Want::Borrow);
        // `null` / `undefined` has no properties: Node's `TypeError`.
        let h = match self.cx.ty.nothing_of(h.ty) {
            Some(n) => {
                let msg = format!(
                    "TypeError: Cannot read properties of {} (reading '{}')",
                    n.word(),
                    prop.name
                );
                self.checked_present(h, &msg, None, false, span)
            }
            None => h,
        };
        if h.ty != union {
            // Read again by the member read that checks the assertion.
            self.cx.diags.truncate(mark);
            return None;
        }
        Some(self.cast_member_match(h, target, &reads, prop, mode, span))
    }

    /// The match on the union value `h`, an arm per member as `reads` says.
    fn cast_member_match(
        &mut self,
        h: hir::Expr,
        target: TyId,
        reads: &[Read],
        prop: &ast::Ident,
        mode: Option<CastTest>,
        span: Span,
    ) -> hir::Expr {
        let union = h.ty;
        let (Some((def, _)), Some(members)) =
            (self.cx.union_def(union), self.cx.union_members(union))
        else {
            return self.error_expr(span);
        };
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mut arms = Vec::with_capacity(members.len());
        // The member `M` first: its field's type is the read's.
        let order = std::iter::once(target).chain(members.iter().copied().filter(|&m| m != target));
        let mut ty = None;
        for m in order {
            let v = members.iter().position(|&x| x == m).unwrap_or(0);
            let read = reads[v];
            let (sub, body) = if read == Read::Undefined {
                let wild = self.pat(P::Wildcard, m, span);
                (wild, None)
            } else {
                let mode = match (self.cx.is_copy(m), consume) {
                    (true, _) => UseMode::Copy,
                    (false, true) => UseMode::Move,
                    (false, false) => UseMode::Borrow,
                };
                let b = self.new_local("<member>", m, false, span, LocalKind::Bind);
                let value = self.mk(H::Local(b, mode), m, span);
                let read = self.member_of(value, prop, Want::Borrow, span);
                (self.pat(P::Binding(b, mode), m, span), Some(read))
            };
            let t = *ty.get_or_insert_with(|| body.as_ref().map_or(self.cx.ty.error, |b| b.ty));
            let body = self.cast_member_value(body, t, mode, span);
            let kind = P::Variant {
                def,
                variant: v as u32,
                args: vec![sub],
            };
            arms.push(hir::Arm {
                pat: self.pat(kind, union, span),
                guard: None,
                body,
            });
        }
        let t = ty.unwrap_or(self.cx.ty.error);
        let ty = match mode {
            Some(CastTest::Truthy) => self.cx.ty.bool_,
            Some(CastTest::AsIs) => self.as_is_ty(t),
            Some(CastTest::Number) | None => t,
        };
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, ty, span)
    }

    /// One arm's value (`None`: `undefined`) as the read gives it: of the field's type `t`,
    /// tested, or as it is.
    fn cast_member_value(
        &mut self,
        v: Option<hir::Expr>,
        t: TyId,
        mode: Option<CastTest>,
        span: Span,
    ) -> hir::Expr {
        let nothing = |s: &mut Self, t: TyId| s.nothing_value(Nothing::Undefined, t, span);
        match (mode, v) {
            (Some(CastTest::Truthy), Some(v)) => self.truthy(v),
            (Some(CastTest::Truthy), None) => {
                self.mk(H::Lit(hir::Lit::Bool(false)), self.cx.ty.bool_, span)
            }
            (Some(CastTest::AsIs), v) => {
                let t = self.as_is_ty(t);
                match v {
                    Some(v) => self.fit(v, t),
                    None => nothing(self, t),
                }
            }
            (Some(CastTest::Number) | None, Some(v)) => self.fit(v, t),
            (Some(CastTest::Number), None) if self.cx.ty.is_float(t) => {
                self.mk(H::Lit(hir::Lit::Float(f64::NAN)), t, span)
            }
            (Some(CastTest::Number), None) => self.cast_member_value(None, t, None, span),
            (None, None) if self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined()) => {
                nothing(self, t)
            }
            (None, None) => match self.any_ty() {
                // Node's value is `undefined`, which a `t` cannot hold: the conversion's
                // `TypeError`.
                Some(any) => {
                    let u = self.dynamic_nothing(Nothing::Undefined, any, span);
                    self.cast_dynamic(u, t)
                }
                None => self.error_expr(span),
            },
        }
    }

    /// `v` as a `t`: as it is, converted, or through `any`, checked (an object type's field of
    /// another type than `M`'s).
    fn fit(&mut self, v: hir::Expr, t: TyId) -> hir::Expr {
        if v.ty == t {
            return v;
        }
        match self.try_coerce(v, t) {
            Ok(h) => h,
            Err(v) => match self.any_ty() {
                Some(any) => {
                    let v = self.share_if_place(v);
                    let boxed = self.coerce(v, any);
                    self.cast_dynamic(boxed, t)
                }
                None => self.error_expr(v.span),
            },
        }
    }

    /// `t | undefined` (`t` itself when it has `undefined`).
    fn as_is_ty(&mut self, t: TyId) -> TyId {
        match self.cx.ty.nothing_of(t) {
            Some(n) if n.has_undefined() => t,
            _ if self.cx.ty.is_bottom(t) => t,
            _ => self.cx.ty.option_of(t, Nothing::Undefined),
        }
    }

    /// What each member of union `u` (outside generic code) gives for `(u as target).name`,
    /// where `target` is an object type member with the field `name`; `None` when the read is
    /// left to the checked assertion.
    fn cast_member_reads(&mut self, u: TyId, target: TyId, name: &str) -> Option<Vec<Read>> {
        let generic = |s: &Self, t: TyId| {
            let mut ps = vec![];
            collect_params(&s.cx.ty, t, &mut ps);
            !ps.is_empty()
        };
        if self.cx.ty.has_error(u) || self.cx.ty.has_error(target) {
            return None;
        }
        if generic(self, u) || generic(self, target) || self.cx.union_def(u).is_none() {
            return None;
        }
        let members = self.cx.union_members(u)?;
        if !members.contains(&target) || self.adt_of(target).is_none() {
            return None;
        }
        self.field_of(target, name)?;
        let named = |names: &[&str]| names.contains(&name) || OBJECT_PROTO.contains(&name);
        let index = name.starts_with(|c: char| c.is_ascii_digit());
        let mut reads = Vec::with_capacity(members.len());
        for m in members {
            let read = if self.adt_of(m).is_some() {
                if self.field_of(m, name).is_some() {
                    Read::Field
                } else if named(&[])
                    || self.record_args(m).is_some()
                    || self.is_class_ty(m)
                    || self.has_getter(m, name)
                    || self.is_method_value(m, name)
                {
                    return None;
                } else {
                    Read::Undefined
                }
            } else if self.cx.lit_value(m).is_some() {
                if named(STRING_PROTO) || named(NUMBER_PROTO) || name == "length" || index {
                    return None;
                }
                Read::Undefined
            } else if m == self.cx.ty.str_ || self.cx.ty.array_elem(m).is_some() {
                let protos = if m == self.cx.ty.str_ {
                    STRING_PROTO
                } else {
                    ARRAY_PROTO
                };
                if name == "length" {
                    Read::Length
                } else if named(protos) || index {
                    return None;
                } else {
                    Read::Undefined
                }
            } else if self.cx.ty.is_numeric(m) || m == self.cx.ty.bool_ {
                match named(NUMBER_PROTO) {
                    true => return None,
                    false => Read::Undefined,
                }
            } else {
                return None;
            };
            reads.push(read);
        }
        Some(reads)
    }

    /// `(x as M).f` (or `?.f`) where `x` is no union and declares the field `f` itself, and `M`
    /// is an object type (or one or `null`/`undefined`) with a field `f`: `x.f`, converted to
    /// `M`'s field type (#1185, sigx's `(ctx as { parent?: Ctx<any> | null }).parent` of a
    /// `Ctx<any>` whose `parent` is a `Ctx | null`). TypeScript reads `x`'s own property, so
    /// the assertion converts nothing; the read converts as an assignment would (a generic
    /// interface to `any` arguments). `None` when the field does not convert, or another rule
    /// applies.
    #[allow(clippy::too_many_arguments)] // the parts of the member read, as `cast_member_read` has them
    fn cast_field_read(
        &mut self,
        inner: &ast::Expr,
        from: TyId,
        target: TyId,
        prop: &ast::Ident,
        optional: bool,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let mut object = target;
        while let Some(p) = self.cx.ty.opt_payload(object) {
            object = p;
        }
        let core = self.cx.ty.core(from).unwrap_or(from);
        let plain_object = self.adt_of(object).is_some()
            && !self.is_class_ty(object)
            && self.record_args(object).is_none()
            && self.cx.union_def(object).is_none();
        if !plain_object
            || self.cx.union_def(core).is_some()
            || self.cx.ty.has_error(core)
            || !self.declares_field(core, &prop.name)
            || !self.overlaps(core, object, span)
        {
            return None;
        }
        let (d, args) = self.adt_of(object)?;
        let field = self
            .cx
            .adt(d)?
            .fields
            .iter()
            .find(|f| f.name == prop.name)?
            .clone();
        let mut ty = self.cx.subst(field.ty, &args);
        // `?.f` reads `undefined` where `x` is missing, as an optional field does when absent.
        let undefined = matches!(
            self.cx.ty.nothing_of(ty),
            Some(crate::hir::Nothing::Undefined | crate::hir::Nothing::Both)
        );
        if (field.optional || optional) && !undefined {
            ty = self.cx.ty.option_of(ty, crate::hir::Nothing::Undefined);
        }
        let mark = self.cx.diags.len();
        let read = self.member(inner, prop, optional, None, want, span);
        if self.cx.diags.len() > mark {
            self.cx.diags.truncate(mark);
            return None;
        }
        self.try_coerce(read, ty).ok()
    }

    /// Do the value type `x` and the object type `m` overlap as tsc's assertion needs (TS2352):
    /// every field `m` requires is one of `x`'s, of a type comparable to it (`x` is assignable
    /// to `m`), or every field `x` has is one of `m`'s (`m` is assignable to `x`)? A field the
    /// other type lacks rules a direction out, so `(c as { n: number; extra: string }).n` of a
    /// `{ name: string; n: number }` stays a cast error.
    fn overlaps(&mut self, x: TyId, m: TyId, span: Span) -> bool {
        let Some(xs) = self.own_fields(x) else {
            return false;
        };
        let Some(ms) = self.own_fields(m) else {
            return false;
        };
        let mark = self.cx.diags.len();
        let mut covered = |need: &[(String, TyId, bool)], have: &[(String, TyId, bool)]| {
            need.iter()
                .filter(|(_, _, optional)| !optional)
                .all(|(n, t, _)| {
                    have.iter()
                        .find(|(h, _, _)| h == n)
                        .is_some_and(|(_, u, _)| self.comparable(*u, *t, span))
                })
        };
        let ok = covered(&ms, &xs) || covered(&xs, &ms);
        self.cx.diags.truncate(mark);
        ok
    }

    /// The fields of an object type or a fields-only view of an interface value, with their
    /// types at `t`'s arguments and whether each is optional. An interface with methods has
    /// members no object type of fields gives: `None` then, unless `t` is only read from.
    fn own_fields(&mut self, t: TyId) -> Option<Vec<(String, TyId, bool)>> {
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(d, args) => {
                let fields = self.cx.adt(d)?.fields.clone();
                Some(
                    fields
                        .iter()
                        .map(|f| (f.name.clone(), self.cx.subst(f.ty, &args), f.optional))
                        .collect(),
                )
            }
            TyKind::Dyn(id, args) => {
                let fields = self.cx.iface(id)?.fields.clone();
                let mut out: Vec<(String, TyId, bool)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), self.cx.subst(f.ty, &args), f.optional))
                    .collect();
                // A method is a member `m` would have to give too (as a required one).
                let methods = self.cx.iface(id)?.methods.clone();
                out.extend(
                    methods
                        .iter()
                        .map(|f| (f.name.clone(), self.cx.ty.error, false)),
                );
                Some(out)
            }
            _ => None,
        }
    }

    /// Does a value of `a` convert to `b`, or one of `b` to `a`?
    fn comparable(&mut self, a: TyId, b: TyId, span: Span) -> bool {
        if a == b {
            return true;
        }
        if self.cx.ty.has_error(a) || self.cx.ty.has_error(b) {
            return false;
        }
        let probe = |s: &mut Self, from: TyId, to: TyId| {
            let mut d = s.error_expr(span);
            d.ty = from;
            s.try_coerce(d, to).is_ok()
        };
        probe(self, a, b) || probe(self, b, a)
    }

    /// Does a value of `t` have the field `name` of its own: an object type's or class's field,
    /// or an interface's (not a method or getter, which a subclass may implement otherwise)?
    fn declares_field(&mut self, t: TyId, name: &str) -> bool {
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(..) => self.field_of(t, name).is_some(),
            TyKind::Dyn(id, _) => self
                .cx
                .iface(id)
                .is_some_and(|i| i.fields.iter().any(|f| f.name == name)),
            _ => false,
        }
    }

    /// Is `t` a class instance type (whose value may be of a subclass with more members)?
    fn is_class_ty(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self.is_class_def(*d),
            _ => false,
        }
    }
}
