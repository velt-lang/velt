//! TypeScript's spellings of the iteration protocol types (docs/book/ts-developers.md).
//!
//! TypeScript's `Generator<T, TReturn, TNext>` (and `Iterator`, `Iterable` and their async
//! twins) take the return type second and the type `next(value)` takes third; Velt's take the
//! error type second (`Generator<T, E>`), since its generators return no value and take none
//! from `next()`. So that TypeScript code keeps compiling, a second argument TypeScript writes
//! for "returns nothing" (`void`, `undefined`, `unknown`, `any`) and a third argument are
//! dropped: `Generator<number, void, unknown>` is `Generator<number>`. Any other second argument
//! must be an error type (a class extending `Error`, a union of them, an interface or a type
//! parameter); anything else (`Generator<number, string>`) can only be TypeScript's return type,
//! and is reported at the user's annotation. Classes' base classes are known once
//! `collect::shapes` ran, so earlier checks wait until then ([`Ctx::check_deferred_ts_returns`]).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// The prelude's protocol types whose second argument TypeScript reads as `TReturn`.
const PROTOCOL_TYPES: [&str; 9] = [
    "Generator",
    "AsyncGenerator",
    "Iterator",
    "AsyncIterator",
    "Iterable",
    "AsyncIterable",
    "IterableIterator",
    "AsyncIterableIterator",
    "IteratorObject",
];

/// A second argument checked once base classes are known: (type name, `T`, the argument, its
/// span).
pub(crate) type TsReturnCheck = (&'static str, TyId, TyId, Span);

/// Is `t` a type TypeScript writes for "returns nothing" (`void`, `undefined`, `unknown`,
/// `any`)? The last three are no Velt types, so only this position accepts them.
fn no_return_spelling(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Void => true,
        ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
            matches!(path[0].name.as_str(), "undefined" | "unknown" | "any")
        }
        _ => false,
    }
}

/// Do the written arguments of a protocol type use TypeScript's spelling (a third argument, or
/// a "returns nothing" second one), so that no error type is written?
pub(crate) fn ts_spelling(args: &[ast::TypeExpr]) -> bool {
    args.len() == 3 || (args.len() == 2 && no_return_spelling(&args[1]))
}

impl Ctx<'_> {
    /// The protocol type `d` is (its name), if it is one.
    pub(crate) fn protocol_type(&self, d: DefId) -> Option<&'static str> {
        PROTOCOL_TYPES
            .into_iter()
            .find(|n| self.prelude_adt(n).or_else(|| self.prelude_iface(n)) == Some(d))
    }

    /// The type arguments of protocol type `name` written `args` (two or more), in Velt's terms
    /// (module docs). `None`: reported.
    pub(crate) fn protocol_args(
        &mut self,
        name: &'static str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Option<Vec<TyId>> {
        if args.len() > 3 {
            return Some(args.iter().map(|a| self.resolve_type(a, env)).collect());
        }
        let t = self.resolve_type(&args[0], env);
        if no_return_spelling(&args[1]) {
            return Some(vec![t]);
        }
        let second = self.resolve_type(&args[1], env);
        if t == self.ty.error || second == self.ty.error {
            return None;
        }
        if second == self.ty.unit {
            return Some(vec![t]);
        }
        if args.len() == 3 {
            self.report_ts_return(name, t, second, args[1].span, true);
            return None;
        }
        let check = (name, t, second, args[1].span);
        if self.shapes_done {
            self.check_ts_return(check);
        } else {
            self.deferred_ts_returns.push(check);
        }
        Some(vec![t, second])
    }

    /// Is `item` the prelude's `IteratorResult<T>` alias?
    pub(crate) fn is_iterator_result_alias(&self, item: Item) -> bool {
        matches!((item, self.prelude.get("IteratorResult")), (Item::Alias(a), Some(Item::Alias(b))) if a == *b)
    }

    /// TypeScript's `IteratorResult<T, TReturn>`: a `TReturn` that says "returns nothing" is
    /// dropped (`IteratorResult<number, void>` is `IteratorResult<number>`); any other one is
    /// reported, since a finished result carries no value. `None`: reported.
    pub(crate) fn iterator_result_args(
        &mut self,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Option<Vec<TyId>> {
        let t = self.resolve_type(&args[0], env);
        if no_return_spelling(&args[1]) {
            return Some(vec![t]);
        }
        let second = self.resolve_type(&args[1], env);
        if second == self.ty.unit || second == self.ty.error {
            return Some(vec![t]);
        }
        let (tn, rn) = (self.display(t), self.display(second));
        self.error(
            Diagnostic::error(
                format!("`IteratorResult` takes no return type: `{rn}` is TypeScript's `TReturn`"),
                args[1].span,
            )
            .with_note(format!(
                "TypeScript allows this (`IteratorResult<T, TReturn>`, whose finished result carries a `{rn}` value); Velt doesn't because generators and iterators return no value: a finished result is `{{ done: true }}`; write `IteratorResult<{tn}>`, and deliver a final value some other way (yield it, or store it where the caller can read it)"
            )),
        );
        None
    }

    /// The second-argument checks made before base classes were known.
    pub(crate) fn check_deferred_ts_returns(&mut self) {
        for c in std::mem::take(&mut self.deferred_ts_returns) {
            self.check_ts_return(c);
        }
    }

    fn check_ts_return(&mut self, (name, t, e, span): TsReturnCheck) {
        if !self.error_like(e, 0) {
            self.report_ts_return(name, t, e, span, false);
        }
    }

    /// Can `e` be an error type: `never`, a class extending `Error`, an interface, a type
    /// parameter, or a union of those?
    fn error_like(&mut self, e: TyId, depth: u32) -> bool {
        if self.ty.is_bottom(e) || depth > 8 {
            return true;
        }
        if let Some(ms) = self.union_members(e) {
            return ms.into_iter().all(|m| self.error_like(m, depth + 1));
        }
        match self.ty.kind(e) {
            TyKind::Param(_) | TyKind::Dyn(..) => true,
            _ => match (self.class_of(e), self.prelude_adt("Error")) {
                (Some((d, _)), Some(error)) => self.class_extends(d, error),
                _ => false,
            },
        }
    }

    /// `name<t, e>` (`with_next`: `name<t, e, TNext>`) where `e` can only be TypeScript's
    /// `TReturn`.
    fn report_ts_return(&mut self, name: &str, t: TyId, e: TyId, span: Span, with_next: bool) {
        let (tn, en) = (self.display(t), self.display(e));
        let (msg, ts) = match with_next {
            true => (
                format!("`{name}` takes no return type: `{en}` is TypeScript's `TReturn`"),
                format!("`{name}<T, TReturn, TNext>`"),
            ),
            false => (
                format!(
                    "the second type argument of `{name}` is the error type it throws, and `{en}` is not an error type"
                ),
                format!("`{name}<T, TReturn>`, which takes the return type second"),
            ),
        };
        self.error(Diagnostic::error(msg, span).with_note(format!(
            "TypeScript allows this ({ts}); Velt doesn't because a finished `IteratorResult` carries no value and `next()` takes no argument, so `{name}<T, E>` takes the error type `E` second; write `{name}<{tn}>`, or `{name}<{tn}, E>` with the error class `E` it throws"
        )));
    }
}
