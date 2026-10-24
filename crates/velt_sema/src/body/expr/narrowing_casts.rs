//! Type assertions that narrow (docs/reference/types.md "Type assertions"): `x as T` with
//! `x: T | null` (or `| undefined`), `u as A` with `u` a union that has the member `A`, and
//! `u as A | B` (also with `| undefined`) with `u` a union that has those members and more,
//! and a tuple as a tuple whose elements are each one of these (or an `any` as a static type).
//! TypeScript trusts them; Velt checks them, as it checks `x!`: the value is `T`'s, or the
//! program panics (`type assertion failed: …`). A value of the wrong type never reads as `T`.
//!
//! In generic code, `x as T` of a `T | null` is checked only at instantiations where `T` has
//! no `null` of its own (at `T = U | null`, `T | null` is `U | null`); a union that has a type
//! parameter as a member converts the other members through `any` at run time (at
//! `T = unknown`, every value is a `T`). A sub-union of members that are other types
//! (`Ok<T> | Err` of an `Ok<T> | Err | Pend`) is checked as outside generic code.

use velt_common::Span;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, PatKind as P, TyId, UseMode};
use crate::types::collect_params;

use super::union_coerce::outer_mode;

impl FnCx<'_, '_> {
    /// `h as target` when `target` is narrower than `h`'s type in one of the ways above, checked
    /// at run time; `Err(h)` otherwise.
    pub(super) fn narrowing_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let generic = |s: &Self, t: TyId| {
            let mut ps = vec![];
            collect_params(&s.cx.ty, t, &mut ps);
            !ps.is_empty()
        };
        if self.cx.ty.has_error(h.ty) {
            return Err(h);
        }
        let mut h = match self.tuple_assertion(h, target, span) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        if generic(self, h.ty) || generic(self, target) {
            if !self.param_union(h.ty) {
                // Members that are distinct types at every instantiation (`Ok<T> | Err`).
                h = match self.subunion_assertion(h, target, span) {
                    Ok(h) => return Ok(h),
                    Err(h) => h,
                };
            }
            if self.cx.ty.nothing_of(h.ty).is_none() && self.has_member(h.ty, target) {
                return self.member_assertion(h, target, span);
            }
            return self.param_present_assertion(h, target, span);
        }
        // `x as Record<string, any>` of a union with a record viewed with named members
        // (`crate::index_signatures`): the assertion to that member, as its record.
        let core = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        let view = self.cx.union_members(core).and_then(|ms| {
            ms.into_iter().find(|m| {
                self.cx
                    .record_view(*m)
                    .is_some_and(|(base, _)| base == target)
            })
        });
        if let Some(view) = view {
            return self
                .narrowing_assertion(h, view, span)
                .map(|h| self.unview(h));
        }
        let h = match self.subunion_assertion(h, target, span) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        let t = &self.cx.ty;
        if t.nothing_of(target).is_some() {
            return Err(h);
        }
        match (t.nothing_of(h.ty), t.core(h.ty)) {
            (Some(n), Some(core)) => {
                let msg = format!(
                    "TypeError: type assertion failed: the value is {}",
                    n.word()
                );
                if core != target && !self.has_member(core, target) {
                    // `props as Record<string, any>` of a `Record<string, unknown> | undefined`:
                    // the present value, then the assertion its type allows (TypeScript checks
                    // the cast against the type without the nothing).
                    let present = self.checked_present(h.clone(), &msg, None, false, span);
                    return self.try_coerce(present, target).map_err(|_| h);
                }
                let present = self.checked_present(h, &msg, None, false, span);
                if present.ty == target {
                    return Ok(present);
                }
                self.member_assertion(present, target, span)
            }
            _ if self.has_member(h.ty, target) => self.member_assertion(h, target, span),
            _ => Err(h),
        }
    }

    /// `x as T & object` for an `x` whose type is the type parameter `T` (or `T | null`)
    /// without an `object` bound: sigx's `target as T & object` after a primitive test.
    /// `T & object` is `T` at an object type and `never` at a primitive, so the value is
    /// first `x as object`, checked at run time ([`param_as_object`](Self::param_as_object):
    /// a primitive is a `TypeError`), then that object as `T & object`. The value is shared,
    /// not copied. `Err(h)` for other casts.
    pub(super) fn param_as_object_part(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        use crate::any::Dynamic;
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let hir::TyKind::Op(hir::TyOp::Intersect, parts) = self.cx.ty.kind(target).clone() else {
            return Err(h);
        };
        let mut objects = None;
        let mut rest = vec![];
        for p in parts {
            match self.cx.ty.kind(p) {
                hir::TyKind::Op(hir::TyOp::Intersect, ps) => rest.extend(ps.iter().copied()),
                _ => rest.push(p),
            }
        }
        rest.retain(|&p| match self.cx.dynamic(p) {
            Some(Dynamic::Object) => {
                objects = Some(p);
                false
            }
            _ => true,
        });
        let Some(object) = objects else {
            return Err(h);
        };
        if !matches!(self.cx.ty.kind(inner), hir::TyKind::Param(_))
            || rest.iter().any(|&p| p != inner)
        {
            return Err(h);
        }
        let boxed = self.param_as_object(h, object)?;
        self.coerce_dynamic(boxed, target, true)
    }

    /// Is `target` a member of union `u`?
    fn has_member(&mut self, u: TyId, target: TyId) -> bool {
        self.cx
            .union_members(u)
            .is_some_and(|ms| ms.contains(&target))
    }

    /// Union value `h` as its member `target`: `match (h) { Target(v) => v, _ => panic }`.
    fn member_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (Some((def, _)), Some(members)) =
            (self.cx.union_def(h.ty), self.cx.union_members(h.ty))
        else {
            return Err(h);
        };
        let Some(v) = members.iter().position(|m| *m == target) else {
            return Err(h);
        };
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let (sub, value) = if self.cx.lit_value(target).is_some() {
            let wild = hir::Pat {
                kind: P::Wildcard,
                ty: target,
                span,
            };
            (wild, self.lit_const(target, span))
        } else {
            let mode = if self.cx.is_copy(target) {
                UseMode::Copy
            } else if consume {
                UseMode::Move
            } else {
                UseMode::Borrow
            };
            let b = self.new_local("<asserted>", target, false, span, LocalKind::Bind);
            let bind = hir::Pat {
                kind: P::Binding(b, mode),
                ty: target,
                span,
            };
            (bind, self.mk(H::Local(b, mode), target, span))
        };
        let shown = self.cx.display(target);
        let msg = self.str_lit(
            &format!("TypeError: type assertion failed: the value is not of type `{shown}`"),
            span,
        );
        let never = self.cx.ty.never;
        let mut arms = vec![hir::Arm {
            pat: hir::Pat {
                kind: P::Variant {
                    def,
                    variant: v as u32,
                    args: vec![sub],
                },
                ty: h.ty,
                span,
            },
            guard: None,
            body: value,
        }];
        for (i, &m) in members.iter().enumerate() {
            if i != v && (self.instance_valued(m) || self.instance_valued(target)) {
                arms.push(self.member_as_dynamic(def, i, m, h.ty, target, consume, span));
            }
        }
        arms.push(hir::Arm {
            pat: hir::Pat {
                kind: P::Wildcard,
                ty: h.ty,
                span,
            },
            guard: None,
            body: self.intrinsic(Intrinsic::Panic, vec![msg], never, span),
        });
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, target, span))
    }

    /// The arm of a [`member_assertion`](Self::member_assertion) in generic code for another
    /// member `m` (variant `i` of union `u`), where `m` or `target` is a type parameter: the
    /// value converted through `any`, checked at run time. At an instantiation where they
    /// are the same type, or `target` is `unknown`, every value of `m` passes, as in
    /// TypeScript (`T | string` as `T` at `T = unknown` of a string); `T | Mod<T>` as `Mod<T>`
    /// at `T = unknown` converts the `unknown` as `u as Mod<unknown>` does; at another
    /// instantiation a value of `m` is a `TypeError`.
    #[allow(clippy::too_many_arguments)]
    fn member_as_dynamic(
        &mut self,
        def: hir::DefId,
        i: usize,
        m: TyId,
        u: TyId,
        target: TyId,
        consume: bool,
        span: Span,
    ) -> hir::Arm {
        let mode = match (self.cx.is_copy(m), consume) {
            (true, _) => UseMode::Copy,
            (false, true) => UseMode::Move,
            (false, false) => UseMode::Borrow,
        };
        let b = self.new_local("<member>", m, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), m, span);
        let value = self.share_if_place(value);
        let body = match self.any_ty() {
            Some(any) => {
                let boxed = self.coerce(value, any);
                self.cast_dynamic(boxed, target)
            }
            None => self.error_expr(span),
        };
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: m,
            span,
        };
        hir::Arm {
            pat: hir::Pat {
                kind: P::Variant {
                    def,
                    variant: i as u32,
                    args: vec![bind],
                },
                ty: u,
                span,
            },
            guard: None,
            body,
        }
    }

    /// `u as V` where `V` (or `V | null`) is a union of some of union `u`'s members (or one of
    /// them, with `null`): `match (u) { A(a) => V.A(a), ..., _ => panic }`, through the option
    /// when `u` may be `null` or `undefined` and `V` may be that too.
    fn subunion_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let t = &self.cx.ty;
        let (from_n, to_n) = (t.nothing_of(h.ty), t.nothing_of(target));
        let from = t.core(h.ty).unwrap_or(h.ty);
        let to = t.core(target).unwrap_or(target);
        if let (Some(a), Some(b)) = (from_n, to_n) {
            if !a.within(b) {
                return Err(h);
            }
        }
        // `T | null` as `T`, and a union as one member, are the cases above.
        if to_n.is_none() && self.cx.union_def(to).is_none() {
            return Err(h);
        }
        let Some(have) = self.cx.union_members(from) else {
            return Err(h);
        };
        let want = self.cx.union_members(to).unwrap_or_else(|| vec![to]);
        if want.len() >= have.len() || !want.iter().all(|w| have.contains(w)) {
            return Err(h);
        }
        if let (Some(n), None) = (from_n, to_n) {
            // `u as A | B` of a `A | B | C | undefined`: a nothing stops as for `u!`.
            let msg = format!(
                "TypeError: type assertion failed: the value is {}",
                n.word()
            );
            let present = self.checked_present(h, &msg, None, false, span);
            return self.members_or_panic(present, to, &want, target, span);
        }
        if from_n.is_none() {
            let inner = self.members_or_panic(h, to, &want, target, span)?;
            return Ok(match to_n {
                Some(_) => self.mk(H::WrapSome(Box::new(inner)), target, span),
                None => inner,
            });
        }
        // `match (u) { null => null, v => <v as V> }`.
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mode = match consume {
            true => UseMode::Move,
            false => UseMode::Borrow,
        };
        let b = self.new_local("<some>", from, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), from, span);
        let Ok(inner) = self.members_or_panic(value, to, &want, target, span) else {
            return Err(h);
        };
        // (`to_n` is known here: a `null` value stays one.)
        let some = self.mk(H::WrapSome(Box::new(inner)), target, span);
        let none = self.mk(H::Lit(hir::Lit::Null), target, span);
        let pat = |kind, ty| hir::Pat { kind, ty, span };
        let bind = pat(P::Binding(b, mode), from);
        let arms = vec![
            hir::Arm {
                pat: pat(P::Some(Box::new(bind)), h.ty),
                guard: None,
                body: some,
            },
            hir::Arm {
                pat: pat(P::None, h.ty),
                guard: None,
                body: none,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, target, span))
    }

    /// Union value `h` as `to`, which has the members `want` of `h`'s: each of those converts,
    /// any other member panics (`type assertion failed`, naming `shown`).
    fn members_or_panic(
        &mut self,
        h: hir::Expr,
        to: TyId,
        want: &[TyId],
        shown: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let name = self.cx.display(shown);
        let msg = format!("TypeError: type assertion failed: the value is not of type `{name}`");
        let never = self.cx.ty.never;
        self.map_members(h, to, &mut |s, value| match want.contains(&value.ty) {
            true => s.try_coerce(value, to).ok(),
            false => {
                let m = s.str_lit(&msg, span);
                Some(s.intrinsic(Intrinsic::Panic, vec![m], never, span))
            }
        })
    }

    /// `x as T` with `x: T | undefined` (or `| null`) in generic code: the value, checked to be
    /// present where `T` has no such value (`type assertion failed`). Where `T` has one
    /// (`T = U | undefined`), the two types are the same and every value passes, as in
    /// TypeScript: `HoldsNull(T) ? (x ?? <the nothing as T>) : (x ?? panic)`, a condition known
    /// per instantiation, so lowering keeps one branch (hir_encodings.md). A `T` with only the
    /// other nothing (`T = U | null` of an `undefined`) cannot hold it: a `TypeError`.
    fn param_present_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let t = &self.cx.ty;
        let (Some(n), Some(core)) = (t.nothing_of(h.ty), t.core(h.ty)) else {
            return Err(h);
        };
        if core != target || !matches!(t.kind(core), hir::TyKind::Param(_)) {
            return Err(h);
        }
        // A value that is not a place (`m.get(k) as T`) is read once, from a temporary.
        let (ty, mut lets) = (h.ty, vec![]);
        let h = match is_place(&h) {
            true => h,
            false => {
                let tmp = self.new_local("<asserted>", ty, false, span, LocalKind::Temp);
                lets.push(hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: tmp,
                        init: Some(h),
                    },
                    span,
                });
                self.mk(H::Local(tmp, UseMode::Move), ty, span)
            }
        };
        let (b, never) = (self.cx.ty.bool_, self.cx.ty.never);
        let probe = self.intrinsic(Intrinsic::NullAs, vec![], core, span);
        let cond = self.intrinsic(Intrinsic::HoldsNull, vec![probe], b, span);
        // `T`'s own nothing where it has that one (`T = U | undefined`); where it has the other
        // (`T = U | null` of an `undefined`), a `TypeError`, as `undefined as U | null` from
        // an `any` is: the value never changes.
        let null = match self.any_ty() {
            Some(any) => {
                let nothing = self.dynamic_nothing(n, any, span);
                self.cast_dynamic(nothing, core)
            }
            None => self.intrinsic(Intrinsic::NullAs, vec![], core, span),
        };
        let then = self.present_or(h.clone(), core, null, span);
        let msg = format!(
            "TypeError: type assertion failed: the value is {}",
            n.word()
        );
        let m = self.str_lit(&msg, span);
        let panic = self.intrinsic(Intrinsic::Panic, vec![m], never, span);
        let els = self.present_or(h, core, panic, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        let checked = self.mk(kind, core, span);
        if lets.is_empty() {
            return Ok(checked);
        }
        let block = hir::Block {
            stmts: lets,
            value: Some(Box::new(checked)),
            span,
        };
        Ok(self.mk(H::Block(block), core, span))
    }

    /// `match (h) { v => v, null => none }` for an `h` of type `core | null`.
    fn present_or(&mut self, h: hir::Expr, core: TyId, none: hir::Expr, span: Span) -> hir::Expr {
        let consume = outer_mode(&h) == Some(UseMode::Move);
        let mode = match (self.cx.is_copy(core), consume) {
            (true, _) => UseMode::Copy,
            (false, true) => UseMode::Move,
            (false, false) => UseMode::Borrow,
        };
        let b = self.new_local("<asserted>", core, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), core, span);
        let pat = |kind, ty| hir::Pat { kind, ty, span };
        let bind = pat(P::Binding(b, mode), core);
        let arms = vec![
            hir::Arm {
                pat: pat(P::Some(Box::new(bind)), h.ty),
                guard: None,
                body: value,
            },
            hir::Arm {
                pat: pat(P::None, h.ty),
                guard: None,
                body: none,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, core, span)
    }

    /// `t as [A, B]` with `t` a tuple of as many elements, each of which asserts (or converts)
    /// to its counterpart: `match (t) { (a, b) => (a as A, b as B) }`, element by element in
    /// order, each checked as alone.
    fn tuple_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (hir::TyKind::Tuple(from), hir::TyKind::Tuple(to)) = (
            self.cx.ty.kind(h.ty).clone(),
            self.cx.ty.kind(target).clone(),
        ) else {
            return Err(h);
        };
        if from.len() != to.len() || from == to {
            return Err(h);
        }
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let (mut pats, mut elems) = (vec![], vec![]);
        for (&f, &t) in from.iter().zip(&to) {
            let mode = match (self.cx.is_copy(f), consume) {
                (true, _) => UseMode::Copy,
                (false, true) => UseMode::Move,
                (false, false) => UseMode::Borrow,
            };
            let b = self.new_local("<element>", f, false, span, LocalKind::Bind);
            pats.push(hir::Pat {
                kind: P::Binding(b, mode),
                ty: f,
                span,
            });
            let value = self.mk(H::Local(b, mode), f, span);
            let Ok(e) = self.element_assertion(value, t, span) else {
                return Err(h);
            };
            elems.push(e);
        }
        let body = self.mk(H::Tuple(elems), target, span);
        let arm = hir::Arm {
            pat: hir::Pat {
                kind: P::Tuple(pats),
                ty: h.ty,
                span,
            },
            guard: None,
            body,
        };
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms: vec![arm],
        };
        Ok(self.mk(kind, target, span))
    }

    /// One element of a [`tuple_assertion`](Self::tuple_assertion) as `t`.
    fn element_assertion(
        &mut self,
        v: hir::Expr,
        t: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if v.ty == t {
            return Ok(v);
        }
        if self.cx.dynamic(v.ty).is_some() {
            return self.coerce_dynamic(v, t, true);
        }
        self.try_coerce(v, t)
            .or_else(|h| self.param_as_object(h, t))
            .or_else(|h| self.narrowing_assertion(h, t, span))
    }
}
