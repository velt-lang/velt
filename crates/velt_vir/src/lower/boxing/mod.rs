//! Counted types (docs/internals/design/semantics.md "Which types are counted"): which concrete types
//! carry a reference count because the program shares their values.
//!
//! Lowering records *facts* while it builds functions — every share of a value of type `T`
//! ([`Cx::note_share`]) and every borrow of a place reached through a value of type `C` that
//! has to stay alive while user code runs ([`Cx::note_projection`], see `stabilize.rs`).
//! [`Cx::close_boxing`] turns them into the next [`Boxing`]; `lower_program` repeats lowering
//! until the set is stable. Programs that share nothing lower once, with today's
//! representation everywhere.

mod cycles;
mod cycles_keys;
mod cycles_scc;
mod cycles_sites;
mod kinds;
mod locks;
mod module_reach;
mod notes;
mod replay;
mod seed;
mod transfers;
mod tuples;
mod weak;

use crate::lower::fresh_fns::FnKey;
use crate::vir::FuncId;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::Cx;

pub(super) use cycles_keys::CycleKey;
pub(super) use kinds::ShareKind;
pub(super) use seed::without_seed;
pub use seed::CountedSeed;

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
    /// The program compares function values and makes a closure wrapping another function
    /// value to fit a function type: wrappers lead to the function they wrap
    /// (closure/fn_wrappers.rs).
    pub(super) fn_wrappers: bool,
    /// The program holds a generic closure as `unknown` (glue/vtable.rs `ClosureAny`): every
    /// function's `any` table has the slots that carry its table back.
    pub(super) generic_any: bool,
    /// The program writes a member through `any` (`AnySet` / `AnySetIndex`): the `any` tables
    /// have the write slots (any_set.rs).
    pub(super) any_writes: bool,
    /// Code built in a program sema gave no member slots (`hir::Program::dyn_members`) reads or
    /// calls a member through `any`: a std hook a generic body reaches at a dynamic type
    /// argument (`a == b` with `T = unknown`, #2578). The `any` tables have the member slots,
    /// filled with fields only (any_member.rs `any_member_entries`).
    pub(super) any_members: bool,
    /// The program writes through `any` and holds functions there: a function may have
    /// properties (any_fn_props.rs), a declared function's are module state the runtime keeps,
    /// and every `with`, `read` and `write` takes the module lock first (velt_rt
    /// `velt_rt_module_with`), since a callback may reach them.
    pub(super) fn_props: bool,
    /// The program tests `key in x` on an `any` (`AnyHas`): the `any` tables have the `has`
    /// slot (any_has.rs).
    pub(super) any_has: bool,
    /// A function in `shared(...)` is made or called: only then do environments' transfer entries
    /// answer for a call's copy and the check's entries have a strict walk (glue/transfer_env.rs,
    /// glue/many.rs).
    pub(super) shared_fns: bool,
    /// The prelude deletes a property of an `any` that is not a proxy (`AnyDelete`): the `any`
    /// tables have the delete slot (any_delete.rs).
    pub(super) any_delete: bool,
    /// The program lists the keys of an `any` (`AnyKeys`): the `any` tables have the keys slot
    /// (any_keys.rs).
    pub(super) any_keys: bool,
    /// The program reflects on `any` values (velt/reflect): the `any` tables have the
    /// reflection slot (any_reflect.rs).
    pub(super) any_reflect: bool,
    /// The program converts an `any` to a number (`Intrinsic::AnyToNumber`, an operator on
    /// `any`): every `any` table has the `ToNumber` slot, not only those of primitives
    /// (any_number.rs).
    pub(super) any_number: bool,
    /// The method names called through `any` with a key known at compile time (`""`: a key
    /// known only at run time, which may name any): the built-in methods the `AnyCall` slots
    /// of strings, arrays, numbers and booleans look for (any_builtin.rs).
    pub(super) any_call_keys: BTreeSet<String>,
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
    /// Array views (#1485, array_view.rs): the boxed array types whose header may point at an
    /// `ArrayView`. Empty in a program that converts no `any` to an array type.
    pub(super) array_views: FxHashSet<TyId>,
    /// Record views (#379, record_view.rs): the record types whose values may be views of an
    /// object an `any` held. Empty in a program that converts no `any` to such a record type.
    pub(super) record_views: FxHashSet<TyId>,
    /// The value types of the program's `RwLock`s, the lock set they reach (class hierarchies
    /// by root, other counted types), and those whose `read` is exclusive (locks.rs).
    pub(super) lock_values: FxHashSet<TyId>,
    pub(super) lock_roots: FxHashSet<DefId>,
    pub(super) lock_boxes: FxHashSet<TyId>,
    pub(super) lock_exclusive: FxHashSet<TyId>,
    /// Code that runs in a `read` callback calls a function value (or another function through
    /// a pointer): every type a shared read reaches is counted (locks.rs `count_reached`).
    pub(super) read_indirect: bool,
}

impl Boxing {
    /// The sets of types (not of class hierarchies) the counted types are made of (shape/).
    pub(super) fn ty_sets(&self) -> [&FxHashSet<TyId>; 12] {
        [
            &self.boxes,
            &self.interior,
            &self.identity_dyns,
            &self.transferred,
            &self.weak,
            &self.dyn_types,
            &self.proxy_types,
            &self.array_views,
            &self.record_views,
            &self.lock_values,
            &self.lock_boxes,
            &self.lock_exclusive,
        ]
    }

    /// The counted types before any share is observed: the recursive object types, and the
    /// object types sema relies on being counted (`hir::Program::counted_objects`).
    /// Do `self` and `next` differ at most in the names called through `any`?
    pub(in crate::lower) fn same_but_call_keys(&self, next: &Boxing) -> bool {
        let mut a = self.clone();
        a.any_call_keys.clone_from(&next.any_call_keys);
        a == *next
    }

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
            any_call_keys: hir.any_call_names.iter().cloned().collect(),
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
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Facts {
    /// Types of shared values.
    shares: FxHashSet<TyId>,
    /// `(container, value)`: a borrowed place of type `value` reached through a `container`.
    projections: FxHashSet<(TyId, TyId)>,
    /// Object types compared by identity (`==`, same.rs).
    identity: FxHashSet<TyId>,
    /// `any` values compared by identity (`Boxing::any_identity`).
    pub(in crate::lower) any_identity: bool,
    /// Interface upcasts made, `(from, to)` (upcast.rs): `from` values are compared by
    /// identity when `to` values are.
    upcasts: FxHashSet<(TyId, TyId)>,
    /// Value types a callee may change in place while it borrows them out of an array other
    /// references reach (stabilize.rs): counted, so the borrow can share the value itself.
    identity_borrows: FxHashSet<TyId>,
    /// Tuple types written in place (`t[0] = v`; tuples.rs).
    tuple_writes: FxHashSet<TyId>,
    /// Types of values copied out of a place that keeps its own (tuples.rs).
    tuple_copies: FxHashSet<TyId>,
    /// Per function: the tuple types it writes in place, and the types of the `Copy`
    /// parameters it only reads (tuples.rs).
    fn_tuple_writes: FxHashMap<FuncId, Vec<TyId>>,
    fn_read_params: FxHashMap<FuncId, Vec<TyId>>,
    /// Types of values transferred to another thread as a whole (transfers.rs).
    transfers: FxHashSet<TyId>,
    /// Function values are compared (or hashed) somewhere.
    fn_compared: bool,
    /// A function wrapper is made (`Boxing::fn_wrappers`).
    fn_wrapper: bool,
    /// The types of the function values compared (fresh_fns.rs).
    pub(in crate::lower) fn_compared_keys: FxHashSet<FnKey>,
    /// The generic function types whose values are held as `unknown` (fresh_fns.rs).
    pub(in crate::lower) generic_any_keys: FxHashSet<FnKey>,
    /// The conversions of function values between function types (fresh_fns.rs).
    pub(in crate::lower) fn_edges: FxHashSet<(FnKey, FnKey)>,
    /// The fresh closures made (fresh_fns.rs).
    pub(in crate::lower) fresh_made: FxHashSet<(velt_sema::hir::DefId, FnKey, bool)>,
    /// A generic closure is held as `unknown` (`Boxing::generic_any`).
    pub(super) generic_any: bool,
    /// A member is written through `any`.
    any_set: bool,
    /// A member slot is called in a program without them (`Boxing::any_members`).
    any_member_call: bool,
    /// `key in x` is tested on an `any`.
    any_has: bool,
    /// A function in `shared(...)` is made or called: only then do environments' transfer entries
    /// answer for a call's copy and the check's entries have a strict walk (glue/transfer_env.rs,
    /// glue/many.rs).
    shared_fns: bool,
    /// A property of an `any` is deleted.
    any_delete: bool,
    /// `Object.keys` of an `any` is listed.
    any_keys: bool,
    /// An `any` is reflected on.
    any_reflect: bool,
    /// An `any` is converted to a number.
    any_number: bool,
    /// Method names called through `any`.
    any_call_keys: BTreeSet<String>,
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
    /// Array types an `any` is converted to in the program's own code (array_view.rs).
    pub(super) array_view_targets: FxHashSet<TyId>,
    /// Record types an `any` is converted to with `any` values in the program's own code
    /// (record_view.rs).
    pub(super) record_view_targets: FxHashSet<TyId>,
    /// A share was lowered as a placeholder because its type was not counted yet: the output
    /// of this pass must not be used.
    pub(super) unmet: bool,
    /// Value types of the `RwLock`s made or locked (locks.rs).
    pub(super) lock_values: FxHashSet<TyId>,
    /// Code in read mode called a function through a pointer (`Boxing::read_indirect`).
    pub(super) read_indirect: bool,
    /// Closure environments built, `(key, closure, type arguments)`: the key is the function
    /// type the closure was made at (cycles_keys.rs; `None`: a generic closure's own type).
    pub(super) closure_envs: FxHashSet<(Option<CycleKey>, DefId, Vec<TyId>)>,
    /// `(interface, concrete type)` of the implementation tables built (cycles.rs).
    pub(super) iface_impls: FxHashSet<(DefId, TyId)>,
    /// Where values of each type are made (`new`, literals, closures), recorded only for
    /// `VELT_DEBUG_COUNTED` (cycles_sites.rs).
    pub(super) alloc_sites: FxHashSet<(TyId, velt_common::Span)>,
    /// Generator objects made, `(generator class type, generator function, type arguments)`:
    /// the object holds the function's state after its fields (cycles.rs).
    pub(super) gen_frames: FxHashSet<(TyId, DefId, Vec<TyId>)>,
}

impl Cx<'_> {
    /// The counted types implied by this pass's facts (a superset of the current ones).
    pub(super) fn close_boxing(&mut self) -> Boxing {
        let mut next = self.boxing.clone();
        self.share_from.clear();
        self.shared_locks.clear();
        // A function that may get properties through `any` needs an identity to carry them
        // (any_fn_props.rs): in a program that writes through `any` and holds functions there.
        let fn_props = self.facts.any_set
            && (self.facts.dyn_types.iter())
                .any(|t| matches!(self.kind(*t), TyKind::FnPtr { .. } | TyKind::Closure(_)));
        next.fn_identity |= self.facts.fn_compared || fn_props;
        next.fn_wrappers |= next.fn_identity && self.facts.fn_wrapper;
        next.fn_props |= fn_props;
        next.generic_any |= self.facts.generic_any;
        next.any_identity |= self.facts.any_identity;
        next.any_writes |= self.facts.any_set;
        next.any_members |= self.facts.any_member_call;
        next.any_has |= self.facts.any_has;
        next.shared_fns |= self.facts.shared_fns;
        next.any_delete |= self.facts.any_delete;
        next.any_keys |= self.facts.any_keys;
        next.any_reflect |= self.facts.any_reflect;
        next.any_number |= self.facts.any_number;
        next.any_call_keys
            .extend(self.facts.any_call_keys.iter().cloned());
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
        self.close_tuples(&mut next);
        self.close_proxies(&mut next);
        self.close_array_views(&mut next);
        self.close_record_views(&mut next);
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
                self.close_locks(&mut next);
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
                // One thread at a time has its value, and a copy would be a second lock:
                // `counted_lock_diagnostics`.
                _ if self.is_lock(t) => {
                    self.shared_locks.insert(t);
                }
                _ => {
                    next.boxes.insert(t);
                }
            },
            ShareKind::Value => {
                // Where each part's share comes from: the shared value that holds it.
                let from = self.share_from.get(&t).copied().unwrap_or(t);
                for p in self.part_types(t) {
                    self.share_from.entry(p).or_insert(from);
                    work.push(p);
                }
            }
            _ => {}
        }
    }

    /// `counted` against a candidate set.
    pub(super) fn counted_in(&self, b: &Boxing, t: TyId) -> bool {
        let t = self.ph_rep(t);
        match self.raw_kind(t) {
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
        let t = self.ph_rep(t);
        match self.raw_kind(t) {
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

impl Boxing {
    /// The fields that differ from `next`, each with its value there and here: its number of
    /// entries for a set, or the flag (`VELT_LOWER_PROFILE`).
    pub(super) fn changes(&self, next: &Boxing) -> String {
        let mut out = vec![];
        macro_rules! field {
            ($($f:ident),*) => {$(
                if self.$f != next.$f {
                    let size = |d: String| match d.as_str() {
                        "{}" | "[]" | "None" => "0".to_string(),
                        _ if d.starts_with(['{', '[']) => (d.matches(", ").count() + 1).to_string(),
                        _ => d,
                    };
                    out.push(format!("{} {}->{}", stringify!($f), size(format!("{:?}", self.$f)), size(format!("{:?}", next.$f))));
                }
            )*};
        }
        field!(
            roots,
            boxes,
            interior,
            identity_dyns,
            any_identity,
            fn_identity,
            fn_wrappers,
            generic_any,
            any_writes,
            any_members,
            fn_props,
            any_has,
            any_delete,
            any_keys,
            any_reflect,
            any_number,
            any_call_keys,
            recursive,
            transferred,
            weak_roots,
            weak,
            weak_object,
            dyn_types,
            dyn_to_any,
            proxy_roots,
            proxy_types,
            proxy_class,
            proxy_traced,
            array_views,
            record_views,
            lock_values,
            lock_roots,
            lock_boxes,
            lock_exclusive,
            read_indirect
        );
        out.join(", ")
    }
}
