//! Conversions of values whose types hold error-set variables (`throws::vars`): converted as
//! the types are written, assuming the variables empty; when one is not, the program is checked
//! again with the solved sets.

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` converted to `exp` as their types are written without their error-set variables.
    /// `traps`: only when every variable in them is one of std/proxy's traps
    /// ([`crate::ctx::Ctx::only_trap_vars`]).
    pub(super) fn coerce_as_written(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        traps: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        // A check by units: std's unit keeps the variables its traps had in std's check, also
        // in the app's rounds without variables, where a check of the whole program has none.
        let vars = self.cx.fn_sites.use_vars || (traps && self.cx.units);
        if !vars || !(self.cx.mentions_err_var(h.ty) || self.cx.mentions_err_var(exp)) {
            return Err(h);
        }
        if traps && !(self.cx.only_trap_vars(h.ty) && self.cx.only_trap_vars(exp)) {
            return Err(h);
        }
        let (from, to) = (self.cx.plain_type(h.ty), self.cx.plain_type(exp));
        let ty = h.ty;
        let mut p = h;
        p.ty = from;
        match self.try_coerce_typed(p, to) {
            Ok(mut c) => {
                self.cx.assume_empty_in(ty);
                self.cx.assume_empty_in(exp);
                // Only while nothing is known to be stored into the variable: once something
                // is, the assumption fails and the program is checked again with the solved
                // sets, so the conversion made as before costs nothing more in this check.
                if c.ty == self.cx.ty.never && self.cx.var_known_empty(ty) {
                    return Ok(self.never_as_written(c));
                }
                c.ty = exp;
                Ok(c)
            }
            Err(mut p) => {
                p.ty = ty;
                Err(p)
            }
        }
    }

    /// `c`, a value typed by an error-set variable alone and assumed empty (`e as Error` of a
    /// `then` handler's reason when the promise never rejects, #3924): a `never` value, as in a
    /// check without variables, not one of the type it converts to, which would be a read of
    /// nothing stored as a value. Reading a local of it moves nothing: a check without
    /// variables passes a `never` parameter as `Copy`.
    fn never_as_written(&mut self, mut c: hir::Expr) -> hir::Expr {
        if let hir::ExprKind::Local(_, m @ hir::UseMode::Move) = &mut c.kind {
            *m = hir::UseMode::Copy;
        }
        c
    }

    /// A conversion from or to a union whose types hold only the traps' error-set variables
    /// (`E | <trap variables>`), made as the types are written: a conversion of the union
    /// itself would have the shape it loses once the variables are solved empty.
    pub(super) fn union_as_written(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.union_def(exp).is_none() && self.cx.union_def(h.ty).is_none() {
            return Err(h);
        }
        self.coerce_as_written(h, exp, true)
    }

    /// `h` as its type is written, when that type holds only the traps' error-set variables
    /// (assumed empty): a member of `E | <trap variables>` is read as the member of `E` it is
    /// once they are solved empty, and a value typed by a trap's variable alone as `never`
    /// until the program is checked again with the trap's set.
    pub(crate) fn without_trap_vars(&mut self, h: hir::Expr) -> hir::Expr {
        // Only where the variables are the value's own type or members of it (an error value),
        // not inside it (a function type's or a promise's error type, which keeps them).
        let own = self.cx.is_err_var(h.ty)
            || (self.cx.union_members(h.ty))
                .is_some_and(|ms| ms.iter().any(|m| self.cx.is_err_var(*m)));
        if !own || !self.cx.only_trap_vars(h.ty) {
            return h;
        }
        let plain = self.cx.plain_type(h.ty);
        self.coerce_as_written(h, plain, true).unwrap_or_else(|h| h)
    }

    /// In the app's stage of a check by units, `h` with std's trap variables in its type as this
    /// round's seeds give their sites, when its type is built of promises, arrays, options,
    /// tuples and unions over them (`rd(p)`'s `Promise<number>` rejecting with a trap's error):
    /// a whole-program check's later rounds have the traps' errors there. Not a function value
    /// or an object: std's own types (a handler's traps) keep the variables, so those would not
    /// match them.
    pub(crate) fn seeded_trap_value(&mut self, h: hir::Expr) -> hir::Expr {
        if self.cx.units_stage != crate::units::Stage::App
            || !self.cx.mentions_err_var(h.ty)
            || !self.cx.only_trap_vars(h.ty)
            || !self.plain_shape(h.ty, 0)
        {
            return h;
        }
        let seeded = self.cx.plain_seeded(h.ty);
        if seeded == self.cx.plain_type(h.ty) {
            return h;
        }
        self.coerce_as_written(h, seeded, true)
            .unwrap_or_else(|h| h)
    }

    /// Is `t` built of promises, arrays, options, tuples, unions and error-set variables
    /// ([`Self::seeded_trap_value`])?
    fn plain_shape(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        if self.cx.is_err_var(t) || !self.cx.mentions_err_var(t) {
            return true;
        }
        let parts: Vec<TyId> = match self.cx.ty.kind(t).clone() {
            TyKind::Promise(v, e) => vec![v, e],
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Tuple(es) => es,
            _ => match self.cx.union_members(t) {
                Some(ms) => ms,
                None => return false,
            },
        };
        parts.into_iter().all(|p| self.plain_shape(p, depth + 1))
    }

    /// `h` as its type is written when that type holds only the traps' error-set variables, at
    /// any depth: a value whose error types become values (the reasons `Promise.allSettled`
    /// reports), converted once where it is made.
    pub(crate) fn written_trap_vars(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.cx.only_trap_vars(h.ty) {
            return h;
        }
        let plain = self.cx.plain_type(h.ty);
        self.coerce_as_written(h, plain, true).unwrap_or_else(|h| h)
    }
}
