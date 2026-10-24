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
        let H::Variant { def, variant, .. } = &inner.kind else {
            return;
        };
        if self.token_path(token).is_some() {
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

/// Is `e` (in parentheses or not) a cast, `x as T`?
pub(crate) fn is_cast(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(x) => is_cast(x),
        ast::ExprKind::Cast { .. } => true,
        _ => false,
    }
}
