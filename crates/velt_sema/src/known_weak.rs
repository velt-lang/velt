//! Weak collections and `weak` fields in types (known.rs's kind): which classes are the prelude's
//! `WeakMap`, `WeakSet` and `WeakRef`, and which types hold one or a class with a `weak` field,
//! whose values belong to the task that made them.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};

impl Ctx<'_> {
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
    /// handle), or a class with a `weak` field (its slot is the weak core's), so such values
    /// cannot be copied or passed to another task. This looks at the
    /// written type only: an interface value, a base-class value that may be a subclass, and a
    /// type parameter are looked into by the whole-program check of the values crossing
    /// threads (`crate::weak_threads`), which asks [`holds_weak_with`](Self::holds_weak_with).
    pub fn holds_weak(&mut self, t: TyId) -> bool {
        self.holds_weak_in(t, None, Weak::Either, &mut rustc_hash::FxHashSet::default())
    }

    /// [`holds_weak`](Self::holds_weak) for a `weak` field only, not a weak collection: what the
    /// error for copying such a value names.
    pub(crate) fn holds_weak_field_only(&mut self, t: TyId) -> bool {
        let mut seen = rustc_hash::FxHashSet::default();
        if !self.holds_weak_in(t, None, Weak::Field, &mut seen) {
            return false;
        }
        seen.clear();
        !self.holds_weak_in(t, None, Weak::Collection, &mut seen)
    }

    /// [`holds_weak`](Self::holds_weak), where an interface value or a class value also holds
    /// one when its interface or class is in `hiding` (some class converted to it holds one).
    pub(crate) fn holds_weak_with(
        &mut self,
        t: TyId,
        hiding: &rustc_hash::FxHashSet<DefId>,
    ) -> bool {
        self.holds_weak_in(
            t,
            Some(hiding),
            Weak::Either,
            &mut rustc_hash::FxHashSet::default(),
        )
    }

    fn holds_weak_in(
        &mut self,
        t: TyId,
        hiding: Option<&rustc_hash::FxHashSet<DefId>>,
        what: Weak,
        seen: &mut rustc_hash::FxHashSet<TyId>,
    ) -> bool {
        if !seen.insert(t) {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Adt(d, _) if what != Weak::Field && self.is_weak_class(d) => return true,
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) if hiding.is_some_and(|h| h.contains(&d)) => {
                return true
            }
            TyKind::Shared(_) | TyKind::FnPtr { .. } => return false,
            TyKind::Dyn(_, args) if hiding.is_some() => args,
            TyKind::Dyn(..) => return false,
            // A growing type: the walk stops and `crate::expansive_types` reports it
            // (`ObjectState::expansive_cut`). A deep type that is not one is walked to its end.
            TyKind::Adt(d, args) if self.ty.args_too_deep(&args) && self.expansive(d) => {
                self.objects.expansive_cut = Some(d);
                return false;
            }
            // A `weak` field: its slot belongs to the task that has its target (weak_fields).
            TyKind::Adt(d, _)
                if what != Weak::Collection
                    && matches!(&self.info[d.0 as usize], DefInfo::Adt(a) if a.fields.iter().any(|f| f.weak)) =>
            {
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
            k => crate::types::contents(&k),
        };
        parts
            .into_iter()
            .any(|p| self.holds_weak_in(p, hiding, what, seen))
    }
}

/// What [`Ctx::holds_weak_in`] looks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Weak {
    /// A `WeakMap`, `WeakSet` or `WeakRef`.
    Collection,
    /// A class with a `weak` field.
    Field,
    /// Either.
    Either,
}
