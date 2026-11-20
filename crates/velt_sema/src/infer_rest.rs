//! A function type whose rest parameter is a type parameter, matched by one with fixed
//! parameters (`match_ty`, #379): `(rq: Ctx, ...args: A) => R` matched by
//! `(rq: Ctx, a: number, b: string) => R` binds `A` to the tuple of the parameters at the rest
//! position, `[number, string]` (`body::expr::rest_tuple` binds it from an arrow's written
//! types before the arrow is checked).

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// Does pattern `pat` (a function type with a rest parameter) match function type `actual`
    /// (fixed parameters) this way: its rest is a type parameter, and `actual` has at least
    /// the parameters before it?
    pub(crate) fn rest_tuple_matches(&self, pat: TyId, actual: TyId) -> bool {
        let (
            TyKind::FnPtr {
                params: pp,
                rest: true,
                ..
            },
            TyKind::FnPtr {
                params: ap,
                rest: false,
                ..
            },
        ) = (self.ty.kind(pat), self.ty.kind(actual))
        else {
            return false;
        };
        matches!(pp.last().map(|&t| self.ty.kind(t)), Some(TyKind::Param(_)))
            && ap.len() + 1 >= pp.len()
    }

    /// Matches `pat` against `actual` where [`Self::rest_tuple_matches`] says so.
    pub(crate) fn match_rest_tuple(
        &mut self,
        pat: TyId,
        actual: TyId,
        slots: &mut [Option<TyId>],
    ) -> bool {
        let (
            TyKind::FnPtr {
                params: pp,
                ret: pr,
                throws: pt,
                ..
            },
            TyKind::FnPtr {
                params: ap,
                ret: ar,
                throws: at,
                ..
            },
        ) = (self.ty.kind(pat).clone(), self.ty.kind(actual).clone())
        else {
            return false;
        };
        let n = pp.len() - 1;
        let rest = self.ty.intern(TyKind::Tuple(ap[n..].to_vec()));
        let x = self.match_all(&pp[..n], &ap[..n], slots) && self.match_ty(pp[n], rest, slots);
        let y = self.match_error(pt, at, slots);
        self.match_ty(pr, ar, slots) && x && y
    }
}
