//! `unsafe` code (docs/internals/design/c-ffi.md): `unsafe { … }` blocks and expressions open
//! an unsafe context, as does the body of an unsafe function, and calling an unsafe function
//! needs one. Unsafe contexts are lexical: a closure written inside one is unsafe code too (it
//! is checked by the same `FnCx`), a nested function declaration is not (it has its own).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::ctx::Ctx;
use crate::hir::{self, DefId, StmtKind as S, TyId};

impl FnCx<'_, '_> {
    /// A block statement: `{ … }`, or `unsafe { … }` (`is_unsafe`), checked in an unsafe
    /// context.
    pub(super) fn block_stmt(
        &mut self,
        b: &ast::Block,
        is_unsafe: bool,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let depth = self.unsafe_depth;
        self.unsafe_depth += u32::from(is_unsafe);
        let b = self.block(b);
        self.unsafe_depth = depth;
        Self::push(out, S::Block(b), span);
    }

    /// `unsafe { expr }`: `expr`, checked in an unsafe context.
    pub(super) fn unsafe_expr(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
    ) -> hir::Expr {
        self.unsafe_depth += 1;
        let e = self.expr(inner, exp, want);
        self.unsafe_depth -= 1;
        e
    }

    /// Is the code being checked unsafe code: in an `unsafe` block or expression, or in the
    /// body of an unsafe function (or a closure written in one)?
    fn in_unsafe_code(&self) -> bool {
        self.unsafe_depth > 0
            || self.body_def.is_some_and(|f| {
                unsafe_fn(self.cx, f)
                    || (self.cx.unsafe_arrow_fns).contains(&self.cx.fn_info(f).name_span)
            })
    }

    /// A decorator named as a value or called: it is data, which only `@` may name.
    pub(crate) fn reject_decorator_value(&mut self, name: &str, span: Span) {
        self.cx.error(
            Diagnostic::error(format!("`{name}` is a decorator: it can only be used as `@{name}`"), span)
                .with_note("a `declare decorator function` is data read at compile time: nothing runs, so there is nothing to call"),
        );
    }

    /// A call of `d` at `span`: an unsafe function needs an unsafe context.
    pub(crate) fn check_unsafe_call(&mut self, d: DefId, what: &str, span: Span) {
        if crate::decorators::decl_of(self.cx.fn_info(d)).is_some() {
            let name = self.cx.fn_info(d).name.clone();
            self.reject_decorator_value(&name, span);
            return;
        }
        if self.in_unsafe_code() || !unsafe_fn(self.cx, d) {
            return;
        }
        let what = match what.is_empty() {
            true => "this function".to_string(),
            false => what.to_string(),
        };
        self.cx.error(
            Diagnostic::error(
                format!("calling unsafe {what} needs an `unsafe` block or function"),
                span,
            )
            .with_note(
                "wrap the call in `unsafe { … }`, with a `// SAFETY:` comment before it saying why the call is sound",
            ),
        );
    }

    /// Unsafe function `d` used as a value (not called): an error, since a function type
    /// can't say it is unsafe, so a call through the value would not need an unsafe context.
    /// True when it reported one.
    pub(crate) fn reject_unsafe_value(&mut self, d: DefId, name: &str, span: Span) -> bool {
        if !unsafe_fn(self.cx, d) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                format!("unsafe function `{name}` can only be called, not used as a value"),
                span,
            )
            .with_note("pass a safe function that calls it in an `unsafe { … }` block instead"),
        );
        true
    }
}

/// Is `d` an unsafe function or method (`unsafe function`, `unsafe m()`, a C function, `declare
/// unsafe intrinsic function`)?
pub(crate) fn unsafe_fn(cx: &Ctx, d: DefId) -> bool {
    let f = cx.fn_info(d);
    f.c.is_some()
        || f.unsafe_intrinsic
        || f.source
            .is_some_and(|src| crate::body::defaults::fn_sig_ast(src).is_unsafe)
}
