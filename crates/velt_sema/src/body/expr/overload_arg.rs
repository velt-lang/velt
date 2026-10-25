//! `<overloadArg><T>(v, text)`, written only by the overload rewrite (`crate::overload_adapter`;
//! no source can spell the name): the `unknown` argument `v` an implementation passed to a
//! signature's callback, converted to the callback's parameter type `T`, checked at run time.
//! A value of another type stops the program with a `TypeError` that starts with `text`, which
//! names the callback and the argument (`Intrinsic::AnyTo` with a text, lowered by velt_vir
//! any_errors.rs). A type with members that need tests of their own (a union of several
//! types) converts as `v as T` does, with that conversion's `TypeError`.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{Intrinsic, TyKind};

/// The callee name of the conversion.
pub(crate) const OVERLOAD_ARG: &str = "<overloadArg>";

impl FnCx<'_, '_> {
    pub(super) fn overload_arg(
        &mut self,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> crate::hir::Expr {
        let ([t], [v, text]) = (type_args, args) else {
            panic!("ICE: {OVERLOAD_ARG} takes one type and two arguments");
        };
        let ast::ExprKind::Lit(ast::Lit::Str(text)) = &text.kind else {
            panic!("ICE: {OVERLOAD_ARG} text is not a string literal");
        };
        let want = self.resolve(t);
        let h = self.expr(v, None, Want::Move);
        if self.cx.dynamic(h.ty).is_none() || self.cx.ty.has_error(want) {
            return self.coerce(h, want);
        }
        // What lowering's `AnyTo` checks on its own: a type parameter (known per
        // instantiation), a primitive, a class, or one of them that may be `null`/`undefined`.
        let core = self.cx.ty.opt_payload(want).unwrap_or(want);
        let direct = self.cx.has_type_params(want)
            || matches!(
                self.cx.ty.kind(core),
                TyKind::Float(_) | TyKind::Int(_) | TyKind::Bool | TyKind::Str
            )
            || matches!(self.cx.ty.kind(core), TyKind::Adt(d, _) if self.is_class_def(*d));
        if !direct {
            let cast = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Cast {
                    expr: Box::new(v.clone()),
                    ty: t.clone(),
                },
                span,
            };
            return self.expr(&cast, Some(want), Want::Move);
        }
        let h = self.share_if_place(h);
        let text = self.str_lit(text, span);
        self.intrinsic(Intrinsic::AnyTo, vec![h, text], want, span)
    }
}
