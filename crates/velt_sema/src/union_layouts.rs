//! Concrete unions that readonly erasure makes one layout (`crate::readonly`): `number | A` and
//! `number | { readonly a: number }` (`A` a field-only interface) are two unions while bodies
//! are checked, and one member list once their members are erased. Each is the first union def
//! of its erased member list from then on, its variants renumbered to that def's, so lowering
//! lays out one type wherever a value of either is stored, passed or read (#1447). A union whose
//! erased members are all one type (`USD | number` with `USD = number & { __brand: "USD" }`) is
//! that type: its variants are the whole value in lowering (velt_vir `Cx::union_variant`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::{Def, DefId, Expr, ExprKind as E, Pat, PatKind, TyId, TyKind};
use crate::readonly::erase_ty;
use crate::types::Types;
use crate::visit::{self, VisitMut};

/// The concrete union defs that are another def from here on, with each variant's index in it.
pub(crate) type Merged = FxHashMap<DefId, (DefId, Vec<u32>)>;

/// What `plan` found: the merged defs, and every concrete union def no type is from here on
/// (merged, of one member, or a type with error-set variables replaced by its solved type),
/// which `crate::anon::concrete_unions` leaves out.
pub(crate) struct Plan {
    pub merged: Merged,
    pub replaced: FxHashSet<DefId>,
}

/// Bound on the rounds that merge unions holding merged unions (`string | (number | A)[]`).
const ROUNDS: u32 = 8;

/// Map the concrete unions of one erased member list onto one def, and those of one erased
/// member onto that member, in `cache` (the erasure's type map, holding the brands), along
/// with the `solved` types (`throws::vars`: a type with error-set variables is its solved
/// type, erased).
pub(crate) fn plan(
    ty: &mut Types,
    twins: &FxHashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    cache: &mut FxHashMap<TyId, TyId>,
    defs: &[Option<Def>],
    solved: &[(TyId, TyId)],
) -> Plan {
    let base = std::mem::take(cache);
    let replaced: FxHashSet<TyId> = solved.iter().map(|(t, _)| *t).collect();
    let unions: Vec<DefId> = (defs.iter().enumerate())
        .filter(|(_, d)| matches!(d, Some(Def::Enum(e)) if e.is_union && e.generics == 0))
        .map(|(i, _)| DefId(i as u32))
        .collect();
    // Each round erases the members with the mappings found so far: merging two unions can
    // make two unions holding them alike.
    let mut mapped: FxHashMap<TyId, TyId> = FxHashMap::default();
    let mut merged = Merged::default();
    for _ in 0..ROUNDS {
        if unions.is_empty() {
            break;
        }
        *cache = seeded(ty, twins, &base, &mapped, solved);
        let mut first: FxHashMap<Vec<TyId>, (DefId, Vec<TyId>)> = FxHashMap::default();
        let mut grew = false;
        for &d in &unions {
            let t = ty.intern(TyKind::Adt(d, vec![]));
            if mapped.contains_key(&t) || replaced.contains(&t) {
                continue;
            }
            let Some(Def::Enum(e)) = &defs[d.0 as usize] else {
                continue;
            };
            let erased: Vec<TyId> = (e.variants.iter())
                .filter_map(|v| v.payload.first().copied())
                .map(|p| erase_ty(ty, twins, cache, p))
                .collect();
            let mut key: Vec<TyId> = vec![];
            for p in &erased {
                if !key.contains(p) {
                    key.push(*p);
                }
            }
            key.sort_by_key(|t| t.0);
            if key.len() == 1 {
                mapped.insert(t, key[0]);
                grew = true;
                continue;
            }
            match first.get(&key) {
                None => {
                    first.insert(key, (d, erased));
                }
                Some((rep, rep_erased)) => {
                    let perm = (erased.iter())
                        .map(|p| rep_erased.iter().position(|q| q == p).unwrap_or(0) as u32)
                        .collect();
                    merged.insert(d, (*rep, perm));
                    let r = ty.intern(TyKind::Adt(*rep, vec![]));
                    mapped.insert(t, r);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    // A def a later round merged (its members held a union merged in that round) may be the
    // def an earlier one was merged into: each is the last def of its chain (a lower def at
    // each step).
    let ds: Vec<DefId> = merged.keys().copied().collect();
    for d in ds {
        while let Some((r2, p2)) = merged.get(&merged[&d].0).cloned() {
            let perm = merged[&d].1.iter().map(|&i| p2[i as usize]).collect();
            merged.insert(d, (r2, perm));
        }
    }
    *cache = seeded(ty, twins, &base, &mapped, solved);
    let replaced = (unions.iter().copied())
        .filter(|d| {
            let t = ty.intern(TyKind::Adt(*d, vec![]));
            mapped.contains_key(&t) || replaced.contains(&t)
        })
        .collect();
    Plan { merged, replaced }
}

/// The erasure's type map `base` with the unions in `mapped` replaced, each by the end of its
/// chain of replacements, and the `solved` types, all erased with these (a member may hold a
/// union replaced later, or a solved type).
fn seeded(
    ty: &mut Types,
    twins: &FxHashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    base: &FxHashMap<TyId, TyId>,
    mapped: &FxHashMap<TyId, TyId>,
    solved: &[(TyId, TyId)],
) -> FxHashMap<TyId, TyId> {
    let mut cache = base.clone();
    for (&t, &v) in mapped {
        let mut v = v;
        for _ in 0..mapped.len() {
            match mapped.get(&v) {
                Some(&w) => v = w,
                None => break,
            }
        }
        cache.insert(t, v);
    }
    for &(t, s) in solved {
        let r = erase_ty(ty, twins, &mut cache, s);
        cache.insert(t, r);
    }
    let ts: Vec<TyId> = mapped.keys().copied().collect();
    for t in ts {
        let v = cache[&t];
        let v = erase_ty(ty, twins, &mut cache, v);
        cache.insert(t, v);
    }
    cache
}

/// Renumber the variants of merged unions (`plan`) in `e`, before its types are erased (an
/// unwrapped variant's union is its operand's type).
pub(crate) fn renumber_expr(e: &mut Expr, ty: &Types, merged: &Merged) {
    if !merged.is_empty() {
        visit::expr(e, &mut Renumber { ty, merged });
    }
}

/// [`renumber_expr`] over a function body.
pub(crate) fn renumber_block(b: &mut crate::hir::Block, ty: &Types, merged: &Merged) {
    if !merged.is_empty() {
        visit::block(b, &mut Renumber { ty, merged });
    }
}

struct Renumber<'a> {
    ty: &'a Types,
    merged: &'a Merged,
}

impl VisitMut for Renumber<'_> {
    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            E::Variant { def, variant, .. } => {
                if let Some((rep, perm)) = self.merged.get(def) {
                    *variant = perm[*variant as usize];
                    *def = *rep;
                }
            }
            E::UnwrapVariant { expr, variant, .. } => {
                if let TyKind::Adt(d, _) = self.ty.kind(expr.ty) {
                    if let Some((_, perm)) = self.merged.get(d) {
                        *variant = perm[*variant as usize];
                    }
                }
            }
            _ => {}
        }
    }

    fn pat(&mut self, p: &mut Pat) {
        if let PatKind::Variant { def, variant, .. } = &mut p.kind {
            if let Some((rep, perm)) = self.merged.get(def) {
                *variant = perm[*variant as usize];
                *def = *rep;
            }
        }
    }
}
