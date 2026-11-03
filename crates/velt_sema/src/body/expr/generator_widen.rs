//! A generator that throws nothing used where a generator that throws is expected (#2085):
//! `Generator<T>` as `Generator<T, E>`, `AsyncGenerator<T>` as `AsyncGenerator<T, E>`, as
//! TypeScript allows. A generator's results are laid out by its error type, so the value can't
//! just be retyped: it is passed through a generator of the wider type that yields what it
//! yields (`generatorWiden` / `asyncGeneratorWiden` in the prelude), as a promise is widened at
//! run time (`Intrinsic::PromiseWiden`). Closing or dropping the outer generator drops the inner
//! one, which closes it. A generator that throws something already keeps its error type (a
//! wider one would need its errors converted on the way, which these helpers can't type).

use crate::body::FnCx;
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` (a generator that throws nothing) as generator type `exp` of the same class and
    /// values that throws something; `Err(h)` for any other conversion.
    pub(super) fn widen_generator(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((f, args)) = self.generator_widener(h.ty, exp) else {
            return Err(h);
        };
        let span = h.span;
        let call = H::Call {
            callee: hir::Callee::Def(f, args),
            args: vec![h],
        };
        Ok(self.mk(call, exp, span))
    }

    /// Does a generator of type `from` widen to generator type `to` ([`Self::widen_generator`])?
    pub(super) fn generator_widens(&mut self, from: TyId, to: TyId) -> bool {
        self.generator_widener(from, to).is_some()
    }

    /// The prelude function that widens a generator of type `from` to `to`, and its type
    /// arguments (`T` and the wider error type).
    fn generator_widener(&mut self, from: TyId, to: TyId) -> Option<(DefId, Vec<TyId>)> {
        let (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) = (self.cx.ty.kind(from), self.cx.ty.kind(to))
        else {
            return None;
        };
        let never = self.cx.ty.never;
        // `[T, E]` or `[T, E, N]`: the same values, no `next(value)`, and `never` widened.
        let no_next = (xs.iter().skip(2))
            .chain(ys.iter().skip(2))
            .all(|t| *t == never);
        let widens = no_next
            && d == e
            && self.cx.is_generator_class(*d)
            && xs.len() >= 2
            && xs.len() == ys.len()
            && xs[0] == ys[0]
            && xs[1] == never
            && ys[1] != never
            && !self.cx.ty.has_error(ys[1]);
        if !widens {
            return None;
        }
        let args = vec![xs[0], ys[1]];
        let name = match Some(*d) == self.cx.async_generator_class() {
            true => "asyncGeneratorWiden",
            false => "generatorWiden",
        };
        self.cx.prelude_fn(name).map(|f| (f, args))
    }
}
