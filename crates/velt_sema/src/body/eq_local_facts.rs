//! `a === b` of two variables, one of them nullable: where they are equal, the nullable one holds
//! what the other does, so it is not null when the other can't be (`if (!at || at !== dom)
//! return; dom.nextSibling` with `at` narrowed by `!at`), as `tsc` narrows by equality. `==`
//! narrows the same way: a value that is neither `null` nor `undefined` equals neither loosely.

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;

impl FnCx<'_, '_> {
    /// The facts of `lhs === rhs` (when true / when false) for two named variables, one of them
    /// nullable and the other not null here; `None` when that is not the shape.
    pub(super) fn local_eq_facts(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        if !is_ident(lhs) || !is_ident(rhs) {
            return None;
        }
        let a = self.named_local(lhs)?;
        let b = self.named_local(rhs)?;
        let nullable = |s: &Self, l| s.cx.ty.opt_payload(s.local_ty(l)).is_some();
        let (target, other) = match (nullable(self, a), nullable(self, b)) {
            (true, false) => (a, b),
            (false, true) => (b, a),
            (true, true) if self.is_narrowed(b) && !self.is_narrowed(a) => (a, b),
            (true, true) if self.is_narrowed(a) && !self.is_narrowed(b) => (b, a),
            _ => return None,
        };
        // The other one may be `null` or `undefined` after all when it is `any` or `unknown`.
        let ty = self.local_ty(other);
        if self.cx.dynamic(ty).is_some() || self.is_narrowed(target) {
            return None;
        }
        Some((vec![Fact::NonNull(target)], vec![]))
    }
}

fn is_ident(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) => true,
        ast::ExprKind::Paren(inner) => is_ident(inner),
        _ => false,
    }
}
