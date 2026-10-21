//! Function types (`(a: A, ...xs: T[]) => R throws E`): their parameters, rest parameter,
//! result and error type.

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// `(params) => ret [throws E]`, with `rest`: `(a: A, ...xs: T[]) => R`, whose last parameter
    /// must have an array or tuple type. After a `Promise` result, `throws E` is the promise's rejection
    /// type (calling an async function never throws; awaiting its promise does).
    pub(crate) fn resolve_fn_type(
        &mut self,
        params: &[ast::TypeExpr],
        rest: bool,
        ret: &ast::TypeExpr,
        throws: Option<&ast::TypeExpr>,
        env: &TyEnv,
    ) -> TyId {
        let tys: Vec<TyId> = params.iter().map(|t| self.resolve_type(t, env)).collect();
        if let (true, Some(&last), Some(written)) = (rest, tys.last(), params.last()) {
            // A tuple is its elements (`Types::intern`); a type parameter or `infer` slot is
            // one once it is known.
            let later = matches!(
                self.ty.kind(last),
                TyKind::Tuple(_) | TyKind::Param(_) | TyKind::Op(..)
            );
            if !later && !crate::collect::sigs::rest_param_type(self, written.span, last) {
                return self.ty.error;
            }
        }
        let mut ret = self.resolve_type(ret, env);
        let never = self.ty.never;
        let err = throws.map(|t| {
            let e = self.resolve_type(t, env);
            self.no_void_error(e, t.span);
            e
        });
        let err = self.canon_error(err).unwrap_or(never);
        let throws = match self.ty.kind(ret).clone() {
            TyKind::Promise(v, e) if err != never => {
                let e = self.join_errors(Some(e), Some(err)).unwrap_or(never);
                ret = self.ty.promise_rejecting(v, e);
                never
            }
            _ => err,
        };
        self.ty.intern(TyKind::FnPtr {
            params: tys,
            ret,
            throws,
            rest,
        })
    }
}

/// The function type forms Velt parses but doesn't support yet (#943): its error and a note.
pub(crate) fn unsupported_fn_type(kind: &ast::TypeExprKind) -> (&'static str, &'static str) {
    match kind {
        ast::TypeExprKind::Construct(_) => (
            "construct signatures (`new (...) => T`) are not supported yet",
            "a class can't be passed as a value yet; pass a function that constructs it (`(buf: ArrayBuffer) => new Uint8Array(buf)`)",
        ),
        _ => (
            "generic function types (`<T>(x: T) => R`) are not supported yet",
            "a function value has one type; declare a generic `function` and call it, or give the type parameters values (`(x: string) => R`)",
        ),
    }
}
