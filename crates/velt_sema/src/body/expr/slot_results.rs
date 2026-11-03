//! The generator an interface method's implementation in an object literal returns
//! (https://veltlang.com/docs/reference/async/ "Errors").
//!
//! A `Generator<T>` or `AsyncGenerator<T>` written without Velt's error type as an interface
//! method's result is a site (`throws::generator_sites`). A function an object literal gives
//! for the method (an arrow returning a generator, `chunks: (n) => all(n)`, or a function named
//! in `{ chunks }`) adds what its generator throws there, and the program is checked again, as
//! for a function type's result. A promise result needs nothing: the method's `throws` carries
//! what its promise rejects with (`collect::sigs::promise_throws`).

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, DefId, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Function value `h` written as `e` (`None`: a shorthand `{ f }`) for method `slot` of
    /// interface `iface`: what an arrow's `return` or a named function's result throws from its
    /// generator goes into the generator type written as the method's result.
    pub(super) fn bind_slot_result(
        &mut self,
        e: Option<&ast::Expr>,
        h: hir::Expr,
        (iface, slot): (DefId, usize),
    ) -> hir::Expr {
        let method = (self.cx.iface(iface))
            .and_then(|i| i.methods.get(slot))
            .filter(|m| !matches!(self.cx.ty.kind(m.ret), TyKind::Promise(..)))
            .map(|m| m.span);
        let Some(to) = method.and_then(|n| self.cx.written_range(n)) else {
            return h;
        };
        let at = e
            .map(super::method_value::strip_parens)
            .filter(|a| matches!(a.kind, ast::ExprKind::Arrow { .. }))
            .map(|a| a.span);
        self.cx.bind_stored_fn(at, h.ty, to);
        h
    }

    /// The function named by shorthand property `{ key }` for method `slot` of interface
    /// `iface`, expected as `exp` ([`Self::bind_slot_result`]).
    pub(super) fn slot_shorthand(
        &mut self,
        key: &ast::Ident,
        exp: TyId,
        slot: (DefId, usize),
    ) -> hir::Expr {
        let h = self.prop_value(key, None, Some(exp));
        self.bind_slot_result(None, h, slot)
    }
}
