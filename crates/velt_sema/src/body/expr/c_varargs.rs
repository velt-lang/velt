//! Calls of variadic C functions (`declare c("c") function fcntl(fd: i32, cmd: i32, ...args:
//! CVarArgs): i32;`, docs/internals/design/c-ffi.md "Variadic functions"): the fixed arguments
//! are checked as any call's, then each further one by itself. It passes as what it is, so its
//! type has to be one C's `...` takes: an integer, `f32`, `f64`, `bool`, `Ptr<T>`,
//! `Ptr<T> | null` or `null`. A number literal there has no type of its own (C would read the
//! wrong bits for a wrong guess): it says one, `0 as i32`. Lowering applies C's default
//! argument promotions (velt_vir lower/c_call.rs).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ops::untyped;
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H};

/// What C's `...` takes, for the errors.
const TAKES: &str = "a variadic argument is an integer (`i8` … `u64`, `isize`, `usize`), `f32`, `f64`, `bool`, `Ptr<T>`, `Ptr<T> | null` or `null`";

impl FnCx<'_, '_> {
    /// `name(args)` of the variadic C function `d`.
    pub(super) fn c_variadic_call(
        &mut self,
        d: DefId,
        name: &str,
        args: &[ast::Expr],
        exp: Option<hir::TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut c = self.fn_callable(d, format!("function `{name}`"), span);
        // The fixed parameters only: `...` is no parameter (`collect::c_sig`).
        c.rest = false;
        if args.len() < c.params.len() {
            let (n, got) = (c.params.len(), args.len());
            let plural = if n == 1 { "argument" } else { "arguments" };
            let were = if got == 1 { "was" } else { "were" };
            self.cx.err(
                format!("C function `{name}` takes at least {n} {plural} before its `...`, but {got} {were} supplied"),
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let (head, extra) = args.split_at(c.params.len());
        self.system_args = self.cx.fn_info(d).has_buffer_params();
        self.c_args = true;
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, head, exp, span);
        let mut all = ck.args;
        for a in extra {
            all.push(self.c_vararg(name, a));
        }
        let kind = H::Call {
            callee: Callee::Def(d, ck.type_args),
            args: all,
        };
        self.mk(kind, ck.ret, span)
    }

    /// Variadic argument `a` of C function `name`, checked.
    fn c_vararg(&mut self, name: &str, a: &ast::Expr) -> hir::Expr {
        if let ast::ExprKind::Spread(x) = &a.kind {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "C function `{name}` takes its variadic arguments one by one, not spread"
                    ),
                    a.span,
                )
                .with_note("C's `...` has no array: write each argument"),
            );
            self.expr(x, None, Want::Borrow);
            return self.error_expr(a.span);
        }
        if untyped(a) {
            self.cx.error(
                Diagnostic::error(
                    format!("a number literal passed to C function `{name}`'s `...` needs its C type"),
                    a.span,
                )
                .with_note("C reads a variadic argument as the type the format or command says, so say which: `0 as i32`, `1.5 as f64`"),
            );
            self.expr(a, None, Want::Borrow);
            return self.error_expr(a.span);
        }
        // `null` is `NULL`: a null of no other type.
        let exp = matches!(
            super::method_value::strip_parens(a).kind,
            ast::ExprKind::Lit(ast::Lit::Null)
        )
        .then(|| self.cx.ty.option_of(self.cx.ty.never, hir::Nothing::Null));
        let h = self.expr(a, exp, Want::Borrow);
        if h.ty == self.cx.ty.error
            || crate::collect::c_sig::value_ty(self.cx, h.ty).is_some()
            || crate::collect::c_sig::is_null(self.cx, h.ty)
        {
            return h;
        }
        let shown = self.cx.display(h.ty);
        self.cx.error(
            Diagnostic::error(
                format!("`{shown}` can't be passed to C function `{name}`'s `...`"),
                a.span,
            )
            .with_note(TAKES),
        );
        self.error_expr(a.span)
    }
}
