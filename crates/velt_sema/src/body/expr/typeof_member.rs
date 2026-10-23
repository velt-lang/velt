//! The `typeof` of one member of a union value (`typeof m` with `m: T | Mod<T>`): a constant
//! tag for a member of a known type, the dynamic tag of the instantiation's value for a member
//! that is a type parameter (`T = number` gives `"number"`, `T = unknown` the value's own) or
//! `any` / `unknown` itself (such a union at `T = unknown`, anon.rs `canon_union`); also
//! `typeof m === tag`, which asks such a member's value the same way.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::narrow::Fact;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, LocalId, Nothing, PatKind as P, TyId, TyKind, UseMode};

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
        match self.open_member_typeof(pat, t, span) {
            Some(e) => e,
            None => {
                let tag = self.cx.typeof_tag(t);
                self.str_lit(tag, span)
            }
        }
    }

    /// `typeof s === tag` (`negate`: `!==`) on a union with a member whose tag the
    /// instantiation decides (`T | Mod<T>`): a `match` whose arms are constants, or the test
    /// of the value's own tag for such a member. `Err` gives `s` back for other unions.
    pub(super) fn open_typeof_test(
        &mut self,
        s: hir::Expr,
        tag: &str,
        negate: bool,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let core = self.cx.ty.core(s.ty).unwrap_or(s.ty);
        let members = self.cx.union_members(core).unwrap_or_default();
        let open = members
            .iter()
            .any(|m| self.instance_valued(*m) || self.cx.dynamic(*m).is_some());
        if !open || self.any_ty().is_none() {
            return Err(s);
        }
        let b = self.cx.ty.bool_;
        let mut arms = vec![];
        let mut nothings = self.nothing_arms(s.ty).into_iter();
        for (mut pat, t) in self.member_patterns(s.ty, span) {
            let body = match t {
                None => {
                    let none = match nothings.next() {
                        Some(Nothing::Undefined) => "undefined",
                        _ => "object",
                    };
                    self.mk(H::Lit(hir::Lit::Bool(none == tag)), b, span)
                }
                Some(t) => match self.open_member_typeof(&mut pat, t, span) {
                    Some(v) => {
                        let want = self.str_lit(tag, span);
                        let kind = H::Binary {
                            op: hir::BinOp::Eq,
                            lhs: Box::new(v),
                            rhs: Box::new(want),
                        };
                        self.mk(kind, b, span)
                    }
                    None => {
                        let same = self.cx.typeof_tag(t) == tag;
                        self.mk(H::Lit(hir::Lit::Bool(same)), b, span)
                    }
                },
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        let m = self.mk(kind, b, span);
        Ok(self.negated(m, negate, span))
    }

    /// The facts of `typeof e === tag`: those of `typeof_split`, where a member whose tag the
    /// instantiation decides stays possible where the test fails, and where it passes unless
    /// another member takes every value with that tag (`T | string` at `T = unknown` holds a
    /// string in its `string`; `T | Mod<T>` may hold an object in its `T`).
    pub(crate) fn typeof_facts(&mut self, e: &ast::Expr, tag: &str) -> (Vec<Fact>, Vec<Fact>) {
        let (mut t, mut f) = self.typeof_split(e, tag);
        self.add_open_members(&mut t, Some(tag));
        self.add_open_members(&mut f, None);
        (t, f)
    }

    /// Adds the members whose `typeof` the instantiation decides to each `Members` fact; with
    /// `covered`, not when a fixed member takes every value of that tag.
    fn add_open_members(&mut self, facts: &mut [Fact], covered: Option<&str>) {
        for fact in facts {
            let Fact::Members(l, vs) = fact else {
                continue;
            };
            let ty = self.local_ty(*l);
            let core = self.cx.ty.core(ty).unwrap_or(ty);
            let members = self.cx.union_members(core).unwrap_or_default();
            let taken = covered.is_some_and(|tag| {
                members.iter().any(|m| {
                    let whole = match self.cx.ty.kind(*m) {
                        TyKind::Str => tag == "string",
                        TyKind::Int(_) | TyKind::Float(_) => tag == "number",
                        TyKind::Bool => tag == "boolean",
                        TyKind::Symbol => tag == "symbol",
                        _ => false,
                    };
                    whole && self.cx.brand_base(*m).is_none()
                })
            });
            if taken {
                continue;
            }
            for (i, m) in members.iter().enumerate() {
                let open = self.instance_valued(*m) || self.cx.dynamic(*m).is_some();
                if open && !vs.contains(&(i as u32)) {
                    vs.push(i as u32);
                }
            }
            vs.sort_unstable();
        }
    }

    /// The tag of member `t` read from its value (bound in `pat`) when the instantiation
    /// decides it: a type parameter or `any` / `unknown`. `None`: a constant tag.
    fn open_member_typeof(&mut self, pat: &mut hir::Pat, t: TyId, span: Span) -> Option<hir::Expr> {
        let dynamic = self.cx.dynamic(t).is_some();
        if !(self.instance_valued(t) || dynamic) || self.any_ty().is_none() {
            return None;
        }
        let b = self.new_local("<member>", t, false, span, LocalKind::Bind);
        if !bind_payload(pat, t, b) {
            return None;
        }
        let v = self.mk(H::Local(b, UseMode::Borrow), t, span);
        let v = self.param_as_any(v);
        let str_ = self.cx.ty.str_;
        Some(self.intrinsic(hir::Intrinsic::AnyTypeof, vec![v], str_, span))
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
