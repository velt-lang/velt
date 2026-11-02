//! Inferred error types of generator types written without one (https://veltlang.com/docs/reference/async/
//! "Errors").
//!
//! TypeScript's `AsyncGenerator<T>` (and `Generator<T>`) may throw any value from `next()`, so
//! such a type written without Velt's error type argument in a user module is a site, as a
//! `Promise<T>` is (`promise_sites`): a generator whose body throws `E`, stored where the site's
//! type is expected (a returned value, a typed variable or field, an argument), adds `E` to that
//! site's set, and the program is checked again with `AsyncGenerator<T, E>` written there. The
//! result type of a generator function is no site: its errors are what its body throws. A
//! generator of another error type is no value of such a type once it took errors (its results
//! are laid out by the error type).
//! TypeScript's two- and three-argument spellings (`AsyncGenerator<T, void, unknown>`) are sites
//! too; a written Velt error type is not.

use velt_common::Span;

use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// The generator type `t` (`Generator<T, never>` / `AsyncGenerator<T, never>`) written at
    /// `span` in module `module` without an error type: with what is stored into it as its error
    /// type (`never` until something is), recording the site.
    pub(crate) fn generator_site(&mut self, t: TyId, span: Span, module: usize) -> TyId {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return t;
        };
        let never = self.ty.never;
        let user = self.scopes.get(module).is_some_and(|s| !s.is_std);
        if !self.is_generator_class(d)
            || args.len() < 2
            || args[1] != never
            || !user
            || span == Span::DUMMY
            || self.fn_sites.generator_ret
        {
            return t;
        }
        let e = self.site_error(span, module).unwrap_or(never);
        let mut args = args;
        args[1] = e;
        let g = self.ty.intern(TyKind::Adt(d, args));
        self.record_fn_site(span, module, g);
        g
    }

    /// Is `item` written with `args` a generator type without Velt's error type
    /// (`AsyncGenerator<T>`, or TypeScript's `AsyncGenerator<T, void, unknown>`)?
    pub(crate) fn generator_site_written(&self, item: Item, args: &[ast::TypeExpr]) -> bool {
        matches!(item, Item::Def(d) if self.is_generator_class(d))
            && (args.len() == 1 || crate::ts_protocol::ts_spelling(args))
    }

    /// [`Ctx::generator_site`] of `t` when `site`, else `t`.
    pub(crate) fn generator_site_if(
        &mut self,
        site: bool,
        t: TyId,
        span: Span,
        module: usize,
    ) -> TyId {
        match site {
            true => self.generator_site(t, span, module),
            false => t,
        }
    }

    /// The error type of a promise or generator type (`Promise<T, E>`, `AsyncGenerator<T, E>`,
    /// `Generator<T, E, N>`): what awaiting it or its `next()` throws.
    pub(super) fn rejection_error(&self, t: TyId) -> Option<TyId> {
        match self.ty.kind(t) {
            TyKind::Promise(_, e) => Some(*e),
            TyKind::Adt(d, args) if args.len() >= 2 && self.is_generator_class(*d) => Some(args[1]),
            _ => None,
        }
    }

    /// The promise or generator type `t` with error type `e`.
    pub(super) fn with_rejection_error(&mut self, t: TyId, e: TyId) -> TyId {
        match *self.ty.kind(t) {
            TyKind::Promise(v, _) => self.ty.promise_rejecting(v, e),
            _ => self.with_generator_error(t, e),
        }
    }

    /// Is `t` a type whose sites take error types (a promise or a generator type)?
    pub(super) fn takes_rejections(&self, t: TyId) -> bool {
        self.rejection_error(t).is_some()
    }

    /// Can a generator of type `used` have been typed by generator site type `g`: the same class
    /// and type arguments, error types ignored (`None`: not two generator types).
    pub(super) fn generator_site_matches(
        &self,
        g: TyId,
        used: TyId,
        m: impl Fn(TyId, TyId) -> bool,
    ) -> Option<bool> {
        let (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) = (self.ty.kind(g), self.ty.kind(used)) else {
            return None;
        };
        if d != e || !self.is_generator_class(*d) || xs.len() != ys.len() || xs.len() < 2 {
            return None;
        }
        let same = (xs.iter().zip(ys).enumerate())
            .filter(|(i, _)| *i != 1)
            .all(|(_, (x, y))| m(*x, *y));
        Some(same)
    }
}
