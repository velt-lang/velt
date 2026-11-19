//! The `Generator<T, E>` and `AsyncGenerator<T, E>` classes generators create, and the
//! interfaces a generator may be declared to return.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};

/// The interfaces a generator may be declared to return (`Generator<T, E>` implements them).
const SYNC_RESULTS: [&str; 4] = ["Iterator", "Iterable", "IterableIterator", "IteratorObject"];

/// The interfaces an async generator may be declared to return.
const ASYNC_RESULTS: [&str; 3] = ["AsyncIterator", "AsyncIterable", "AsyncIterableIterator"];

impl Ctx<'_> {
    /// The prelude's `Generator<T, E>` class: what calling a generator creates.
    pub fn generator_class(&self) -> Option<DefId> {
        self.prelude_adt("Generator")
    }

    /// The prelude's `AsyncGenerator<T, E>` class: what calling an async generator creates.
    pub fn async_generator_class(&self) -> Option<DefId> {
        self.prelude_adt("AsyncGenerator")
    }

    /// Is `d` the prelude's `Generator` or `AsyncGenerator` class?
    pub fn is_generator_class(&self, d: DefId) -> bool {
        Some(d) == self.generator_class() || Some(d) == self.async_generator_class()
    }

    /// `(def, [T, E])` when `t` is a type a generator may be declared to return:
    /// `Generator<T, E>` or one of the interfaces it implements ([`SYNC_RESULTS`]), or for an
    /// async generator `AsyncGenerator<T, E>` or one of [`ASYNC_RESULTS`].
    pub fn generator_result(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        self.generator_result_kind(t).map(|(d, args, _)| (d, args))
    }

    /// [`generator_result`](Self::generator_result), and whether it is an async one.
    pub fn generator_result_kind(&self, t: TyId) -> Option<(DefId, Vec<TyId>, bool)> {
        let iface = |name: &str, d: &DefId| Some(*d) == self.prelude_iface(name);
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if Some(*d) == self.generator_class() => {
                Some((*d, args.clone(), false))
            }
            TyKind::Adt(d, args) if Some(*d) == self.async_generator_class() => {
                Some((*d, args.clone(), true))
            }
            TyKind::Dyn(d, args) if SYNC_RESULTS.iter().any(|n| iface(n, d)) => {
                Some((*d, args.clone(), false))
            }
            TyKind::Dyn(d, args) if ASYNC_RESULTS.iter().any(|n| iface(n, d)) => {
                Some((*d, args.clone(), true))
            }
            _ => None,
        }
    }

    /// `T` when `t` is an `Iterable<T, E>` value: what an array literal written where one is
    /// expected holds (`sum([1.5, 2])` with `sum(xs: Iterable<f64>)`).
    pub fn iterable_elem(&self, t: TyId) -> Option<TyId> {
        match self.ty.kind(t) {
            TyKind::Dyn(d, args) if Some(*d) == self.prelude_iface("Iterable") => {
                args.first().copied()
            }
            _ => None,
        }
    }

    /// Does a value of type `t` hold a generator object (`Generator`, `AsyncGenerator`; in a
    /// field, element or payload)? Its suspended state cannot be copied, so such values cannot
    /// be cloned or passed to another task. Interface values are not looked into.
    pub fn holds_generator(&mut self, t: TyId) -> bool {
        self.holds_generator_in(t, &mut rustc_hash::FxHashSet::default())
    }

    fn holds_generator_in(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Adt(d, _) if self.is_generator_class(d) => return true,
            TyKind::Shared(_) | TyKind::FnPtr { .. } | TyKind::Dyn(..) => return false,
            // A growing type: the walk stops and `crate::expansive_types` reports it
            // (`ObjectState::expansive_cut`). A deep type that is not one is walked to its end.
            TyKind::Adt(d, args) if self.ty.args_too_deep(&args) && self.expansive(d) => {
                self.objects.expansive_cut = Some(d);
                return false;
            }
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    DefInfo::Enum(e) => e
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter().copied())
                        .collect(),
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.subst(f, &args)).collect()
            }
            k => crate::types::children(&k),
        };
        parts.into_iter().any(|p| self.holds_generator_in(p, seen))
    }

    /// Generator result type `t` with `e` as its error type argument.
    pub fn with_generator_error(&mut self, t: TyId, e: TyId) -> TyId {
        let Some((d, mut args)) = self.generator_result(t) else {
            return t;
        };
        if args.len() < 2 {
            return t;
        }
        args[1] = e;
        match self.ty.kind(t) {
            TyKind::Dyn(..) => self.ty.intern(TyKind::Dyn(d, args)),
            _ => self.ty.intern(TyKind::Adt(d, args)),
        }
    }

    /// `Generator<t, e, n>`: `n` is the type of `next(value)`'s argument (`never`: none).
    pub fn generator_ty_next(&mut self, t: TyId, e: TyId, n: TyId) -> TyId {
        match self.generator_class() {
            Some(d) => self.ty.intern(TyKind::Adt(d, vec![t, e, n])),
            None => self.ty.error,
        }
    }

    /// Are `a` and `b` `Generator<T, E, N>` types that differ only in `N`, one of them `never`?
    pub(crate) fn generator_next_differs(&self, a: TyId, b: TyId) -> bool {
        match (self.ty.kind(a), self.ty.kind(b)) {
            (TyKind::Adt(da, x), TyKind::Adt(db, y))
                if da == db && Some(*da) == self.generator_class() && x.len() == 3 =>
            {
                x[..2] == y[..2] && (x[2] == self.ty.never || y[2] == self.ty.never)
            }
            _ => false,
        }
    }

    /// The `next(value)` type `N` of a `Generator<T, E, N>` value type (`never`: none, also
    /// for any other type).
    pub fn generator_next_ty(&self, t: TyId) -> TyId {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if Some(*d) == self.generator_class() && args.len() == 3 => {
                args[2]
            }
            _ => self.ty.never,
        }
    }

    /// `AsyncGenerator<t, e>`.
    pub fn async_generator_ty(&mut self, t: TyId, e: TyId) -> TyId {
        match self.async_generator_class() {
            Some(d) => self.ty.intern(TyKind::Adt(d, vec![t, e])),
            None => self.ty.error,
        }
    }
}
