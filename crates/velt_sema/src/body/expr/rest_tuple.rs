//! A rest parameter typed by a type parameter, inferred from a callback that lists its
//! parameters one by one, as TypeScript infers it: `wrap((rq: Ctx, a: number, b: string) => …)`
//! for `wrap<A extends unknown[]>(f: (rq: Ctx, ...args: A) => R)` gives `A = [number, string]`.
//! A function type whose rest parameter is a tuple is those parameters (`Types::intern`), so the
//! callback is checked as `(rq: Ctx, a: number, b: string) => R`.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is `t` a function type whose rest parameter is a type parameter (`(rq: Ctx, ...args: A)
    /// => R`)? What it takes is known per instance only (`[number, number]` from a callback), so
    /// in a generic body it can be passed on as it is, not called or converted.
    pub(super) fn rest_param_fn(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::FnPtr {
                params, rest: true, ..
            } => params
                .last()
                .is_some_and(|&p| matches!(self.cx.ty.kind(p), TyKind::Param(_))),
            _ => false,
        }
    }

    /// Is `from` (such a function type) converted to another type by going where `to` is
    /// expected? Not to the same function type but for its error sites (a field of the same
    /// written type, `this.f = f`): the rules for function values' errors link those.
    pub(super) fn converts_rest_param_fn(&mut self, from: TyId, to: TyId) -> bool {
        from != to && self.cx.ty.without_error_types(from) != self.cx.ty.without_error_types(to)
    }

    /// Reports converting such a function value (`f as unknown as (…) => …`, `const g: any = f`,
    /// a field of another function type) at `span`.
    pub(super) fn rest_param_fn_converted(&mut self, span: velt_common::Span) {
        self.cx.error(
            velt_common::Diagnostic::error(
                "a function value whose rest parameter is typed by a type parameter (`...args: A`) can't be converted to another type yet (#943)",
                span,
            )
            .with_note("what it takes is known per call only (`A` from the callback); pass it on as it is, or take the callback as `(rq, ...args: unknown[]) => R`"),
        );
    }

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
                if ap.len() >= fixed && !ap.iter().any(|&t| self.cx.ty.has_error(t)) {
                    known[k] = Some(self.cx.ty.intern(TyKind::Tuple(ap[fixed..].to_vec())));
                }
            }
            return;
        };
        if !type_params.is_empty() || params.len() < fixed {
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
