//! Narrowing by assignment, as TypeScript does: after `x = v` (and `let x: T = v`) the variable
//! holds what `v` is until the next assignment or join. A value that is not nullish rules out
//! `null` (`WrapSome`); a value of one member of a union local's type narrows the local to that
//! member (`let u: string | number = "x"; u.length`, `else { p = new B(); } p.b`). A value
//! cast to the variable's type (`v = "7" as number | string`) has that whole type, as in
//! TypeScript, and narrows nothing.

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{self, ExprKind as H, LocalId, Nothing};

impl FnCx<'_, '_> {
    /// `token` (a local or the token of a field path, of type `ty`) was just assigned the
    /// checked value `v`: the facts `v` establishes.
    pub(crate) fn narrow_to_assigned(&mut self, token: LocalId, ty: hir::TyId, v: &hir::Expr) {
        // A value that never comes (`x = document.createElement("div")`, whose read of
        // `document` throws as on Node) leaves nothing after it to run: there the variable holds
        // what the expression's type says, which isn't null, as under `tsc`.
        if v.ty == self.cx.ty.never {
            self.narrow(&Fact::NonNull(token));
            return;
        }
        let mut inner = v;
        if let H::WrapSome(x) = &v.kind {
            // On a `T | null | undefined` a `WrapSome` of a `T | null` only rules out
            // `undefined`.
            let present = !self.is_three_state(v.ty) || matches!(x.kind, H::WrapSome(_));
            if present {
                self.narrow(&Fact::NonNull(token));
            } else {
                self.narrow(&Fact::Excludes(token, Nothing::Undefined));
            }
            inner = match &x.kind {
                H::WrapSome(y) if present && self.is_three_state(v.ty) => y,
                _ if present => x,
                _ => return,
            };
        }
        if matches!(inner.kind, H::Match { .. } | H::If { .. } | H::Block(_)) {
            return self.narrow_to_branches(token, ty, inner);
        }
        let H::Variant { def, variant, .. } = &inner.kind else {
            return;
        };
        if self.token_path(token).is_some() || self.token_global_ty(token).is_some() {
            return;
        }
        let union = self.cx.ty.core(ty).unwrap_or(ty);
        if self.cx.union_def(union).is_some_and(|(d, _)| d == *def) && inner.ty == union {
            // A view (`x as U`, `crate::union_views`) is no member.
            if self
                .cx
                .union_view(union)
                .is_some_and(|(_, v, _)| v == *variant)
            {
                return;
            }
            self.narrow(&Fact::Members(token, vec![*variant]));
            if let Some(s) = self.f.scopes.last_mut() {
                s.held.push(token);
            }
        }
    }
}

impl FnCx<'_, '_> {
    /// A union value made of branches that each give some of the members of `token`'s union
    /// type: a re-tag (`result = await result` of a `R | Promise<R>` where `R` is `A | B`), or
    /// a conditional, `||` or `&&` (`x &&= "c"` on `"" | "a" | "c"` after `x = s` with
    /// `s: "" | "a"`, #3370): those members.
    fn narrow_to_branches(&mut self, token: LocalId, ty: hir::TyId, v: &hir::Expr) {
        let union = self.cx.ty.core(ty).unwrap_or(ty);
        let Some((udef, _)) = self.cx.union_def(union) else {
            return;
        };
        if v.ty != union
            || self.token_path(token).is_some()
            || self.token_global_ty(token).is_some()
        {
            return;
        }
        // A union with a view (`crate::union_views`, a variant after the members) may hold the
        // view whatever is assigned.
        let n = self.cx.union_members(union).map_or(0, |ms| ms.len() as u32);
        let mut vs = vec![];
        if !self.branch_variants(v, udef, n, &mut vs) {
            return;
        }
        vs.sort_unstable();
        vs.dedup();
        self.narrow(&Fact::Members(token, vs));
    }

    /// The members (variants below `n` of union `udef`) that value `e` may be, gathered into
    /// `vs` through its branches; false when some branch gives something else.
    fn branch_variants(&self, e: &hir::Expr, udef: hir::DefId, n: u32, vs: &mut Vec<u32>) -> bool {
        if self.cx.ty.is_bottom(e.ty) {
            return true;
        }
        match &e.kind {
            H::Variant { def, variant, .. } if *def == udef && *variant < n => {
                vs.push(*variant);
                true
            }
            H::Match { arms, .. } => {
                (arms.iter()).all(|a| self.branch_variants(&a.body, udef, n, vs))
            }
            H::If { then, els, .. } => {
                self.branch_variants(then, udef, n, vs) && self.branch_variants(els, udef, n, vs)
            }
            H::Block(b) => {
                (b.value.as_deref()).is_some_and(|x| self.branch_variants(x, udef, n, vs))
            }
            _ => false,
        }
    }
}

/// Is `e` (in parentheses or not) a cast, `x as T`?
pub(crate) fn is_cast(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(x) => is_cast(x),
        ast::ExprKind::Cast { .. } => true,
        _ => false,
    }
}
