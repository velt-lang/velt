//! Values of a type parameter used as their bound's type (issue #1212): a function-type bound
//! as that function; an array-type bound, and a union bound (#379), through an `any`, checked
//! at run time (an array bound only of one primitive element type where it may be written,
//! #3062; a union bound with no array member).

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// A value of a type parameter bounded by a function type (`f: F`, `F extends (x: number)
    /// => number`) as that function type: every call site makes `F` the bound itself
    /// (`check_call`), so it is the same value (issue #1212). `Err(h)` for any other value.
    pub(crate) fn fn_bound_value(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let Some(b) = self.cx.constraint_of(p) else {
            return Err(h);
        };
        if !self.cx.exact_fn_bound(b) {
            return Err(h);
        }
        let span = h.span;
        Ok(self.mk(H::Upcast(Box::new(h)), b, span))
    }

    /// A value of a type parameter bounded by a string type (`k: K`, with `K extends string`,
    /// `K extends "a" | "b"` or `K extends Names<E>`, `type Names<E> = … & string`), where a
    /// `string` is expected: the same value, every instantiation being a string.
    pub(super) fn string_bound_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        if *self.cx.ty.kind(exp) != TyKind::Str
            || !(self.cx.constraint_of(p)).is_some_and(|b| self.cx.string_bound(b))
        {
            return Err(h);
        }
        let span = h.span;
        let boxed = self.generic_as_any(h)?;
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![boxed], exp, span))
    }

    /// A value of a type parameter bounded by an array type (`xs: T`, `T extends readonly
    /// number[]`) as that array type, where the body needs it as one (`for...of`, an argument
    /// of the bound's type; issue #1212): the value through an `any`, checked at run time as
    /// `AnyTo` checks it (an instantiation at a tuple type stops with a `TypeError`). `writes`:
    /// the value may be written through (not only read, as `for...of` reads it).
    /// `Err(h)` for any other value.
    pub(crate) fn array_bound_value(
        &mut self,
        h: hir::Expr,
        writes: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let interface_bound = self.bounds.get(p as usize).is_some_and(|b| !b.is_empty());
        let Some(b) = self.cx.constraint_of(p).filter(|_| !interface_bound) else {
            return Err(h);
        };
        let TyKind::Array(e) = *self.cx.ty.kind(b) else {
            return Err(h);
        };
        if self.cx.has_type_params(b) {
            return Err(h);
        }
        // An instantiation may be an array of a narrower element type (`number[]` for `(number |
        // string)[]`), whose view stops writes of the wider elements: where the value may be
        // written through, only an element type with one layout. A bound written read-only
        // doesn't make it safe: Velt doesn't check that a read-only array stays one (TS4104),
        // so a cast or a call can still write through it. Reading it (`for...of`) is the same
        // either way, unless its elements hold arrays, which the loop's variable can write
        // through (`exact_test`).
        if !self.one_layout(e) && (writes || !self.exact_test(e, 0)) {
            return Err(h);
        }
        let span = h.span;
        let boxed = self.generic_as_any(h)?;
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![boxed], b, span))
    }

    /// A value of a type parameter bounded by a union (`k: K`, `K extends string | readonly
    /// unknown[]`) where a union holding every member of the bound is expected (the bound
    /// itself, or a wider union: `K | null` as `KeyValue | null`, #379): the value through an
    /// `any`, converted as `as` converts it, which picks the member the instantiation is.
    /// Only where every member of the bound has an exact run-time test (`exact_test`), so an
    /// instantiation passes the test: an interface or object type member would take a subtype
    /// that the test rejects, and an enum has no test (#924). An array member may be an array of
    /// a narrower element type (`number[]` for `(number | string)[]`, `"a"[]` for `string[]`),
    /// which converts as a view that stops writes of the wider elements, so a bound with an
    /// array member is refused, also one written read-only: Velt doesn't check that a read-only
    /// array stays one (TS4104, #3061), so a cast or a call can still write through it.
    /// `Err(h)` for any other value.
    pub(crate) fn union_bound_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let interface_bound = self.bounds.get(p as usize).is_some_and(|b| !b.is_empty());
        let Some(b) = self.cx.constraint_of(p).filter(|_| !interface_bound) else {
            return Err(h);
        };
        let wide = self.flat_union(exp);
        if self.cx.union_def(b).is_none() || self.cx.union_def(wide).is_none() {
            return Err(h);
        }
        let want = self.flat_members(exp);
        let have = self.flat_members(b);
        let fits = have.iter().all(|m| want.contains(m));
        if !fits || !have.iter().all(|&m| self.exact_test(m, 0)) {
            return Err(h);
        }
        // Tested as the bound, whose members all have exact tests, then widened to `exp` by
        // re-tagging, which needs no test of `exp`'s other members (an enum has none).
        let boxed = self.generic_as_any(h)?;
        let bound = self.cast_dynamic(boxed, b);
        self.try_coerce(bound, exp)
    }

    /// `t` without its `| null` (or `| undefined`).
    fn flat_union(&mut self, t: TyId) -> TyId {
        self.cx.ty.opt_payload(t).unwrap_or(t)
    }

    /// The members of union `t` (without its `| null`), or `t` itself.
    fn flat_members(&mut self, t: TyId) -> Vec<TyId> {
        let t = self.flat_union(t);
        self.cx.union_members(t).unwrap_or_else(|| vec![t])
    }

    /// Does converting an `any` to `t` test for exactly `t` (a primitive, a literal, a class, an
    /// array or union of such, or one of those or nothing), so that a value of a type `t` may be
    /// instantiated with passes, and is used as a `t`? Not an array: an instantiation may be an
    /// array of a narrower element type (`number[]` for `(number | string)[]`, `"a"[]` for
    /// `string[]`, whose literal elements are laid out differently), which a view of it as `t`
    /// would write wider elements into (`union_bound_value`).
    fn exact_test(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        if let Some(members) = self.cx.union_members(t) {
            return members.iter().all(|&m| self.exact_test(m, depth + 1));
        }
        match self.cx.ty.kind(t).clone() {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Literal(_) => {
                true
            }
            TyKind::Option(e, _) => self.exact_test(e, depth + 1),
            TyKind::Adt(d, _) => self.is_class_def(d) && self.cx.brand_base(t).is_none(),
            _ => false,
        }
    }

    /// Is `e` one primitive type, so an array of it can't be instantiated with a narrower element
    /// type of another member (`number[]` for `(number | string)[]`)? An array of its literal
    /// types still can (`"a"[]` for `string[]`, #3062).
    fn one_layout(&self, e: TyId) -> bool {
        matches!(
            self.cx.ty.kind(e),
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
        )
    }
}
