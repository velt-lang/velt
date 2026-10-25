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
