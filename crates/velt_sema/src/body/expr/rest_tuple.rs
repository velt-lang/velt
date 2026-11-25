//! A rest parameter typed by a type parameter, inferred from a callback that lists its
//! parameters one by one, as TypeScript infers it: `wrap((rq: Ctx, a: number, b: string) => …)`
//! for `wrap<A extends unknown[]>(f: (rq: Ctx, ...args: A) => R)` gives `A = [number, string]`.
//! A function type whose rest parameter is a tuple is those parameters (`Types::intern`), so the
//! callback is checked as `(rq: Ctx, a: number, b: string) => R`. One with fewer parameters
//! than those before the rest (`() => …`, a function declared so) gives `A = []` (#3296).

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Argument `arg` for parameter type `pat`: when `pat` is a function type (or one or
    /// nothing) whose rest parameter is the type parameter `k`, not known yet, and `arg` an arrow
    /// that types each of the parameters it lists there, `k` is the tuple of their types.
    pub(super) fn rest_tuple_slot(
        &mut self,
        arg: &ast::Expr,
        pat: TyId,
        known: &mut [Option<TyId>],
    ) {
        let core = self.cx.ty.opt_payload(pat).unwrap_or(pat);
        let TyKind::FnPtr {
            params: pp,
            rest: true,
            ..
        } = self.cx.ty.kind(core).clone()
        else {
            return;
        };
        let Some(&TyKind::Param(k)) = pp.last().map(|&t| self.cx.ty.kind(t)) else {
            return;
        };
        let k = k as usize;
        if !matches!(known.get(k), Some(None)) {
            return;
        }
        let mut e = arg;
        while let ast::ExprKind::Paren(inner) = &e.kind {
            e = inner;
        }
        let fixed = pp.len() - 1;
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ..
        } = &e.kind
        else {
            // A function value (`wrap(handler)`, a declared function): its own parameters at the
            // rest position, from its type (checked on its own, rolled back).
            let Some(t) = self.trial_type(arg) else {
                return;
            };
            if let TyKind::FnPtr {
                params: ap,
                rest: false,
                ..
            } = self.cx.ty.kind(t).clone()
            {
                // With fewer parameters than the fixed ones the rest is empty (#3296).
                if !ap.iter().any(|&t| self.cx.ty.has_error(t)) {
                    let rest = ap.get(fixed..).unwrap_or_default().to_vec();
                    known[k] = Some(self.cx.ty.intern(TyKind::Tuple(rest)));
                }
            }
            return;
        };
        if !type_params.is_empty() {
            return;
        }
        // Fewer parameters than the fixed ones (`() => …`): the rest is empty, as in TypeScript,
        // and the arrow is called with the arguments it takes (#3296).
        if params.len() < fixed {
            known[k] = Some(self.cx.ty.intern(TyKind::Tuple(vec![])));
            return;
        }
        // Each one plain: an optional or defaulted one, or the arrow's own rest parameter, has a
        // tuple form Velt has no type for (`[a?: number]`, `[number, ...T[]]`).
        let extra = &params[fixed..];
        if extra
            .iter()
            .any(|p| p.rest || p.optional || p.default.is_some())
        {
            return;
        }
        // An unwritten one is `unknown`, as TypeScript infers it (`(rq, a) => …` gives
        // `A = [unknown]`).
        let Some(unknown) = self.cx.any.map(|d| d.unknown) else {
            return;
        };
        let unknown = self.cx.ty.intern(TyKind::Dyn(unknown, vec![]));
        let before = self.cx.diags.len();
        let tys: Vec<TyId> = (extra.iter())
            .map(|p| p.ty.as_ref().map_or(unknown, |t| self.resolve(t)))
            .collect();
        // Reported when the arrow itself is checked.
        self.cx.diags.truncate(before);
        if tys.iter().any(|&t| self.cx.ty.has_error(t)) {
            return;
        }
        known[k] = Some(self.cx.ty.intern(TyKind::Tuple(tys)));
    }
}
