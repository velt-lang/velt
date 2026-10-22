//! The `typeof` of one member of a union value (`typeof m` with `m: T | Mod<T>`): a constant
//! tag for a member of a known type, the dynamic tag of the instantiation's value for a member
//! that is a type parameter (`T = number` gives `"number"`, `T = unknown` the value's own) or
//! `any` / `unknown` itself (such a union at `T = unknown`, anon.rs `canon_union`).

use velt_common::Span;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, LocalId, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// The arm body for member `t` (`None`: a nothing, whose tag is `none`) of the `match` that
    /// `typeof` of a union is; binds the member's value in `pat` where the tag depends on it.
    pub(super) fn member_typeof(
        &mut self,
        pat: &mut hir::Pat,
        t: Option<TyId>,
        none: &'static str,
        span: Span,
    ) -> hir::Expr {
        let Some(t) = t else {
            return self.str_lit(none, span);
        };
        let dynamic = self.cx.dynamic(t).is_some();
        if !(self.instance_valued(t) || dynamic) || self.any_ty().is_none() {
            let tag = self.cx.typeof_tag(t);
            return self.str_lit(tag, span);
        }
        let b = self.new_local("<member>", t, false, span, LocalKind::Bind);
        if !bind_payload(pat, t, b) {
            let tag = self.cx.typeof_tag(t);
            return self.str_lit(tag, span);
        }
        let v = self.mk(H::Local(b, UseMode::Borrow), t, span);
        let v = self.param_as_any(v);
        let str_ = self.cx.ty.str_;
        self.intrinsic(hir::Intrinsic::AnyTypeof, vec![v], str_, span)
    }
}

/// Replaces the wildcard for the member's value (of type `t`) inside `pat` (`Some` and variant
/// patterns around it) with a borrowing binding of `b`. False if there is none.
fn bind_payload(pat: &mut hir::Pat, t: TyId, b: LocalId) -> bool {
    match &mut pat.kind {
        P::Wildcard if pat.ty == t => {
            pat.kind = P::Binding(b, UseMode::Borrow);
            true
        }
        P::Some(inner) => bind_payload(inner, t, b),
        P::Variant { args, .. } if args.len() == 1 => bind_payload(&mut args[0], t, b),
        _ => false,
    }
}
