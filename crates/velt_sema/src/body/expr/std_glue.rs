//! Std-only M4 intrinsics with extra rules: `__intrinsic_json_parse` throws the prelude's
//! `JsonError`, and `__intrinsic_http_handler` turns an async arrow literal
//! `async (raw: u64): Promise<u64> => ...` into the runtime's handler descriptor (see
//! `Intrinsic::HttpHandler` and std/http.vlt). What JSON glue can be generated for is checked
//! after all bodies (`crate::json`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::as_arrow;
use super::intrinsics::http_handler_fn;
use crate::body::{FnCx, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, Intrinsic, TyKind};

impl FnCx<'_, '_> {
    /// A decoding failure surfaces as a thrown `JsonError`.
    pub(super) fn json_parse_throws(&mut self, span: Span) {
        match self.cx.json_error_ty() {
            Some(t) => self.throw_src(ThrowSrc::Direct(t, span)),
            None => self.cx.err(
                "`JsonError` is missing from the prelude (std/prelude/json.vlt)",
                span,
            ),
        }
    }

    /// `__intrinsic_http_handler(async (raw: u64): Promise<u64> => ...)`.
    pub(super) fn http_handler(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let expected = http_handler_fn(&mut self.cx.ty);
        let u64_ = self.cx.ty.u64;
        let ret = self.cx.ty.intern(TyKind::Tuple(vec![u64_; 6]));
        let async_arrow = |a: &ast::Expr| {
            as_arrow(a)
                .is_some_and(|x| matches!(x.kind, ast::ExprKind::Arrow { is_async: true, .. }))
        };
        match args {
            [arg] if async_arrow(arg) => {
                let f = self.expr_coerce(arg, expected, Want::Move);
                self.intrinsic(Intrinsic::HttpHandler, vec![f], ret, span)
            }
            _ => {
                self.cx.error(
                    Diagnostic::error(
                        "`__intrinsic_http_handler` takes one async arrow literal",
                        span,
                    )
                    .with_note("write `__intrinsic_http_handler(async (raw: u64): Promise<u64> => { ... })`"),
                );
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }
}
