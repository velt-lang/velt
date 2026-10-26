//! Array literals with no type from context whose elements have different types: an array of
//! their union, as TypeScript infers it (`[1, "a"]` is a `(number | string)[]`).

use velt_common::Span;
use velt_syntax::ast;

use super::ops::untyped;
use crate::body::narrow::nothing_word;
use crate::body::recheck::Mark;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, TyId, TyKind};

/// The element an array literal with no type from context takes its type from: the first that
/// is neither an untyped number (`1`) nor `null` / `undefined`, else the first that is not
/// `null` / `undefined`.
pub(super) fn first_typed_elem(elems: &[ast::Expr]) -> usize {
    let plain = |e: &ast::Expr| nothing_word(e).is_none();
    (elems.iter().position(|e| plain(e) && !untyped(e)))
        .or_else(|| elems.iter().position(plain))
        .unwrap_or(0)
}

/// The nothings written as `null` / `undefined` elements of an array literal.
fn literal_nothings(elems: &[ast::Expr]) -> Option<Nothing> {
    let word = |w| match w {
        "null" => Nothing::Null,
        _ => Nothing::Undefined,
    };
    (elems.iter().filter_map(nothing_word).map(word)).reduce(Nothing::join)
}

impl FnCx<'_, '_> {
    /// An array literal with no type from context whose elements are typed `elem` (from the
    /// first one): when some element does not convert to it (`[1, "a"]`, `[a, d]` of a
    /// `number | undefined` and a `string | null`), an array of the union of the elements'
    /// types, as TypeScript infers it. Elements checked so far are in `out`.
    pub(super) fn mixed_array(
        &mut self,
        elems: &[ast::Expr],
        mut out: Vec<Option<hir::Expr>>,
        elem: TyId,
        span: Span,
    ) -> hir::Expr {
        let mut hs = vec![];
        let mut tys = vec![];
        let mut mixed = false;
        for (i, e) in elems.iter().enumerate() {
            let h = match out[i].take() {
                Some(h) => h,
                None => self.mixed_elem(e, elem),
            };
            tys.push(self.cx.widened(h.ty));
            match self.try_coerce(h, elem) {
                Ok(h) => hs.push(h),
                Err(h) => {
                    mixed |= !self.cx.ty.has_error(h.ty);
                    hs.push(h);
                }
            }
        }
        let elem = if mixed {
            let u = self.cx.union_of(&tys, false, span);
            hs = hs.into_iter().map(|h| self.coerce(h, u)).collect();
            u
        } else {
            hs = hs.into_iter().map(|h| self.coerce(h, elem)).collect();
            elem
        };
        let ty = self.cx.ty.array(elem);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// The element type `t` (from the first typed element) of an array literal with no type
    /// from context, made to allow the `null` and `undefined` its elements write: `[1, null]`
    /// is a `(number | null)[]`, `["u", undefined]` a `(string | undefined)[]`.
    pub(super) fn with_literal_nothings(&mut self, elems: &[ast::Expr], t: TyId) -> TyId {
        let Some(n) = literal_nothings(elems) else {
            return t;
        };
        let ty = &self.cx.ty;
        if ty.is_bottom(t) || ty.has_error(t) || self.cx.dynamic(t).is_some() {
            return t;
        }
        match (ty.nothing_of(t), ty.core(t)) {
            (Some(m), Some(core)) if m.join(n) != m => self.cx.ty.option_of(core, m.join(n)),
            (Some(_), _) => t,
            (None, _) => self.cx.ty.option_of(t, n),
        }
    }

    /// The type of an array literal of only `null` and `undefined` elements with no type from
    /// context: `null`, `undefined`, or `null | undefined` (`[null, undefined]`).
    pub(super) fn all_nothings_ty(&mut self, elems: &[ast::Expr]) -> Option<TyId> {
        if !elems.iter().all(|e| nothing_word(e).is_some()) {
            return None;
        }
        let n = literal_nothings(elems)?;
        let never = self.cx.ty.never;
        Some(self.cx.ty.option_of(never, n))
    }

    /// `[null, undefined]`-like literal of element type `t` (`all_nothings_ty`).
    pub(super) fn nothings_array(&mut self, elems: &[ast::Expr], t: TyId, span: Span) -> hir::Expr {
        let hs = elems
            .iter()
            .map(|e| self.expr_coerce(e, t, Want::Move))
            .collect();
        let ty = self.cx.ty.array(t);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// An element of an array literal with no type from context, checked against the type
    /// `elem` the first one gave. A nested array literal that does not fit it is checked
    /// alone, so that it keeps its own type for the union (`[[1], ["a"]]` is a
    /// `(number[] | string[])[]`).
    pub(super) fn mixed_elem(&mut self, e: &ast::Expr, elem: TyId) -> hir::Expr {
        let nested = matches!(&e.kind, ast::ExprKind::Array(xs) if !xs.is_empty());
        // `elem` is only what the first element suggests: a narrowed module variable or field
        // whose payload is not an `elem` either is read as it is, so the array is of the union
        // (`[s, st.v]` after a call made `st.v` a number); one whose payload is, as its payload
        // (`[x, y]` is a `Box[]`, `checked_copies`).
        if !nested && self.as_is_source(e) {
            let payload = self.with_payload(e, |s| s.peek_ty(e));
            let fits = payload.is_some_and(|t| {
                let t = self.cx.widened(t);
                self.converts_to(t, elem)
            });
            if !fits {
                return self.as_is_expr(e, Some(elem), Want::Move);
            }
        }
        if !nested {
            return self.expr(e, Some(elem), Want::Move);
        }
        let mark = Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let h = self.expr(e, Some(elem), Want::Move);
        if self.cx.diags.len() == diags {
            return h;
        }
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        self.expr(e, None, Want::Move)
    }

    /// The element type of `[...xs, a, b]` with no type from context, `elem` from the spread
    /// arrays: the union with the plain elements' types when one does not convert to it
    /// (`[...m, true]` of an `(number | string)[]` is an `(number | string | boolean)[]`). The
    /// plain elements are checked into `plain`.
    pub(super) fn spread_plain_union(
        &mut self,
        elems: &[ast::Expr],
        plain: &mut [Option<hir::Expr>],
        elem: TyId,
        span: Span,
    ) -> TyId {
        let elem = self.with_literal_nothings(elems, elem);
        let mut tys = vec![elem];
        for (i, e) in elems.iter().enumerate() {
            if matches!(e.kind, ast::ExprKind::Spread(_)) {
                continue;
            }
            let h = self.mixed_elem(e, elem);
            let t = self.cx.widened(h.ty);
            if !self.cx.ty.has_error(t) && !self.converts_to(t, elem) {
                tys.push(t);
            }
            plain[i] = Some(h);
        }
        if tys.len() == 1 {
            return elem;
        }
        self.cx.union_of(&tys, false, span)
    }

    /// Is `elem` a function type (or one that may be `null`)? Elements of such an array are
    /// adapted to it as they are checked, so they keep the plain path.
    pub(super) fn is_fn_elem(&self, elem: TyId) -> bool {
        let t = self.cx.ty.core(elem).unwrap_or(elem);
        matches!(self.cx.ty.kind(t), TyKind::FnPtr { .. })
    }
}
