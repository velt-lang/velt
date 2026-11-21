//! A spread of a value whose type is known only per instantiation into a call's rest parameter
//! of `unknown` or `any` values (#943): `g(...args)` with `args: A`, `A extends unknown[]`, or
//! a deferred type such as `[S] extends [void] ? [] : [S]`. TypeScript accepts it; the value is
//! a tuple at one instantiation and an array at another, so sema can't name its elements. It is
//! `Intrinsic::SpreadParam`, an array of the rest's element type that lowering builds per
//! instantiation (hir_encodings.md "Spreads and conversions in generic bodies"). Only in a
//! call's rest parameter (`pack_rest` marks the array it builds): an array literal or fixed
//! parameters stay refused, as their length would be known only per instantiation.

use velt_syntax::ast::{self, TypeExprKind as K};

use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir::{self, Intrinsic, TyId, TyKind, TyOp};

impl FnCx<'_, '_> {
    /// `h` (a spread source in the array at `at`, whose elements are `elem`) as an array of
    /// `elem` built per instantiation, when the array is a call's packed rest arguments, `elem`
    /// is `unknown` or `any` and `h`'s type is a type parameter bounded by an array type or a
    /// deferred type. `Err(h)` otherwise.
    pub(super) fn spread_param(
        &mut self,
        h: hir::Expr,
        at: velt_common::Span,
        elem: Option<TyId>,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(elem) = elem else {
            return Err(h);
        };
        // `object` and `Function` rests take no primitive, which an element may be.
        let open = matches!(self.cx.dynamic(elem), Some(Dynamic::Any | Dynamic::Unknown));
        if !self.rest_packs.contains(&at) || !open {
            return Err(h);
        }
        let generic = match self.cx.ty.kind(h.ty) {
            TyKind::Param(n) => {
                let n = *n;
                self.cx
                    .constraint_of(n)
                    .is_some_and(|b| self.cx.ty.array_elem(b).is_some() || self.tuple_bound(b))
            }
            TyKind::Op(..) => self.listy_op(h.ty),
            _ => false,
        };
        if !generic {
            return Err(h);
        }
        let span = h.span;
        let ty = self.cx.ty.array(elem);
        Ok(self.intrinsic(Intrinsic::SpreadParam, vec![h], ty, span))
    }

    /// Is `b` a tuple type (a type parameter bounded by one, `A extends [number, string]`)?
    fn tuple_bound(&self, b: TyId) -> bool {
        matches!(self.cx.ty.kind(b), TyKind::Tuple(_))
    }

    /// Is the deferred type `t` a conditional type whose branches are all tuple or array types
    /// (`[S] extends [void] ? [] : [S]`, in an alias or written in the signature), so a value
    /// of it is a tuple or an array at every instantiation? Other deferred types (`keyof T`, an
    /// indexed access, a conditional with another branch) stay refused, as tsc refuses them
    /// (TS2488).
    fn listy_op(&self, t: TyId) -> bool {
        let body = match self.cx.ty.kind(t) {
            TyKind::Op(TyOp::Alias(a), _) => &self.cx.aliases[*a as usize].decl.ty,
            TyKind::Op(TyOp::Inline(i), _) => self.cx.type_level.inline.form_ty(*i),
            _ => return false,
        };
        matches!(&body.kind, K::Conditional(_)) && listy(body)
    }
}

/// Is every branch of `e` a tuple or array type (a conditional's, recursively)?
fn listy(e: &ast::TypeExpr) -> bool {
    match &e.kind {
        K::Tuple(_) | K::Array(_) => true,
        K::Conditional(c) => listy(&c.then) && listy(&c.else_),
        _ => false,
    }
}
