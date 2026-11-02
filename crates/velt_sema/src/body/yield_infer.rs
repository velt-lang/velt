//! The yield type of a generator function expression written without a result type and not
//! expected as a generator function type (`(async function* () { yield f(); })()`,
//! https://veltlang.com/docs/reference/functions/ "Generator function expressions"): as in
//! TypeScript, the type of the values its `yield`s give, joined as the values of an arrow's
//! `return`s are (`returns::common_type`), each converted to it once the body is checked. A
//! body without a `yield` value yields `void`. A `yield*` needs a written result type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId, TyKind};
use crate::visit::{self, VisitMut};

impl FnCx<'_, '_> {
    /// `yield arg` in a generator whose yield type is being inferred: the value as it is
    /// (an async generator's promise awaited), its type recorded.
    pub(super) fn yield_inferred(&mut self, arg: Option<&ast::Expr>, span: Span) -> hir::Expr {
        let v = match arg {
            Some(a) => {
                let h = self.expr(a, None, Want::Move);
                let h = match self.cx.ty.kind(h.ty).clone() {
                    TyKind::Promise(v, _) if self.f.is_async => {
                        self.awaited_using_shares(&h);
                        self.await_throws(&h);
                        let at = h.span;
                        self.mk(H::Await(Box::new(h)), v, at)
                    }
                    _ => h,
                };
                self.widen_value(h)
            }
            None => self.unit_expr(span),
        };
        if let Some(tys) = self.f.infer_yields.as_mut() {
            tys.push((v.ty, span));
        }
        let kind = H::Call {
            callee: Callee::Intrinsic(Intrinsic::Yield),
            args: vec![v],
        };
        self.mk(kind, self.cx.ty.unit, span)
    }

    /// `yield* arg` in a generator whose yield type is being inferred: reported.
    pub(super) fn yield_star_inferred(&mut self, span: Span) -> hir::Expr {
        self.cx.error(
            Diagnostic::error(
                "`yield*` in a generator function expression needs its result type written",
                span,
            )
            .with_note("TypeScript infers it; Velt infers the yield type only from `yield` values; write the result type (`function* (): Generator<T> { ... }`, `async function* (): AsyncGenerator<T> { ... }`)"),
        );
        self.error_expr(span)
    }

    /// The yield type of the body `block` whose yields were recorded (module docs); converts
    /// every yielded value to it.
    pub(crate) fn finish_inferred_yields(&mut self, block: &mut hir::Block, span: Span) -> TyId {
        let recorded = self.f.infer_yields.take().unwrap_or_default();
        let (never, error) = (self.cx.ty.never, self.cx.ty.error);
        let mut tys: Vec<TyId> = vec![];
        for (t, _) in &recorded {
            if *t != never && !tys.contains(t) {
                tys.push(*t);
            }
        }
        if tys.contains(&error) {
            return error;
        }
        let at = recorded.first().map_or(span, |(_, s)| *s);
        let t = match tys.as_slice() {
            [] => self.cx.ty.unit,
            [t] => *t,
            _ => self.common_type(&tys, at),
        };
        if recorded.iter().any(|(y, _)| *y != t) {
            visit::block(block, &mut CoerceYields { fcx: self, to: t });
        }
        t
    }
}

/// Converts the value of every `yield` of a body to the inferred yield type.
struct CoerceYields<'f, 'a, 'm> {
    fcx: &'f mut FnCx<'a, 'm>,
    to: TyId,
}

impl VisitMut for CoerceYields<'_, '_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        let H::Call {
            callee: Callee::Intrinsic(Intrinsic::Yield),
            args,
        } = &mut e.kind
        else {
            return;
        };
        let Some(v) = args.first_mut() else {
            return;
        };
        if v.ty != self.to {
            let placeholder = self.fcx.mk(H::Lit(hir::Lit::Unit), self.to, v.span);
            let h = std::mem::replace(v, placeholder);
            *v = self.fcx.coerce(h, self.to);
        }
    }
}
