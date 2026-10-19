//! JS's conversion functions called as functions: `String(x)` writes any value as a template
//! literal part does (`${x}`), `Number(x)` converts a string (std's `Number`), a number or a
//! boolean, and `Boolean(x)` is the truthiness test of a condition.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H};

impl FnCx<'_, '_> {
    /// `String(x)`: the text of `x`, as `${x}` writes it (`String()` is `""`).
    pub(super) fn string_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        if !self.conversion_arity("String", args, span) {
            return self.error_expr(span);
        }
        let Some(a) = args.first() else {
            return self.str_lit("", span);
        };
        if is_null_lit(a) {
            return self.str_lit("null", span);
        }
        let h = self.expr(a, None, Want::Borrow);
        let part = self.js_text(h, "`String(x)`");
        self.concat_parts(vec![part], span)
    }

    /// `Number(x)`: std's `Number(s)` (function `d`) for a string, the value itself for a number,
    /// `1` / `0` for a boolean, `0` for `null` (`Number()` is `0`).
    pub(super) fn number_call(&mut self, d: DefId, args: &[ast::Expr], span: Span) -> hir::Expr {
        let f64 = self.cx.ty.f64;
        if !self.conversion_arity("Number", args, span) {
            return self.error_expr(span);
        }
        let Some(a) = args.first().filter(|a| !is_null_lit(a)) else {
            return self.mk(H::Lit(hir::Lit::Float(0.0)), f64, span);
        };
        let h = self.expr(a, None, Want::Borrow);
        let h = self.unbrand(h);
        let t = self.cx.widened(h.ty);
        let ty = &self.cx.ty;
        if t == ty.str_ {
            let h = self.coerce(h, ty.str_);
            let kind = H::Call {
                callee: Callee::Def(d, vec![]),
                args: vec![h],
            };
            return self.mk(kind, f64, span);
        }
        if t == ty.bool_ {
            let one = self.mk(H::Lit(hir::Lit::Float(1.0)), f64, span);
            let zero = self.mk(H::Lit(hir::Lit::Float(0.0)), f64, span);
            let h = self.coerce(h, self.cx.ty.bool_);
            let kind = H::If {
                cond: Box::new(h),
                then: Box::new(one),
                els: Box::new(zero),
            };
            return self.mk(kind, f64, span);
        }
        if ty.is_numeric(t) {
            return match self.try_coerce(h, f64) {
                Ok(h) => h,
                Err(h) => self.mk(H::Cast(Box::new(h)), f64, span),
            };
        }
        if !ty.is_bottom(t) && t != ty.error {
            let tn = self.cx.display(h.ty);
            self.cx.err(
                format!("`Number(x)` converts a string, a number or a boolean, not a `{tn}`"),
                h.span,
            );
        }
        self.error_expr(span)
    }

    /// `Boolean(x)`: whether `x` is truthy (`Boolean()` is `false`).
    pub(super) fn boolean_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let b = self.cx.ty.bool_;
        if !self.conversion_arity("Boolean", args, span) {
            return self.error_expr(span);
        }
        let Some(a) = args.first().filter(|a| !is_null_lit(a)) else {
            return self.mk(H::Lit(hir::Lit::Bool(false)), b, span);
        };
        let h = self.expr(a, None, Want::Borrow);
        self.truthy(h)
    }

    /// At most one argument, as TypeScript requires (the others are reported and checked).
    fn conversion_arity(&mut self, name: &str, args: &[ast::Expr], span: Span) -> bool {
        if args.len() <= 1 {
            return true;
        }
        self.arg_count_error(&format!("function `{name}`"), 0, 1, args.len(), span);
        self.check_args_loose(args);
        false
    }
}

fn is_null_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => true,
        ast::ExprKind::Paren(x) => is_null_lit(x),
        _ => false,
    }
}
