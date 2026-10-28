//! Prelude types the compiler knows by name: `Mutex<T>` (the lock-word struct behind
//! `new Mutex(x)` / `.with`), `JsonError` (thrown by `JSON.parse`), the dynamic JSON value
//! (`json::JsonValue`), `Map` (not JSON-serializable), the shared-state receiver shapes, and the
//! `Comparable<T>` interface behind ordering operators on generic params, the `Iterator<T, E>`
//! interface behind `for...of` over iterables, and the `Generator<T, E>` / `AsyncGenerator<T, E>`
//! classes generators create.

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{self, AdtKind, DefId, IntTy, TyId, TyKind};
use crate::type_reach::Look;

/// The method of the prelude's `Comparable<T>`; `a < b` on a bounded `T` calls it.
pub(crate) const COMPARE_TO: &str = "compareTo";

/// The interfaces a generator may be declared to return (`Generator<T, E>` implements them).
const SYNC_RESULTS: [&str; 4] = ["Iterator", "Iterable", "IterableIterator", "IteratorObject"];

/// The interfaces an async generator may be declared to return.
const ASYNC_RESULTS: [&str; 3] = ["AsyncIterator", "AsyncIterable", "AsyncIterableIterator"];

/// How many levels of fields, elements and payloads the searches for a resource look through: a
/// generic type that grows as it recurses (`class A<T> { next: A<T[]> }`) has no end.
const MAX_LEVELS: u32 = 32;

impl Ctx<'_> {
    /// `interface Comparable<T> { compareTo(other: T): i64 }` from the prelude.
    pub fn comparable_iface(&self) -> Option<DefId> {
        match self.prelude.get("Comparable") {
            Some(Item::Def(d)) if self.iface(*d).is_some() => Some(*d),
            _ => None,
        }
    }

    /// Reports `iface` named in an `extends` or `implements` at `span` if it is the prelude's
    /// `AtomicValue` or `Frozen`, whose types only the compiler decides; whether it was.
    pub(crate) fn refuse_known_iface(&mut self, iface: DefId, span: velt_common::Span) -> bool {
        let note = if Some(iface) == self.prelude_iface("AtomicValue") {
            ("AtomicValue", "the compiler decides which types an `Atomic` holds: numbers, `bool`, `string` and `shared` values")
        } else if Some(iface) == self.prelude_iface("Frozen") {
            ("Frozen", "the compiler decides whose `shared` values are read in place: those nothing could change")
        } else {
            return false;
        };
        self.error(
            velt_common::Diagnostic::error(format!("`{}` cannot be extended or implemented", note.0), span)
                .with_note(note.1),
        );
        true
    }

    /// An interface exported by the prelude under `name` (`Iterator`, `Iterable`).
    pub fn prelude_iface(&self, name: &str) -> Option<DefId> {
        match self.prelude.get(name) {
            Some(Item::Def(d)) if self.iface(*d).is_some() => Some(*d),
            _ => None,
        }
    }

    /// A function of the prelude named `name`, exported or not: the helpers and runtime
    /// functions the compiler calls itself (`body/expr/int32.rs`).
    pub fn prelude_fn(&self, name: &str) -> Option<DefId> {
        let found = match self.prelude.get(name) {
            Some(it) => Some(*it),
            None => (0..self.modules.len())
                .filter(|&m| {
                    self.scopes[m].is_std && self.modules[m].path.starts_with("std/prelude/")
                })
                .find_map(|m| self.scopes[m].items.get(name).copied()),
        };
        match found {
            Some(Item::Def(d)) if self.try_fn(d).is_some() => Some(d),
            _ => None,
        }
    }

    /// An ADT exported by the prelude under `name`.
    /// Does the private field `private_to`/`name` hold data that JSON may skip? Only the
    /// prelude `Error`'s `#name` (behind `e.name`): it is not a runtime handle, so error classes
    /// keep their JSON form (written without it, as Node leaves out a `name` from the
    /// prototype; decoded with its default).
    pub(crate) fn plain_private(&self, private_to: Option<DefId>, name: &str) -> bool {
        name == "#name" && private_to.is_some() && private_to == self.prelude_adt("Error")
    }

    pub fn prelude_adt(&self, name: &str) -> Option<DefId> {
        match self.prelude.get(name) {
            Some(Item::Def(d)) if self.adt(*d).is_some() => Some(*d),
            _ => None,
        }
    }

    /// Is `t` the prelude's `IteratorResult<T>`, `{ value: T; done: false } | { done: true }`
    /// (or an object union of that shape)? Its `value` can be read without narrowing
    /// (`body/expr/discriminated.rs`).
    pub fn is_iterator_result(&mut self, t: TyId) -> bool {
        let Some(ms) = self.union_members(t) else {
            return false;
        };
        let mut shapes: Vec<(usize, bool)> = vec![];
        for m in ms {
            let n = match self.ty.kind(m) {
                TyKind::Adt(d, _) => self.adt(*d).map(|a| a.fields.len()),
                _ => None,
            };
            let done = self.field_of(m, "done").map(|(_, f)| self.lit_value(f));
            match (n, done) {
                (Some(n), Some(Some(hir::LitValue::Bool(b)))) => shapes.push((n, b)),
                _ => return false,
            }
        }
        shapes.sort();
        shapes == [(1, true), (2, false)]
    }

    /// The methods of std/channel's `Channel<T>` that hand their value to another task (`send`,
    /// `trySend`), by def: a user type of the same name, or another std type's `send`, is no
    /// channel.
    pub fn channel_sends(&self) -> Vec<DefId> {
        let module = self
            .modules
            .iter()
            .position(|m| m.is_std && m.path == "std/channel");
        let channel = match module.and_then(|m| self.scopes[m].items.get("Channel")) {
            Some(Item::Def(d)) => *d,
            _ => return vec![],
        };
        let Some(a) = self.adt(channel) else {
            return vec![];
        };
        ["send", "trySend"]
            .iter()
            .filter_map(|n| a.methods.get(*n).map(|m| m.def))
            .collect()
    }

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

    /// Is `d` the prelude's `WeakMap`, `WeakSet` or `WeakRef` class, or a subclass of one
    /// (`class Cache extends WeakMap<K, V>` is the same runtime handle with fields of its own)?
    pub fn is_weak_class(&self, d: DefId) -> bool {
        ["WeakMap", "WeakSet", "WeakRef"]
            .iter()
            .filter_map(|n| self.prelude_adt(n))
            .any(|w| self.class_extends(d, w))
    }

    /// The `WeakMap<K, V>`, `WeakSet<T>` or `WeakRef<T>` that the class type `t` is or extends
    /// (the base class with `t`'s type arguments substituted), and that class's name.
    pub fn weak_root(&mut self, t: TyId) -> Option<(TyId, &'static str)> {
        let mut cur = t;
        for _ in 0..64 {
            let (d, _) = self.class_of(cur)?;
            let own = ["WeakMap", "WeakSet", "WeakRef"]
                .into_iter()
                .find(|n| self.prelude_adt(n) == Some(d));
            if let Some(name) = own {
                return Some((cur, name));
            }
            cur = self.base_of(cur)?;
        }
        None
    }

    /// Does a value of type `t` hold a `WeakMap`, `WeakSet` or `WeakRef` (in a field, element
    /// or payload)? They belong to the task that made them (a copy would share the runtime
    /// handle), so such values cannot be copied or passed to another task. This looks at the
    /// written type only: an interface value, a base-class value that may be a subclass, and a
    /// type parameter are looked into by the whole-program check of the values crossing
    /// threads (`crate::weak_threads`), which asks [`holds_weak_with`](Self::holds_weak_with).
    pub fn holds_weak(&mut self, t: TyId) -> bool {
        self.holds_weak_in(t, None, &mut rustc_hash::FxHashSet::default())
    }

    /// [`holds_weak`](Self::holds_weak), where an interface value or a class value also holds
    /// one when its interface or class is in `hiding` (some class converted to it holds one).
    pub(crate) fn holds_weak_with(
        &mut self,
        t: TyId,
        hiding: &rustc_hash::FxHashSet<DefId>,
    ) -> bool {
        self.holds_weak_in(t, Some(hiding), &mut rustc_hash::FxHashSet::default())
    }

    fn holds_weak_in(
        &mut self,
        t: TyId,
        hiding: Option<&rustc_hash::FxHashSet<DefId>>,
        seen: &mut rustc_hash::FxHashSet<TyId>,
    ) -> bool {
        if !seen.insert(t) {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Adt(d, _) if self.is_weak_class(d) => return true,
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) if hiding.is_some_and(|h| h.contains(&d)) => {
                return true
            }
            TyKind::Shared(_) | TyKind::FnPtr { .. } => return false,
            TyKind::Dyn(_, args) if hiding.is_some() => args,
            TyKind::Dyn(..) => return false,
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
        parts
            .into_iter()
            .any(|p| self.holds_weak_in(p, hiding, seen))
    }

    /// Can a `shared<T>` of `t` be read like a `T` (#36, `Intrinsic::SharedRead`)? Not when
    /// `t` holds what a reader could change or must not copy (a `Mutex`, a weak map or ref, a
    /// promise), or what the compiler cannot see into (a function or interface value, a type
    /// parameter, a class with subclasses, whose objects may have more fields), nor when it
    /// holds a type with a `clone()` of its own: copying what a read gets would run it on the
    /// objects other threads read, as a side effect of reading. A `shared` inside is fine: it
    /// counts atomically.
    pub fn frozen_readable(&mut self, t: TyId) -> bool {
        self.frozen_readable_in(t, &mut rustc_hash::FxHashSet::default())
    }

    fn frozen_readable_in(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return true;
        }
        // `any` / `unknown` may hold anything at run time.
        if self.dynamic(t).is_some() {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Shared(_) => return true,
            // `T extends Frozen` in the body being checked: whatever it is, it is readable.
            TyKind::Param(p) => return self.frozen_params.contains(&p),
            TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Promise(..) => return false,
            TyKind::Adt(d, _)
                if Some(d) == self.mutex_ty()
                    || Some(d) == self.rwlock_ty()
                    || self.is_weak_class(d) =>
            {
                return false
            }
            TyKind::Adt(d, _) if self.has_subclass(d) || self.own_clone(d).is_some() => {
                return false
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
        parts.into_iter().all(|p| self.frozen_readable_in(p, seen))
    }

    /// Does a value of type `t` reach a `Mutex` or `RwLock`, whose value is read only under
    /// its lock (through a `shared`, a field, an element or a payload)? Interface and `any`
    /// values are not looked into: what they hold was checked when it went in.
    pub fn holds_lock(&mut self, t: TyId) -> bool {
        self.holds_lock_in(t, &mut rustc_hash::FxHashSet::default())
    }

    fn holds_lock_in(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) -> bool {
        if !seen.insert(t) || self.dynamic(t).is_some() {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Param(_) => {
                return false
            }
            TyKind::Adt(d, _) if Some(d) == self.mutex_ty() || Some(d) == self.rwlock_ty() => {
                return true
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
        parts.into_iter().any(|p| self.holds_lock_in(p, seen))
    }

    /// Does some class extend class `d`?
    fn has_subclass(&self, d: DefId) -> bool {
        let is_class = |s: DefId| self.adt(s).is_some_and(|a| a.kind == AdtKind::Class);
        is_class(d)
            && (0..self.info.len() as u32)
                .map(DefId)
                .any(|s| s != d && is_class(s) && self.class_extends(s, d))
    }

    /// Does a value of type `t` hold a function value (in a field, element or payload)?
    /// Interface and `shared` values are not looked into.
    pub fn holds_fn(&mut self, t: TyId) -> bool {
        self.holds_fn_in(t, &mut rustc_hash::FxHashSet::default())
    }

    fn holds_fn_in(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::FnPtr { .. } | TyKind::Closure(_) => return true,
            TyKind::Shared(_) | TyKind::Dyn(..) => return false,
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
        parts.into_iter().any(|p| self.holds_fn_in(p, seen))
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

    /// The type arguments of class `d` as diagnostics show them: a `Generator<T, E, never>` is
    /// `Generator<T, E>`.
    pub(crate) fn shown_args<'a>(&self, d: DefId, args: &'a [TyId]) -> &'a [TyId] {
        match args {
            [_, _, n] if Some(d) == self.generator_class() && *n == self.ty.never => &args[..2],
            _ => args,
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

    /// `struct Mutex<T> { lock: u64; value: T }` from the prelude.
    pub fn mutex_ty(&self) -> Option<DefId> {
        self.prelude_adt("Mutex")
    }

    /// `struct RwLock<T> { lock: u64; value: T }` from the prelude.
    pub fn rwlock_ty(&self) -> Option<DefId> {
        self.prelude_adt("RwLock")
    }

    /// `struct Atomic<T> { v: T }` from the prelude (docs/internals/design/atomics.md).
    pub fn atomic_ty(&self) -> Option<DefId> {
        self.prelude_adt("Atomic")
    }

    /// `struct ThreadLocal<T> { init: T }` from the prelude.
    pub fn thread_local_ty(&self) -> Option<DefId> {
        self.prelude_adt("ThreadLocal")
    }

    /// `T` of a `ThreadLocal<T>`.
    pub fn thread_local_value(&self, t: TyId) -> Option<TyId> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if Some(*d) == self.thread_local_ty() => args.first().copied(),
            _ => None,
        }
    }

    /// Does a deep copy of a value of `t` copy an `Atomic` (in the value, its arrays, payloads
    /// or the objects it holds; not behind a `shared`, nor in a function, interface or `any`
    /// value)?
    pub fn holds_atomic(&mut self, t: TyId) -> bool {
        self.holds_atomic_in(t, &mut rustc_hash::FxHashSet::default())
    }

    fn holds_atomic_in(&mut self, t: TyId, seen: &mut rustc_hash::FxHashSet<TyId>) -> bool {
        if !seen.insert(t) || self.dynamic(t).is_some() {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Shared(_)
            | TyKind::Dyn(..)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_)
            | TyKind::Param(_) => return false,
            TyKind::Adt(d, _) if Some(d) == self.atomic_ty() => return true,
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
        parts.into_iter().any(|p| self.holds_atomic_in(p, seen))
    }

    /// Is `t` a reference an `Atomic` word holds: `shared<U>`, `shared<U> | null` or `string`
    /// (atomics.md "References")?
    pub fn atomic_ref(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Shared(_) | TyKind::Str => true,
            TyKind::Option(inner, _) => matches!(self.ty.kind(*inner), TyKind::Shared(_)),
            _ => false,
        }
    }

    /// `T` of an `Atomic<T>`.
    pub fn atomic_value(&self, t: TyId) -> Option<TyId> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if Some(*d) == self.atomic_ty() => args.first().copied(),
            _ => None,
        }
    }

    /// `T` of a `Mutex<T>` or `RwLock<T>` receiver, or a `shared` one: a lock word and a value,
    /// reached only through the lock.
    pub fn mutex_value(&self, t: TyId) -> Option<TyId> {
        let m = match self.ty.kind(t) {
            TyKind::Shared(inner) => *inner,
            _ => t,
        };
        match self.ty.kind(m) {
            TyKind::Adt(d, args) if Some(*d) == self.mutex_ty() || Some(*d) == self.rwlock_ty() => {
                args.first().copied()
            }
            _ => None,
        }
    }

    /// Is `t` an `RwLock<T>` or `shared<RwLock<T>>`?
    pub fn is_rwlock(&self, t: TyId) -> bool {
        let m = match self.ty.kind(t) {
            TyKind::Shared(inner) => *inner,
            _ => t,
        };
        matches!(self.ty.kind(m), TyKind::Adt(d, _) if Some(*d) == self.rwlock_ty())
    }

    /// The integer type of a `shared<int>` receiver whose atomics the runtime provides (64-bit).
    pub fn atomic_int(&self, t: TyId) -> Option<TyId> {
        let TyKind::Shared(inner) = self.ty.kind(t) else {
            return None;
        };
        match self.ty.kind(*inner) {
            TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize) => Some(*inner),
            _ => None,
        }
    }

    /// Does a value of type `t` own a value with a `[Symbol.dispose]` drop hook or a promise (directly, or
    /// in a field, element or payload)? The compiler-generated deep `clone()` would copy such a
    /// resource handle (or the pointer to a future, which runs and is freed once) instead of
    /// duplicating the resource, so it is not available for these types.
    pub fn owns_resource(&mut self, t: TyId) -> bool {
        let mut look = |cx: &mut Self, t: TyId| match cx.ty.kind(t).clone() {
            // A shared value's clone only bumps the reference count.
            TyKind::Shared(_) => Look::Stop,
            TyKind::Promise(..) => Look::Found,
            // A function value owns its captures, not its parameter or result types.
            TyKind::FnPtr { .. } => Look::Stop,
            TyKind::Adt(d, _) if matches!(&cx.info[d.0 as usize], DefInfo::Adt(a) if a.has_dispose) => {
                Look::Found
            }
            k => Look::Parts(cx.parts_of(k)),
        };
        self.find_within(t, MAX_LEVELS, &mut look).is_some()
    }

    /// The field types of an object type of kind `k`, the payload types of an enum, the element
    /// types of any other type.
    fn parts_of(&mut self, k: TyKind) -> Vec<TyId> {
        let TyKind::Adt(d, args) = k else {
            return crate::types::children(&k);
        };
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

    /// Does a value of type `t` own a resource that no deep copy can duplicate: a value with a
    /// `[Symbol.dispose]` drop hook whose type has no `clone()` of its own ([`own_clone`]), or
    /// a promise (directly, or in a field, element or payload)? A deep copy (`x.clone()`, a
    /// copy for another task) calls a resource type's own `clone()`.
    ///
    /// [`own_clone`]: Self::own_clone
    pub fn owns_uncopyable(&mut self, t: TyId) -> bool {
        self.uncopyable_part(t).is_some()
    }

    /// The part of `t` that makes it [`owns_uncopyable`](Self::owns_uncopyable): the resource
    /// type without `clone()`, or the promise type (`t` itself, or a field, element or payload).
    pub fn uncopyable_part(&mut self, t: TyId) -> Option<TyId> {
        let mut look = |cx: &mut Self, t: TyId| match cx.ty.kind(t).clone() {
            // A promise is copied by sharing it (#212).
            TyKind::Shared(_) | TyKind::FnPtr { .. } | TyKind::Promise(..) => Look::Stop,
            TyKind::Adt(d, _) if cx.own_clone(d).is_some() => Look::Stop,
            TyKind::Adt(d, _) if matches!(&cx.info[d.0 as usize], DefInfo::Adt(a) if a.has_dispose) => {
                Look::Found
            }
            k => Look::Parts(cx.parts_of(k)),
        };
        self.find_within(t, MAX_LEVELS, &mut look)
    }

    /// Why a `t` cannot be deep-copied, for a diagnostic: "`Conn` owns a resource
    /// (`[Symbol.dispose]`) and has no `clone()`", or "`Pair` holds a `Conn`, which …".
    pub fn uncopyable_why(&mut self, t: TyId) -> String {
        let tn = self.display(t);
        let part = self.uncopyable_part(t).unwrap_or(t);
        let resource = "owns a resource (`[Symbol.dispose]`) and has no `clone()`";
        match part == t {
            true => format!("`{tn}` {resource}"),
            false => {
                let pn = self.display(part);
                format!("`{tn}` holds a `{pn}`, which {resource}")
            }
        }
    }

    /// The class's own `clone()` method: declared on class `d` itself, without parameters,
    /// neither async nor throwing, returning the class. Deep copies call it when the class owns
    /// a resource (velt_vir `Cx::own_clone` finds the same method; a class without a resource
    /// is copied field by field, and needs no `clone()` here either).
    pub fn own_clone(&self, d: DefId) -> Option<DefId> {
        let DefInfo::Adt(a) = &self.info[d.0 as usize] else {
            return None;
        };
        if a.kind != hir::AdtKind::Class {
            return None;
        }
        let m = a.methods.get("clone").filter(|m| !m.is_static)?;
        let f = self.try_fn(m.def)?;
        let returns_self = matches!(self.ty.kind(f.ret), TyKind::Adt(r, _) if *r == d);
        let throws = f.declared_throws.is_some()
            || f.throws
                .is_some_and(|e| !matches!(self.ty.kind(e), TyKind::Never));
        (f.owner == Some(d) && f.params.is_empty() && !f.is_async && !throws && returns_self)
            .then_some(m.def)
    }

    /// The dynamic JSON value class: the prelude's `JsonValue` itself, not a user class of the
    /// same name (that one is an ordinary class to JSON).
    pub fn is_json_value(&self, d: DefId) -> bool {
        self.prelude_adt("JsonValue") == Some(d)
    }

    /// `JsonError` (prelude class), the type `JSON.parse` throws.
    pub fn json_error_ty(&mut self) -> Option<TyId> {
        let d = self.prelude_adt("JsonError")?;
        Some(self.ty.intern(TyKind::Adt(d, vec![])))
    }
}
