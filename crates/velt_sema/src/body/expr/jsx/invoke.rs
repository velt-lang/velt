//! Calls built from arguments the JSX lowering already checked (runtime functions, components
//! called in place): type arguments are inferred from the argument types and each argument is
//! converted to its parameter type, like an ordinary call (`args::check_call` does the same for
//! source arguments).

use velt_common::{Diagnostic, Span};

use super::provider::Provider;
use crate::body::FnCx;
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId};

/// Reports an argument its parameter does not accept: (parameter name, its type, the argument).
pub(super) type OnMismatch<'f, 'a, 'm> = dyn FnMut(&mut FnCx<'a, 'm>, &str, TyId, &hir::Expr) + 'f;

impl<'a, 'm> FnCx<'a, 'm> {
    /// `name(args)` for runtime function `d` of provider `p`.
    pub(super) fn jsx_call(
        &mut self,
        p: &Provider,
        d: DefId,
        name: &str,
        args: Vec<hir::Expr>,
        span: Span,
    ) -> hir::Expr {
        if self.cx.fn_info(d).params.len() != args.len() {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "`{name}` of the JSX provider '{}' must take {} parameters",
                        p.source,
                        args.len()
                    ),
                    span,
                )
                .with_note(CONTRACT_NOTE),
            );
            return self.error_expr(span);
        }
        let source = p.source.clone();
        let mut report = |s: &mut Self, param: &str, expected: TyId, found: &hir::Expr| {
            let (e, f) = (s.cx.display(expected), s.cx.display(found.ty));
            s.cx.error(
                Diagnostic::error(
                    format!(
                        "`{name}` of the JSX provider '{source}' does not accept this: parameter `{param}` has type `{e}`, found `{f}`"
                    ),
                    found.span,
                )
                .with_note(CONTRACT_NOTE),
            );
        };
        self.call_checked(d, args, span, &mut report)
    }

    /// `d(args)` with `args` already checked (as many as `d` has parameters).
    pub(super) fn call_checked(
        &mut self,
        d: DefId,
        args: Vec<hir::Expr>,
        span: Span,
        on_mismatch: &mut OnMismatch<'_, 'a, 'm>,
    ) -> hir::Expr {
        let name = self.cx.fn_info(d).name.clone();
        let c = self.fn_callable(d, format!("`{name}`"), span);
        let mut slots = vec![None; c.slot_names.len()];
        for (h, param) in args.iter().zip(&c.params) {
            self.cx.match_ty(param.ty, h.ty, &mut slots);
        }
        let type_args = self.solve_slots(&c, &slots, false, span);
        let mut hargs = vec![];
        for (h, param) in args.into_iter().zip(&c.params) {
            let target = self.cx.subst(param.ty, &type_args);
            let mut h = match self.try_coerce(h, target) {
                Ok(h) => h,
                Err(h) => {
                    on_mismatch(self, &param.name, target, &h);
                    h
                }
            };
            if param.mode == PassMode::BorrowMut {
                self.use_mutably(&mut h, "modify");
            }
            hargs.push(h);
        }
        self.note_async_args(d, &hargs);
        let ret = self.cx.subst(c.ret, &type_args);
        self.call_throws(d, &type_args, ret, span);
        let kind = H::Call {
            callee: Callee::Def(d, type_args),
            args: hargs,
        };
        self.mk(kind, ret, span)
    }
}

const CONTRACT_NOTE: &str =
    "JSX runtimes implement the signatures in docs/internals/contracts/jsx.md";
