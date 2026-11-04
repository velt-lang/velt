//! A shared instance's facts (shape/, docs/internals/design/shape-sharing.md "Facts and the
//! counted-types fixpoint"): the entries its body records over placeholder types are taken out
//! of the pass's facts after the body is lowered, and added back once per dictionary with the
//! placeholders replaced by that dictionary's types, so the pass ends with the facts the
//! per-type instances would have recorded. Entries over concrete types and the program-wide
//! flags stay as recorded.

use rustc_hash::{FxHashMap, FxHashSet};
use std::hash::Hash;
use velt_sema::hir::TyId;

use super::Facts;
use crate::lower::fresh_fns::FnKey;
use crate::vir::FuncId;

/// A set of type pairs to add to, and the set to add.
type PairSets<'a> = (&'a mut FxHashSet<(TyId, TyId)>, &'a FxHashSet<(TyId, TyId)>);

/// Moves the entries of `from` that `pick` names into `to`.
fn take<T: Clone + Eq + Hash>(
    from: &mut FxHashSet<T>,
    to: &mut FxHashSet<T>,
    pick: &dyn Fn(&T) -> bool,
) {
    let picked: Vec<T> = from.iter().filter(|x| pick(x)).cloned().collect();
    for x in picked {
        from.remove(&x);
        to.insert(x);
    }
}

fn key_tys(k: &FnKey) -> Vec<TyId> {
    match k {
        FnKey::Fixed(ps, r, _) => ps.iter().copied().chain([*r]).collect(),
        FnKey::Generic(_, ts) => ts.clone(),
    }
}

fn map_key(k: &FnKey, map: &mut dyn FnMut(TyId) -> TyId) -> FnKey {
    match k {
        FnKey::Fixed(ps, r, b) => FnKey::Fixed(ps.iter().map(|&t| map(t)).collect(), map(*r), *b),
        FnKey::Generic(d, ts) => FnKey::Generic(*d, ts.iter().map(|&t| map(t)).collect()),
    }
}

impl Facts {
    /// The entries naming a type `ph` holds, taken out. Entries keyed by function (the
    /// tuple facts of `fn_tuple_writes` / `fn_read_params`) are taken too, and reported by
    /// [`by_function`](Self::by_function): a shared instance stands for several functions
    /// there, so they are not replayed (the scan keeps such bodies per type).
    pub(in crate::lower) fn take_types(&mut self, ph: &dyn Fn(TyId) -> bool) -> Facts {
        let mut out = Facts::default();
        let one = |t: &TyId| ph(*t);
        let two = |p: &(TyId, TyId)| ph(p.0) || ph(p.1);
        let key = |k: &FnKey| key_tys(k).into_iter().any(ph);
        take(&mut self.shares, &mut out.shares, &one);
        take(&mut self.projections, &mut out.projections, &two);
        take(&mut self.identity, &mut out.identity, &one);
        take(&mut self.upcasts, &mut out.upcasts, &two);
        take(&mut self.identity_borrows, &mut out.identity_borrows, &one);
        take(&mut self.tuple_writes, &mut out.tuple_writes, &one);
        take(&mut self.tuple_copies, &mut out.tuple_copies, &one);
        take(&mut self.transfers, &mut out.transfers, &one);
        take(&mut self.fn_compared_keys, &mut out.fn_compared_keys, &key);
        take(&mut self.generic_any_keys, &mut out.generic_any_keys, &key);
        take(&mut self.fn_edges, &mut out.fn_edges, &|p: &(
            FnKey,
            FnKey,
        )| {
            key(&p.0) || key(&p.1)
        });
        take(&mut self.fresh_made, &mut out.fresh_made, &|p: &(
            _,
            FnKey,
            bool,
        )| {
            key(&p.1)
        });
        take(&mut self.captures, &mut out.captures, &one);
        take(&mut self.vtable_types, &mut out.vtable_types, &one);
        take(&mut self.weak_seeds, &mut out.weak_seeds, &one);
        take(&mut self.dyn_types, &mut out.dyn_types, &one);
        take(&mut self.object_types, &mut out.object_types, &one);
        take(&mut self.proxy_targets, &mut out.proxy_targets, &one);
        take(&mut self.proxy_casts, &mut out.proxy_casts, &two);
        take(
            &mut self.array_view_targets,
            &mut out.array_view_targets,
            &one,
        );
        take(&mut self.lock_values, &mut out.lock_values, &one);
        let by_fn = [
            (&mut self.fn_tuple_writes, &mut out.fn_tuple_writes),
            (&mut self.fn_read_params, &mut out.fn_read_params),
        ];
        for (from, to) in by_fn {
            let picked: Vec<FuncId> = (from.iter())
                .filter(|(_, ts)| ts.iter().any(|&t| ph(t)))
                .map(|(f, _)| *f)
                .collect();
            for f in picked {
                if let Some(ts) = from.remove(&f) {
                    to.insert(f, ts);
                }
            }
        }
        out
    }

    /// The shared types (`note_share`) these facts name.
    pub(in crate::lower) fn shared_types(&self) -> &FxHashSet<TyId> {
        &self.shares
    }

    /// Whether [`take_types`](Self::take_types) took facts keyed by function.
    pub(in crate::lower) fn by_function(&self) -> bool {
        !self.fn_read_params.is_empty() || !self.fn_tuple_writes.is_empty()
    }

    /// Every type `other` names, mapped by `map` (the concrete types of one dictionary).
    pub(in crate::lower) fn types_of(other: &Facts) -> Vec<TyId> {
        let mut v: Vec<TyId> = vec![];
        for s in [
            &other.shares,
            &other.identity,
            &other.identity_borrows,
            &other.tuple_writes,
            &other.tuple_copies,
            &other.transfers,
            &other.captures,
            &other.vtable_types,
            &other.weak_seeds,
            &other.dyn_types,
            &other.object_types,
            &other.proxy_targets,
            &other.array_view_targets,
            &other.lock_values,
        ] {
            v.extend(s.iter().copied());
        }
        for s in [&other.projections, &other.upcasts, &other.proxy_casts] {
            v.extend(s.iter().flat_map(|p| [p.0, p.1]));
        }
        for k in (other.fn_compared_keys.iter())
            .chain(&other.generic_any_keys)
            .chain(other.fn_edges.iter().flat_map(|p| [&p.0, &p.1]))
            .chain(other.fresh_made.iter().map(|p| &p.1))
        {
            v.extend(key_tys(k));
        }
        v
    }

    /// `other`'s entries with every type replaced through `map` (built from
    /// [`types_of`](Self::types_of)), added to these facts.
    pub(in crate::lower) fn add_mapped(&mut self, other: &Facts, map: &FxHashMap<TyId, TyId>) {
        let m = |t: &TyId| *map.get(t).unwrap_or(t);
        let mut mf = |t: TyId| m(&t);
        let sets: [(&mut FxHashSet<TyId>, &FxHashSet<TyId>); 14] = [
            (&mut self.shares, &other.shares),
            (&mut self.identity, &other.identity),
            (&mut self.identity_borrows, &other.identity_borrows),
            (&mut self.tuple_writes, &other.tuple_writes),
            (&mut self.tuple_copies, &other.tuple_copies),
            (&mut self.transfers, &other.transfers),
            (&mut self.captures, &other.captures),
            (&mut self.vtable_types, &other.vtable_types),
            (&mut self.weak_seeds, &other.weak_seeds),
            (&mut self.dyn_types, &other.dyn_types),
            (&mut self.object_types, &other.object_types),
            (&mut self.proxy_targets, &other.proxy_targets),
            (&mut self.array_view_targets, &other.array_view_targets),
            (&mut self.lock_values, &other.lock_values),
        ];
        for (to, from) in sets {
            to.extend(from.iter().map(m));
        }
        let pairs: [PairSets; 3] = [
            (&mut self.projections, &other.projections),
            (&mut self.upcasts, &other.upcasts),
            (&mut self.proxy_casts, &other.proxy_casts),
        ];
        for (to, from) in pairs {
            to.extend(from.iter().map(|p| (m(&p.0), m(&p.1))));
        }
        for k in &other.fn_compared_keys {
            self.fn_compared_keys.insert(map_key(k, &mut mf));
        }
        for k in &other.generic_any_keys {
            self.generic_any_keys.insert(map_key(k, &mut mf));
        }
        for (a, b) in &other.fn_edges {
            let e = (map_key(a, &mut mf), map_key(b, &mut mf));
            self.fn_edges.insert(e);
        }
        for (d, k, b) in &other.fresh_made {
            self.fresh_made.insert((*d, map_key(k, &mut mf), *b));
        }
    }
}
