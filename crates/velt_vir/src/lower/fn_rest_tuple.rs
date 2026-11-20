//! A function type whose rest parameter is a tuple is those parameters, as sema's
//! `Types::intern` makes it: an instance at `A = [f64, f64]` of `(rq: Ctx, ...args: A) => R` is
//! `(Ctx, f64, f64) => R`, the function a callback of that type is. Substitution in lowering
//! builds the instance's type with the tuple still in the rest position; [`Cx::intern`] makes
//! it the fixed parameters here, so every use of the type (a conversion to `any`, to another
//! function type, a call) sees the function's own parameters.

use velt_sema::hir::TyKind;

use super::Cx;

impl Cx<'_> {
    /// `k` with a tuple in a function type's rest position made its fixed parameters; any
    /// other kind as it is.
    pub(super) fn tuple_rest_fixed(&self, k: TyKind) -> TyKind {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest: true,
        } = &k
        else {
            return k;
        };
        let Some((last, fixed)) = params.split_last() else {
            return k;
        };
        let TyKind::Tuple(ts) = self.raw_kind(*last) else {
            return k;
        };
        let mut params = fixed.to_vec();
        params.extend(ts.iter().copied());
        TyKind::FnPtr {
            params,
            ret: *ret,
            throws: *throws,
            rest: false,
        }
    }
}
