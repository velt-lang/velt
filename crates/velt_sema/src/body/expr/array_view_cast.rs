//! `xs as U[]` of an existing array of another element type (#1485): TypeScript keeps the one
//! array, so Velt makes it a view of that array (velt_vir lower/array_view.rs), written as the
//! conversion through `any` that a view comes from: `AnyTo(U[], xs as any)`. The element
//! types must be comparable (one converts to the other), as `as` needs in TypeScript; a union
//! target with one array member (`result as JSXChildren`) takes the view as that member.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h as target` for an array `h` and a target array type (or union with one array member)
    /// of another, comparable element type: a view of `h`. `Err(h)` otherwise.
    pub(super) fn array_view_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Array(se) = self.cx.ty.kind(h.ty).clone() else {
            return Err(h);
        };
        let Some(arr) = self.array_member(target) else {
            return Err(h);
        };
        let TyKind::Array(te) = self.cx.ty.kind(arr).clone() else {
            return Err(h);
        };
        if arr == h.ty || !self.elems_comparable(se, te, span) {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let orig = h.clone();
        let Ok(a) = self.try_coerce(h, any) else {
            return Err(orig);
        };
        let Ok(v) = self.coerce_dynamic(a, arr, true) else {
            return Err(orig);
        };
        if arr == target {
            return Ok(v);
        }
        self.try_coerce(v, target).map_err(|_| orig)
    }

    /// An existing array `h` (a variable or a field, `vnode.children`) where an `any[]` or
    /// `unknown[]` (or a nullable or union with one) is expected (#924): TypeScript passes the one array, so this is the view
    /// `h as any[]` makes, not a copy. A value stored through the view converts back to the
    /// array's element type when it is written (a `TypeError` if it is not one). `Err(h)`
    /// for another value or expected type.
    pub(super) fn dynamic_array_view(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Array(se) = self.cx.ty.kind(h.ty).clone() else {
            return Err(h);
        };
        // Also as the array member of a nullable or union (`children: any[] | null`).
        let te = match self.array_member(exp).map(|a| self.cx.ty.kind(a).clone()) {
            Some(TyKind::Array(te)) => te,
            _ => return Err(h),
        };
        let dynamic_elem = matches!(
            self.cx.dynamic(te),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        );
        // An array of a type parameter's values converts per instantiation: not a view.
        if !dynamic_elem
            || self.cx.dynamic(se).is_some()
            || self.cx.has_type_params(se)
            || !crate::body::places::is_place(&h)
            || self.cx.holds_lock(se)
        {
            return Err(h);
        }
        let span = h.span;
        self.array_view_cast(h, exp, span)
    }

    /// A conversion of an `any` to `t` here: when `t` holds an array, the view it may make reads
    /// that array's members through `any`, so the `any` tables need the member slots.
    pub(super) fn note_array_view_target(&mut self, t: TyId) {
        if self.array_member(t).is_some() {
            self.note_dyn_members();
        }
    }

    /// `t` itself when it is an array type, else the one array member of the union (or
    /// nullable) `t`.
    fn array_member(&mut self, t: TyId) -> Option<TyId> {
        if matches!(self.cx.ty.kind(t), TyKind::Array(_)) {
            return Some(t);
        }
        let mut inner = t;
        while let Some(p) = self.cx.ty.opt_payload(inner) {
            inner = p;
        }
        if inner != t && matches!(self.cx.ty.kind(inner), TyKind::Array(_)) {
            return Some(inner);
        }
        let members = self.cx.union_members(inner)?;
        let mut arrays = members
            .into_iter()
            .filter(|m| matches!(self.cx.ty.kind(*m), TyKind::Array(_)));
        match (arrays.next(), arrays.next()) {
            (Some(a), None) => Some(a),
            _ => None,
        }
    }

    /// Does a value of `se` convert to `te`, or one of `te` to `se`?
    fn elems_comparable(&mut self, se: TyId, te: TyId, span: Span) -> bool {
        let probe = |lw: &mut Self, from: TyId, to: TyId| {
            let mut d = lw.error_expr(span);
            d.ty = from;
            lw.try_coerce(d, to).is_ok()
        };
        probe(self, se, te) || probe(self, te, se)
    }
}
