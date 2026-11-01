//! Compile-time type tests (std/prelude/type_tests.vlt, docs/reference/functions.md "Type
//! tests"): `typeIs<A, B>()` is a call of the prelude function, which lowering replaces by a
//! constant per instantiation (velt_vir `lower/type_tests.rs`). Inside the `then` branch of
//! `if (typeIs<A, B>())` (`Frame::type_guards`), `asType<B>(x)` with `x: A` converts between the
//! two, which that instantiation makes the same type; anywhere else it is an error, as is any
//! other form of the call (`B` not written, or `A` written: `x` is what says it). A closure in the
//! branch is lowered in the same instantiation and has its guards. Neither function can be used
//! as a value: its body is not what a call means.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, CpuTarget, DefId, ExprKind as H, TyId};

/// The guards an `if`'s `then` branch entered (`enter_guards`), for `exit_guards`.
pub(super) struct Guards {
    cpu: Option<CpuTarget>,
    ty: bool,
}

impl FnCx<'_, '_> {
    /// Enters the guards the `then` branch of `if (c)` runs under: a CPU check
    /// (`body::gated`) and a `typeIs<A, B>()` test.
    pub(super) fn enter_guards(&mut self, c: &hir::Expr) -> Guards {
        let cpu = self.cpu_guard(c);
        if let Some(g) = cpu {
            self.f.cpu_guards.push(g);
        }
        let ty = self.type_guard(c);
        if let Some(g) = ty {
            self.f.type_guards.push(g);
        }
        Guards {
            cpu,
            ty: ty.is_some(),
        }
    }

    pub(super) fn exit_guards(&mut self, g: Guards) {
        if g.cpu.is_some() {
            self.f.cpu_guards.pop();
        }
        if g.ty {
            self.f.type_guards.pop();
        }
    }

    /// `[A, B]` when `c` is the prelude's `typeIs<A, B>()`.
    fn type_guard(&self, c: &hir::Expr) -> Option<(TyId, TyId)> {
        let hir::ExprKind::Call {
            callee: hir::Callee::Def(d, targs),
            args,
        } = &c.kind
        else {
            return None;
        };
        match targs.as_slice() {
            [a, b] if args.is_empty() && Some(*d) == self.cx.prelude_fn("typeIs") => Some((*a, *b)),
            _ => None,
        }
    }

    /// `asType<B>(x)`: the prelude's `asType<B, A>` with `A` the type of `x`, inside
    /// `if (typeIs<A, B>())` (either order); `None` for a call of another function. Every call of
    /// it comes here: one of another form is an error.
    pub(crate) fn as_type_call(
        &mut self,
        d: DefId,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        if Some(d) != self.cx.prelude_fn("asType") {
            return None;
        }
        let ([target], [arg]) = (type_args, args) else {
            self.cx.error(
                Diagnostic::error("`asType` takes one type argument and one argument", span)
                    .with_note(
                    "write the type to convert to, `asType<B>(x)`: the type of `x` is the other",
                ),
            );
            for a in args {
                self.expr(a, None, Want::Borrow);
            }
            return Some(self.error_expr(span));
        };
        let b = self.resolve(target);
        // Borrowed: the converted value is the same one (a consumer that keeps it counts it).
        let x = self.expr(arg, None, Want::Borrow);
        let a = x.ty;
        self.check_as_type(a, b, span);
        let kind = H::Call {
            callee: hir::Callee::Def(d, vec![b, a]),
            args: vec![x],
        };
        Some(self.mk(kind, b, span))
    }

    /// `asType<B>` of an `A` must sit inside `if (typeIs<A, B>())` (either order).
    fn check_as_type(&mut self, a: TyId, b: TyId, span: Span) {
        if a == b
            || self
                .f
                .type_guards
                .iter()
                .any(|&g| g == (a, b) || g == (b, a))
        {
            return;
        }
        let (an, bn) = (self.cx.display(a), self.cx.display(b));
        self.cx.error(
            Diagnostic::error(
                format!("`asType<{bn}>` of a `{an}` needs an enclosing `if (typeIs<{an}, {bn}>())`"),
                span,
            )
            .with_note("the compiler converts only where it knows the two types are the same instantiation"),
        );
    }

    /// Function `d` (named `name`) used as a value: an error for `typeIs` and `asType`, whose
    /// calls the compiler decides. Whether it is one.
    pub(super) fn type_test_value(&mut self, d: DefId, name: &str, span: Span) -> bool {
        let known = [self.cx.prelude_fn("typeIs"), self.cx.prelude_fn("asType")];
        if !known.contains(&Some(d)) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(format!("`{name}` can only be called directly"), span)
                .with_note("the compiler decides each call where the generic is used; a function value would not be"),
        );
        true
    }
}
