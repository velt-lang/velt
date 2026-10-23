//! Counted types (semantics stage 2, docs/design/semantics-stage2.md §3.1): which concrete types
//! carry a reference count because the program shares their values.
//!
//! Lowering records *facts* while it builds functions — every share of a value of type `T`
//! ([`Cx::note_share`]) and every borrow of a place reached through a value of type `C` that
//! has to stay alive while user code runs ([`Cx::note_projection`], see `stabilize.rs`).
//! [`Cx::close_boxing`] turns them into the next [`Boxing`]; `lower_program` repeats lowering
//! until the set is stable. Programs that share nothing lower once, with today's
//! representation everywhere.

mod kinds;
mod transfers;
mod weak;

use rustc_hash::FxHashSet;
use std::collections::BTreeSet;

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::Cx;

pub(super) use kinds::ShareKind;

/// The counted types of one lowering pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Boxing {
    /// Class hierarchies (by root class) whose objects carry a count in front of them.
    pub(super) roots: FxHashSet<DefId>,
    /// Arrays and object types whose values are counted boxes (`Ptr` to the inline value with
    /// the count in front) instead of inline values.
    pub(super) boxes: FxHashSet<TyId>,
    /// Types of values borrowed in place inside a counted object: a pointer to one may have
    /// other owners, so params of these types are never `noalias`.
    interior: FxHashSet<TyId>,
    /// Interface types whose values are compared by identity: an object type converted to one
    /// is counted, so the interface value points at the object itself (`make_dyn`).
    identity_dyns: FxHashSet<TyId>,
    /// The program compares `any` values that may both be objects, or keys collections by
    /// them (any_eq.rs): every dynamic type holds objects by identity (#1256).
    any_identity: bool,
    /// The program compares function values: every evaluation of a closure without captures
    /// gets an environment of its own as its identity (closure.rs).
    fn_identity: bool,
    /// The program writes a member through `any` (`AnySet` / `AnySetIndex`): the `any` tables
    /// have the write slots (any_set.rs).
    pub(super) any_writes: bool,
    /// The program tests `key in x` on an `any` (`AnyHas`): the `any` tables have the `has`
    /// slot (any_has.rs).
    pub(super) any_has: bool,
    /// The prelude deletes a property of an `any` that is not a proxy (`AnyDelete`): the `any`
    /// tables have the delete slot (any_delete.rs).
    pub(super) any_delete: bool,
    /// The program lists the keys of an `any` (`AnyKeys`): the `any` tables have the keys slot
    /// (any_keys.rs).
    pub(super) any_keys: bool,
    /// The program writes a dynamic value as JSON: the `any` tables have the JSON slot
    /// (json/any.rs).
    pub(super) any_json: bool,
    /// Object types that contain themselves by value (`interface Node { next?: Node }`, #376):
    /// every instance is a counted box, so the field holds a pointer and the size is finite.
    recursive: FxHashSet<DefId>,
    /// Counted types a transfer to another thread can copy (transfers.rs).
    transferred: FxHashSet<TyId>,
    /// Weak-capable class hierarchies (by root class) and other counted types: their release
    /// tells a weakly held object apart (weak.rs). Empty in a program without weak references.
    weak_roots: FxHashSet<DefId>,
    weak: FxHashSet<TyId>,
    /// `object` is a weak key or `WeakRef` target (issue #926): the concrete types that reach
    /// `object` (`dyn_types`) are counted and weak-capable, the `any` tables have the weak slot,
    /// and the trace glue looks into dynamic values (weak_object.rs). False in every other
    /// program, which compiles as before.
    pub(super) weak_object: bool,
    pub(super) dyn_types: FxHashSet<TyId>,
    /// Interfaces whose values are converted to `any` / `unknown`: their tables hold the
    /// `any` table of their concrete type (`Cx::vtable`, `make_dyn`).
    pub(super) dyn_to_any: FxHashSet<DefId>,
    /// Typed proxies (#823, proxy_typed.rs): class hierarchies (by root class) and object types
    /// whose values may hold a tagged pointer to the prelude's `Proxy` object. Empty in a
    /// program that creates no proxy.
    pub(super) proxy_roots: FxHashSet<DefId>,
    pub(super) proxy_types: FxHashSet<TyId>,
    /// The prelude's `Proxy` class, once the program creates a proxy.
    pub(super) proxy_class: Option<DefId>,
    /// The types a `Proxy` object's target is traced as by weak maps (glue/trace.rs): the
    /// concrete `AnyTo` targets and `Proxy` itself. Others are opaque (a leak at worst).
    pub(super) proxy_traced: BTreeSet<TyId>,
}

impl Boxing {
    /// The counted types before any share is observed: the recursive object types, and the
    /// object types sema relies on being counted (`hir::Program::counted_objects`).
    pub(super) fn initial(hir: &hir::Program) -> Boxing {
        let mut recursive = recursive_objects(hir);
        // A generic counted type (a generic interface adapter): every instance of it.
        for &t in &hir.counted_objects {
            if let TyKind::Adt(d, args) = hir.types.kind(t) {
                if args
                    .iter()
                    .any(|a| matches!(hir.types.kind(*a), TyKind::Param(_)))
                {
                    recursive.insert(*d);
                }
            }
        }
        Boxing {
            recursive,
            boxes: hir.counted_objects.iter().copied().collect(),
            ..Boxing::default()
        }
    }
}

/// Object types (anonymous ones, which field-only interfaces are by now) that reach themselves
/// through fields stored inline: nested object types, nullable values, tuples and union
/// payloads, not arrays, classes or other pointers.
fn recursive_objects(hir: &hir::Program) -> FxHashSet<DefId> {
    let objects: Vec<DefId> = (0..hir.defs.len() as u32)
        .map(DefId)
        .filter(|d| matches!(hir.def(*d), hir::Def::Adt(a) if a.kind == AdtKind::Anon))
        .collect();
    objects
        .into_iter()
        .filter(|&d| {
            let mut seen = FxHashSet::default();
            fields_of(hir, d)
                .into_iter()
                .any(|t| reaches(hir, t, d, &mut seen))
        })
        .collect()
}

fn fields_of(hir: &hir::Program, d: DefId) -> Vec<TyId> {
    match hir.def(d) {
        hir::Def::Adt(a) if a.kind != AdtKind::Class => a.fields.iter().map(|f| f.ty).collect(),
        hir::Def::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// Does a value of type `t` hold a `target` inline?
fn reaches(hir: &hir::Program, t: TyId, target: DefId, seen: &mut FxHashSet<TyId>) -> bool {
    if !seen.insert(t) {
        return false;
    }
    match hir.types.kind(t) {
        TyKind::Adt(d, args) => {
            // A class's type arguments are behind its pointer (`Map<string, G>`): not walked,
            // so `seen` holds only types reached inline.
            *d == target
                || matches!(hir.def(*d), hir::Def::Adt(a) if a.kind != AdtKind::Class)
                    && args.iter().any(|a| reaches(hir, *a, target, seen))
                || fields_of(hir, *d)
                    .into_iter()
                    .any(|f| reaches(hir, f, target, seen))
        }
        TyKind::Option(x, _) => reaches(hir, *x, target, seen),
        TyKind::Tuple(ts) => ts.iter().any(|x| reaches(hir, *x, target, seen)),
        TyKind::Result(a, b) => reaches(hir, *a, target, seen) || reaches(hir, *b, target, seen),
        _ => false,
    }
}

/// What a lowering pass observed (see the module docs).
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// Types of shared values.
    shares: FxHashSet<TyId>,
    /// `(container, value)`: a borrowed place of type `value` reached through a `container`.
    projections: FxHashSet<(TyId, TyId)>,
    /// Object types compared by identity (`==`, same.rs).
    identity: FxHashSet<TyId>,
    /// `any` values compared by identity (`Boxing::any_identity`).
    any_identity: bool,
    /// Interface upcasts made, `(from, to)` (upcast.rs): `from` values are compared by
    /// identity when `to` values are.
    upcasts: FxHashSet<(TyId, TyId)>,
    /// Value types a callee may change in place while it borrows them out of an array other
    /// references reach (stabilize.rs): counted, so the borrow can share the value itself.
    identity_borrows: FxHashSet<TyId>,
    /// Types of values transferred to another thread as a whole (transfers.rs).
    transfers: FxHashSet<TyId>,
    /// Function values are compared (or hashed) somewhere.
    fn_compared: bool,
    /// A member is written through `any`.
    any_set: bool,
    /// `key in x` is tested on an `any`.
    any_has: bool,
    /// A property of an `any` is deleted.
    any_delete: bool,
    /// `Object.keys` of an `any` is listed.
    any_keys: bool,
    /// A dynamic value is written as JSON.
    any_json: bool,
    /// Capture types of the closure environments built (transfers.rs).
    captures: FxHashSet<TyId>,
    /// Concrete types a vtable was built for: interface implementors and classes in a
    /// hierarchy (transfers.rs).
    vtable_types: FxHashSet<TyId>,
    /// Types of weak keys, `WeakRef` targets and weak map values (weak.rs).
    weak_seeds: FxHashSet<TyId>,
    /// `object` is a weak key or a `WeakRef` target.
    pub(super) weak_object: bool,
    /// Concrete types an `any` table was built for: the types dynamic values hold.
    pub(super) dyn_types: FxHashSet<TyId>,
    /// Concrete types converted to `object` straight from a typed value.
    pub(super) object_types: FxHashSet<TyId>,
    /// An `any`, `unknown` or interface value is converted to `object`: it may hold any type
    /// dynamic values hold.
    pub(super) object_from_dyn: bool,
    /// Interfaces whose values are converted to `any` / `unknown`.
    dyn_to_any: FxHashSet<DefId>,
    /// The program creates a `Proxy` (proxy_typed.rs).
    pub(super) proxy_new: bool,
    /// Concrete targets of `AnyTo` in the program's own code (proxy_typed.rs).
    pub(super) proxy_targets: FxHashSet<TyId>,
    /// `(from, to)` of the `Upcast`s lowered: a proxiable `from` makes `to` proxiable.
    pub(super) proxy_casts: FxHashSet<(TyId, TyId)>,
    /// A share was lowered as a placeholder because its type was not counted yet: the output
    /// of this pass must not be used.
    pub(super) unmet: bool,
}

impl Cx<'_> {
    /// Record that a value of type `t` is shared.
    pub(super) fn note_share(&mut self, t: TyId) {
        self.facts.shares.insert(t);
    }

    /// Record that values of the object type `t` are compared by identity.
    pub(super) fn note_identity(&mut self, t: TyId) {
        self.facts.identity.insert(t);
    }

    /// Record that a value of type `t` (one of `in_place_parts`) may be changed in place by a
    /// callee while it is borrowed out of an array other references reach.
    pub(super) fn note_identity_borrow(&mut self, t: TyId) {
        self.facts.identity_borrows.insert(t);
    }

    /// Record an interface upcast of a `from` value to `to` (upcast.rs).
    pub(super) fn note_upcast(&mut self, from: TyId, to: TyId) {
        self.facts.upcasts.insert((from, to));
    }

    /// Record that values of the interface `iface` are converted to `any` / `unknown`.
    pub(super) fn note_dyn_to_any(&mut self, iface: DefId) {
        self.facts.dyn_to_any.insert(iface);
    }

    /// Record that `any` values are compared by identity (see [`Boxing::any_identity`]).
    pub(super) fn note_any_identity(&mut self) {
        self.facts.any_identity = true;
    }

    /// Record that function values are compared by identity.
    pub(super) fn note_fn_identity(&mut self) {
        self.facts.fn_compared = true;
    }

    /// Record that a member is written through `any`.
    pub(super) fn note_any_set(&mut self) {
        self.facts.any_set = true;
    }

    /// Record that `key in x` is tested on an `any`.
    pub(super) fn note_any_has(&mut self) {
        self.facts.any_has = true;
    }

    /// Record that a property of an `any` is deleted.
    pub(super) fn note_any_delete(&mut self) {
        self.facts.any_delete = true;
    }

    /// Record that `Object.keys` of an `any` is listed.
    pub(super) fn note_any_keys(&mut self) {
        self.facts.any_keys = true;
    }

    /// Record that a dynamic value is written as JSON.
    pub(super) fn note_any_json(&mut self) {
        self.facts.any_json = true;
    }

    /// Do closures need an identity of their own (see [`Boxing::fn_identity`])?
    pub(super) fn fn_identity(&self) -> bool {
        self.boxing.fn_identity
    }

    /// The interface value type `dyn_ty` is compared by identity, and its implementor `t` is an
    /// object type that a conversion would copy: record that `t` must be counted.
    pub(super) fn note_dyn_identity(&mut self, dyn_ty: TyId, t: TyId) {
        // Once values convert to `object` / `Function` (issue #926), which key collections by
        // identity, every dynamic type holds objects by identity: a value may reach `object`
        // through `any` or `unknown` (`u as object`).
        let identity = self.boxing.identity_dyns.contains(&dyn_ty)
            || (self.is_dynamic(dyn_ty)
                && (self.boxing.any_identity
                    || self
                        .boxing
                        .identity_dyns
                        .iter()
                        .any(|d| self.is_narrow_dynamic(*d))));
        if identity && self.copied_object(t) {
            self.facts.identity.insert(t);
            self.facts.shares.insert(t);
        }
    }

    /// Record a borrow of a `value`-typed place reached through a `container`-typed value.
    pub(super) fn note_projection(&mut self, container: TyId, value: TyId) {
        self.facts.projections.insert((container, value));
    }

    /// Does a value of `t` carry a reference count?
    pub(super) fn counted(&self, t: TyId) -> bool {
        self.counted_in(&self.boxing, t)
    }

    /// Are references to values of `t` provably unique while a call borrows them (no other
    /// owner can reach the value: it is neither counted nor borrowed inside a counted object)?
    /// Only then may a param of type `t` be `noalias` (vir.rs invariant 9).
    pub(super) fn unique_refs(&self, t: TyId) -> bool {
        !self.counted(t) && !self.boxing.interior.contains(&t)
    }

    /// Display names of the counted types (`VELT_DEBUG_COUNTED=1`, bench/rc_stats.ps1).
    pub(super) fn counted_names(&self) -> Vec<String> {
        let roots = self.boxing.roots.iter().map(|d| match self.hir.def(*d) {
            velt_sema::hir::Def::Adt(a) => format!("class {}", a.name),
            _ => "?".into(),
        });
        let boxes = self.boxing.boxes.iter().map(|&t| self.type_name(t));
        let recursive = self
            .boxing
            .recursive
            .iter()
            .map(|d| match self.hir.def(*d) {
                velt_sema::hir::Def::Adt(a) => format!("{} (recursive)", a.name),
                _ => "?".into(),
            });
        let mut out: Vec<String> = roots.chain(boxes).chain(recursive).collect();
        out.sort();
        out
    }

    /// The counted types implied by this pass's facts (a superset of the current ones).
    pub(super) fn close_boxing(&mut self) -> Boxing {
        let mut next = self.boxing.clone();
        next.fn_identity |= self.facts.fn_compared;
        next.any_identity |= self.facts.any_identity;
        next.any_writes |= self.facts.any_set;
        next.any_has |= self.facts.any_has;
        next.any_delete |= self.facts.any_delete;
        next.any_keys |= self.facts.any_keys;
        next.any_json |= self.facts.any_json;
        next.dyn_to_any
            .extend(self.facts.dyn_to_any.iter().copied());
        let dyns = self.facts.identity.iter().copied();
        let dyns: Vec<TyId> = dyns
            .filter(|t| matches!(self.kind(*t), TyKind::Dyn(..)))
            .collect();
        next.identity_dyns.extend(dyns);
        loop {
            let more: Vec<TyId> = self
                .facts
                .upcasts
                .iter()
                .filter(|(f, t)| next.identity_dyns.contains(t) && !next.identity_dyns.contains(f))
                .map(|&(f, _)| f)
                .collect();
            if more.is_empty() {
                break;
            }
            next.identity_dyns.extend(more);
        }
        next.boxes
            .extend(self.facts.identity_borrows.iter().copied());
        self.close_proxies(&mut next);
        let mut work: Vec<TyId> = self.facts.shares.iter().copied().collect();
        // A weak key of type `object` (issue #926) is any dynamic value's object: each type
        // dynamic values hold is counted, so its data pointer is the object itself.
        next.weak_object |= self.facts.weak_object;
        if next.weak_object {
            // Only the types that reach `object` pay (all a dynamic value holds when an `any`,
            // `unknown` or interface value converts to `object`).
            let reach = match self.facts.object_from_dyn {
                true => &self.facts.dyn_types,
                false => &self.facts.object_types,
            };
            next.dyn_types.extend(reach.iter().copied());
            work.extend(next.dyn_types.iter().copied());
        }
        let mut seen = FxHashSet::default();
        loop {
            while let Some(t) = work.pop() {
                if seen.insert(t) {
                    self.close_share(t, &mut next, &mut work);
                }
            }
            // A borrow through a counted container keeps the borrowed value alive by sharing it.
            let projections: Vec<(TyId, TyId)> = self.facts.projections.iter().copied().collect();
            for (c, v) in projections {
                if self.counted_in(&next, c) {
                    next.interior.insert(v);
                    if !seen.contains(&v) {
                        work.push(v);
                    }
                }
            }
            if work.is_empty() {
                self.close_transfers(&mut next);
                self.close_weak(&mut next);
                return next;
            }
        }
    }

    /// What sharing a value of `t` requires of `next`.
    fn close_share(&mut self, t: TyId, next: &mut Boxing, work: &mut Vec<TyId>) {
        // An object type copied when shared would lose its identity: compared by identity, a
        // shared one is one counted object.
        if self.facts.identity.contains(&t) && self.copied_object(t) {
            next.boxes.insert(t);
            return;
        }
        match self.share_kind(t) {
            ShareKind::Object => match self.kind(t) {
                TyKind::Adt(d, _) if self.is_class(t) => {
                    next.roots.insert(self.class_root(d));
                }
                _ => {
                    next.boxes.insert(t);
                }
            },
            ShareKind::Value => work.extend(self.part_types(t)),
            _ => {}
        }
    }

    /// `counted` against a candidate set.
    pub(super) fn counted_in(&self, b: &Boxing, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => b.roots.contains(&self.class_root(*d)),
            TyKind::Adt(d, _) if b.recursive.contains(d) => true,
            _ => b.boxes.contains(&t),
        }
    }

    /// Is `d` an object type that contains itself (always boxed; its values can form cycles)?
    pub(super) fn recursive_object(&self, d: DefId) -> bool {
        self.boxing.recursive.contains(&d)
    }

    /// Is `t` an array or object type stored as a counted box (a `Ptr` to its inline value)?
    pub(super) fn boxed(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.boxing.recursive.contains(d) => true,
            _ => self.boxing.boxes.contains(&t),
        }
    }

    /// The inline layout of a boxed type's value (what the box holds).
    pub(super) fn payload_ty(&mut self, t: TyId) -> crate::vir::Ty {
        match self.kind(t) {
            TyKind::Array(_) => crate::vir::Ty::Agg(self.array_agg()),
            _ => crate::vir::Ty::Agg(self.value_agg(t)),
        }
    }
}
