//! A function type whose rest parameter is a type parameter, matched by one with fixed
//! parameters (`match_ty`, #379): `(rq: Ctx, ...args: A) => R` matched by
//! `(rq: Ctx, a: number, b: string) => R` binds `A` to the tuple of the parameters at the rest
//! position, `[number, string]` (`body::expr::rest_tuple` binds it from an arrow's written
//! types before the arrow is checked). One with fewer parameters than those before the rest
//! (`() => R`, as any function value may have) binds `A` to `[]` (#3296).

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// Does pattern `pat` (a function type with a rest parameter) match function type `actual`
    /// (fixed parameters) this way: its rest is a type parameter?
    pub(crate) fn rest_tuple_matches(&self, pat: TyId, actual: TyId) -> bool {
        let (
            TyKind::FnPtr {
                params: pp,
                rest: true,
                ..
            },
            TyKind::FnPtr { rest: false, .. },
        ) = (self.ty.kind(pat), self.ty.kind(actual))
        else {
            return false;
        };
        matches!(pp.last().map(|&t| self.ty.kind(t)), Some(TyKind::Param(_)))
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
        // The parameters `actual` has before the rest position; with fewer, the rest is empty.
        let k = n.min(ap.len());
        let rest = self.ty.intern(TyKind::Tuple(ap[k..].to_vec()));
        // The fixed parameters only bind what they hold: `actual`'s may be wider (`rq?: Ctx` for
        // `rq: Ctx`), as a function value's parameter may be, and the value is converted to the
        // inferred type (and checked) afterwards.
        self.match_all(&pp[..k], &ap[..k], slots);
        let x = self.match_ty(pp[n], rest, slots);
        let y = self.match_error(pt, at, slots);
        self.match_ty(pr, ar, slots) && x && y
    }
}
