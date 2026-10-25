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
}
