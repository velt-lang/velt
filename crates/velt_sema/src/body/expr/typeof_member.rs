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
    pub(crate) fn open_typeof_test(
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
    /// instantiation decides stays possible in both branches (`T | Mod<T>` at `T = unknown`;
    /// a `T | string` made from a `T` in generic code holds a string in its `T`). Where the
    /// test passes, a `Class` fact for the tag's type then reads such a member as that type,
    /// converted from `any` and checked (`open_view`).
    pub(crate) fn typeof_facts(&mut self, e: &ast::Expr, tag: &str) -> (Vec<Fact>, Vec<Fact>) {
        let (mut t, mut f) = self.typeof_split(e, tag);
        // The union the facts are about: of a local, or of a field path's token.
        let u = match self.local_with_members(e) {
            Some((_, _, u)) => u,
            None => match self.field_with_members(e) {
                Some((_, _, u, _)) => u,
                None => return (t, f),
            },
        };
        let views = self.add_open_members(&mut t, u, Some(tag));
        t.extend(views);
        self.add_open_members(&mut f, u, None);
        (t, f)
    }

    /// Adds the members of union `u` whose `typeof` the instantiation decides to each `Members`
    /// fact; with `tag` (the branch where the test passes), gives the `Class` facts that read
    /// the local as the tag's type.
    fn add_open_members(&mut self, facts: &mut [Fact], u: TyId, tag: Option<&str>) -> Vec<Fact> {
        let mut views = vec![];
        let members = self.cx.union_members(u).unwrap_or_default();
        for fact in facts {
            let Fact::Members(l, vs) = fact else {
                continue;
            };
            let mut open = false;
            for (i, m) in members.iter().enumerate() {
                if self.open_member(*m) {
                    open = true;
                    if !vs.contains(&(i as u32)) {
                        vs.push(i as u32);
                    }
                }
            }
            vs.sort_unstable();
            let view = tag
                .filter(|_| open)
                .and_then(|t| self.typeof_view(*l, u, vs, t));
            if let Some(view) = view {
                views.push(view);
            }
        }
        views
    }

    /// Where `typeof l === tag` holds and `l` (a local or field token of union `u`) is narrowed
    /// to members `vs`, some of them open: the `Class` fact reading it as the tag's type, when
    /// that is a primitive the other members of `vs` are.
    pub(crate) fn typeof_view(
        &mut self,
        l: LocalId,
        u: TyId,
        vs: &[u32],
        tag: &str,
    ) -> Option<Fact> {
        let members = self.cx.union_members(u)?;
        let live: Vec<TyId> = vs
            .iter()
            .filter_map(|v| members.get(*v as usize))
            .copied()
            .collect();
        if !live.iter().any(|m| self.open_member(*m)) {
            return None;
        }
        let fixed: Vec<TyId> = live.into_iter().filter(|m| !self.open_member(*m)).collect();
        let target = match fixed.as_slice() {
            [] => match tag {
                "number" => self.cx.ty.f64,
                "string" => self.cx.ty.str_,
                "boolean" => self.cx.ty.bool_,
                _ => return None,
            },
            [m] if self.cx.typeof_tag(*m) == tag => *m,
            _ => return None,
        };
        self.view_target(target).then_some(Fact::Class(l, target))
    }

    /// Is `m` a union member whose `typeof` the instantiation decides (a type parameter, `any`
    /// or `unknown`)?
    pub(crate) fn open_member(&self, m: TyId) -> bool {
        self.instance_valued(m) || self.cx.dynamic(m).is_some()
    }

    /// Can a `typeof` test read an open member as `t` (a primitive, not branded)?
    fn view_target(&self, t: TyId) -> bool {
        let prim = matches!(
            self.cx.ty.kind(t),
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool
        );
        prim && self.cx.brand_base(t).is_none()
    }

    /// `e`, a read of local or field token `l`, as the primitive type `target` a `typeof` test
    /// narrowed it to (`typeof_facts`), when `e` is an open member or a union of those and
    /// `target` (the members `l` is narrowed to): each open member's value converted from
    /// `any`, checked. `Err` gives `e` back otherwise.
    pub(crate) fn open_view(
        &mut self,
        l: LocalId,
        e: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.view_target(target) || self.any_ty().is_none() {
            return Err(e);
        }
        if self.instance_valued(e.ty) {
            let span = e.span;
            let v = self.param_as_any(e);
            return Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![v], target, span));
        }
        let Some(members) = self.cx.union_members(e.ty) else {
            return Err(e);
        };
        let live = self
            .allowed_members(l)
            .unwrap_or_else(|| (0..members.len() as u32).collect());
        let fits = live.iter().all(|v| {
            let m = members[*v as usize];
            m == target || self.open_member(m)
        });
        if !fits {
            return Err(e);
        }
        self.map_members_in(e, target, Some(live), &mut |s, value| {
            if value.ty == target {
                return Some(value);
            }
            let span = value.span;
            let v = match s.cx.dynamic(value.ty) {
                Some(_) => s.share_if_place(value),
                None => s.param_as_any(value),
            };
            Some(s.intrinsic(hir::Intrinsic::AnyTo, vec![v], target, span))
        })
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
