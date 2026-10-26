//! Conversions of values whose types hold error-set variables (`throws::vars`): converted as
//! the types are written, assuming the variables empty; when one is not, the program is checked
//! again with the solved sets.

use crate::body::FnCx;
use crate::hir::{self, TyId};

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
        if !self.cx.fn_sites.use_vars
            || !(self.cx.mentions_err_var(h.ty) || self.cx.mentions_err_var(exp))
        {
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
                c.ty = exp;
                Ok(c)
            }
            Err(mut p) => {
                p.ty = ty;
                Err(p)
            }
        }
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
