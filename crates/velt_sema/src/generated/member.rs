//! `__member<E>(i)` in generated code (docs/internals/design/source-generators.md §7): member
//! `i` of the enum `E` (the index into the model's `Type.members`). Velt has no conversion from
//! a number or a string to an enum, and generated code cannot name the enum's members (its
//! type is a handle), so a generator that makes enum values, such as a JSON decoder that has
//! matched the member's value, names the member by its position.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyKind};

/// The name of the form.
pub const MEMBER: &str = "__member";

impl FnCx<'_, '_> {
    /// `__member<E>(i)` checked: the value of member `i` of enum `E`.
    pub(crate) fn generated_member(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if id.name != MEMBER || !self.cx.in_generated_file(callee.span) {
            return None;
        }
        let index = match args {
            [i] => match &i.kind {
                ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => u32::try_from(*value).ok(),
                _ => None,
            },
            _ => None,
        };
        let ([t], Some(variant)) = (type_args, index) else {
            let msg =
                format!("`{MEMBER}` takes an enum type and a member index (an integer literal)");
            self.cx.err(msg, span);
            return Some(self.error_expr(span));
        };
        let ty = self.resolve(t);
        let target = match self.cx.ty.kind(ty) {
            TyKind::Adt(d, targs) => Some((*d, targs.clone())),
            _ => None,
        };
        let members = target.as_ref().and_then(|(d, _)| {
            let e = self.cx.enum_info(*d)?;
            let c_like = !e.is_union && e.variants.iter().all(|v| v.payload.is_empty());
            c_like.then_some(e.variants.len())
        });
        let (Some((def, type_args)), Some(n)) = (target, members) else {
            let tn = self.cx.display(ty);
            self.cx
                .err(format!("`{MEMBER}` takes an enum, not `{tn}`"), t.span);
            return Some(self.error_expr(span));
        };
        if variant as usize >= n {
            let tn = self.cx.display(ty);
            let msg = format!("`{tn}` has {n} members, not a member {variant}");
            self.cx.err(msg, span);
            return Some(self.error_expr(span));
        }
        let kind = H::Variant {
            def,
            type_args,
            variant,
            args: vec![],
        };
        Some(self.mk(kind, ty, span))
    }
}
