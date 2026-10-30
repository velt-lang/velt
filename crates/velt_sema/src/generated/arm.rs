//! `__arm(v, i)` in generated code (docs/internals/design/source-generators.md §7): whether the
//! union `v` holds its member `i` (the index into the model's `Type.arms`), a test Velt's
//! narrowing understands: where it holds, `v` is that member. Generated code writes a union
//! as its active member with one such test per member, also for members no JavaScript test
//! tells apart (`i32 | f64`, `number[] | string[]`, two object types without a discriminant).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H};

/// The name of the test.
pub const ARM: &str = "__arm";

impl FnCx<'_, '_> {
    /// `(v, i)` of `__arm(v, i)` in generated code: the argument and the member index.
    pub(crate) fn arm_call<'e>(
        &self,
        callee: &ast::Expr,
        args: &'e [ast::Expr],
    ) -> Option<(&'e ast::Expr, u32)> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if id.name != ARM || !self.cx.in_generated_file(callee.span) {
            return None;
        }
        let [v, i] = args else {
            return None;
        };
        let ast::ExprKind::Lit(ast::Lit::Int { value, .. }) = &i.kind else {
            return None;
        };
        Some((v, u32::try_from(*value).ok()?))
    }

    /// `__arm(v, i)` checked: a `bool` test of `v`'s member.
    pub(crate) fn generated_arm(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if id.name != ARM || !self.cx.in_generated_file(callee.span) {
            return None;
        }
        let Some((v, i)) = self.arm_call(callee, args) else {
            let msg =
                format!("`{ARM}` takes a union value and a member index (an integer literal)");
            self.cx.err(msg, span);
            return Some(self.error_expr(span));
        };
        // The local as declared, not as narrowed by the tests before (which may have left one
        // member): the test reads the union's tag.
        let x = match self.named_local(v) {
            Some(l) => {
                let t = self.local_ty(l);
                self.mk(H::Local(l, hir::UseMode::Borrow), t, v.span)
            }
            None => self.expr(v, None, crate::body::Want::Borrow),
        };
        // A `U | null`: the member's pattern inside a non-null one.
        let core = self.cx.ty.core(x.ty).filter(|c| *c != x.ty);
        let u = core.unwrap_or(x.ty);
        let members = self.cx.union_members(u).map(|m| m.len());
        let def = match (self.cx.ty.kind(u), members) {
            (crate::hir::TyKind::Adt(d, _), Some(n)) if (i as usize) < n => *d,
            _ => {
                let tn = self.cx.display(x.ty);
                let msg = format!("`{ARM}`: `{tn}` has no member {i}");
                self.cx.err(msg, span);
                return Some(self.error_expr(span));
            }
        };
        let b = self.cx.ty.bool_;
        let member = self.pat(
            hir::PatKind::Variant {
                def,
                variant: i,
                args: vec![],
            },
            u,
            span,
        );
        let pat = match core {
            Some(_) => self.pat(hir::PatKind::Some(Box::new(member)), x.ty, span),
            None => member,
        };
        let arms = vec![
            hir::Arm {
                pat,
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Bool(true)), b, span),
            },
            hir::Arm {
                pat: self.pat(hir::PatKind::Wildcard, x.ty, span),
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(x),
            arms,
        };
        Some(self.mk(kind, b, span))
    }
}
