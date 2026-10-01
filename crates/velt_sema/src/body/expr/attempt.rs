//! `attempt(f)`: errors as values. Calls `f: () => T throws E` and returns its result or its
//! error: `T | E` (the union, with `E`'s members flattened in), or `E | null` when `T` is
//! `void`. Encoded as `Intrinsic::Attempt`; a function that cannot throw is just called.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(crate) fn attempt_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [arg] = args else {
            self.arg_count_error("`attempt`", 1, 1, args.len(), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let unknown = self.cx.ty.error;
        let expected = self.cx.ty.intern(TyKind::FnPtr {
            params: vec![],
            ret: unknown,
            throws: unknown,
        });
        // A closure literal is only borrowed by the call: it stays non-escaping.
        let mut f = match super::args::as_arrow(arg) {
            Some(a) => self.closure(a, Some(expected), false),
            None => self.expr(arg, Some(expected), Want::Borrow),
        };
        let Some((ret, throws)) = self.attempt_target(&f) else {
            return self.error_expr(span);
        };
        crate::body::places::set_place_mode(&mut f, hir::UseMode::Borrow);
        if throws == self.cx.ty.never {
            let kind = H::Call {
                callee: hir::Callee::Indirect(Box::new(f)),
                args: vec![],
            };
            return self.mk(kind, ret, span);
        }
        let ty = if ret == self.cx.ty.unit {
            self.cx.union_of(&[throws], true, span)
        } else {
            self.cx.union_of(&[ret, throws], false, span)
        };
        self.intrinsic(hir::Intrinsic::Attempt, vec![f], ty, span)
    }

    /// Result and error type of the function `attempt` calls (a synchronous `() => T`).
    fn attempt_target(&mut self, f: &hir::Expr) -> Option<(TyId, TyId)> {
        match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } if params.is_empty() && self.cx.ty.promise_payload(ret).is_none() => {
                Some((ret, throws))
            }
            TyKind::Error => None,
            _ => {
                let found = self.cx.display(f.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("`attempt` needs a function without parameters, found `{found}`"),
                        f.span,
                    )
                    .with_note("write `attempt(() => f(x))`; for async code, use `try { await ... } catch (e) { ... }`"),
                );
                None
            }
        }
    }
}
