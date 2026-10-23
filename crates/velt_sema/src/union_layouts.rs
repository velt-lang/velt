//! Concrete unions that readonly erasure makes one layout (`crate::readonly`): `number | A` and
//! `number | { readonly a: number }` (`A` a field-only interface) are two unions while bodies
//! are checked, and one member list once their members are erased. Each is the first union def
//! of its erased member list from then on, its variants renumbered to that def's, so lowering
//! lays out one type wherever a value of either is stored, passed or read (#1447). A union whose
//! erased members are all one type (`USD | number` with `USD = number & { __brand: "USD" }`) is
//! that type: its variants are the whole value in lowering (velt_vir `Cx::union_variant`). Variants
//! of one union whose payloads erase alike are the first of them, and anonymous object types
//! whose fields erase alike (they hold unions merged here) are the first def of those fields.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::{AdtKind, Def, DefId, Expr, ExprKind as E, Pat, PatKind, TyId, TyKind};
use crate::readonly::erase_ty;
use crate::types::Types;
use crate::visit::{self, VisitMut};

/// The concrete union defs whose variants are renumbered: each with the def its type is from
/// here on (itself when only its variants of one erased payload become the first of them) and
/// each variant's index in that def.
pub(crate) type Merged = FxHashMap<DefId, (DefId, Vec<u32>)>;

/// What `plan` found: the merged defs, and every concrete union def no type is from here on
/// (merged, of one member, or a type with error-set variables replaced by its solved type),
/// which `crate::anon::concrete_unions` leaves out.
pub(crate) struct Plan {
    pub merged: Merged,
    pub replaced: FxHashSet<DefId>,
    /// Anonymous object types alike once their fields are erased (`{ v: number | A }` and
    /// `{ v: number | R }`), each with the def it is from here on.
    pub objects: Vec<(DefId, DefId)>,
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
    let anons: Vec<DefId> = (defs.iter().enumerate())
        .filter(|(i, d)| {
            // Only plain shapes: nothing but their fields tells two of them apart.
            matches!(d, Some(Def::Adt(a)) if a.kind == AdtKind::Anon && a.generics == 0
                && a.base.is_none() && a.vtable.is_empty() && a.ctor.is_none()
                && a.dispose.is_none() && a.to_string.is_none() && a.to_json.is_none()
                && a.inspect.is_none())
                && !twins.contains_key(&DefId(*i as u32))
        })
        .map(|(i, _)| DefId(i as u32))
        .collect();
    let mut objects = vec![];
    // Each round erases the members with the mappings found so far: merging two unions can
    // make two unions holding them alike.
    let mut mapped: FxHashMap<TyId, TyId> = FxHashMap::default();
    for _ in 0..ROUNDS {
        if unions.is_empty() && anons.len() < 2 {
            break;
        }
        *cache = seeded(ty, twins, &base, &mapped, solved);
        let mut first: FxHashMap<Vec<TyId>, DefId> = FxHashMap::default();
        let mut grew = false;
        // Object types whose fields hold unions merged so far are the first def of their
        // erased fields (`crate::anon::concrete_shapes` keeps the first too).
        let mut shapes: FxHashMap<Vec<(&str, TyId, bool, bool)>, DefId> = FxHashMap::default();
        for &d in &anons {
            let t = ty.intern(TyKind::Adt(d, vec![]));
            if mapped.contains_key(&t) {
                continue;
            }
            let Some(Def::Adt(a)) = &defs[d.0 as usize] else {
                continue;
            };
            let key = (a.fields.iter())
                .map(|f| {
                    let ft = erase_ty(ty, twins, cache, f.ty);
                    (f.name.as_str(), ft, f.optional, f.presence)
                })
                .collect();
            match shapes.get(&key) {
                None => {
                    shapes.insert(key, d);
                }
                Some(&rep) => {
                    let r = ty.intern(TyKind::Adt(rep, vec![]));
                    mapped.insert(t, r);
                    objects.push((d, rep));
                    grew = true;
                }
            }
        }
        if grew {
            // The unions below see these objects as one type.
            *cache = seeded(ty, twins, &base, &mapped, solved);
        }
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
                    first.insert(key, d);
                }
                Some(&rep) => {
                    let r = ty.intern(TyKind::Adt(rep, vec![]));
                    mapped.insert(t, r);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    // An object type a later round merged may be the one an earlier one was merged into.
    let to: FxHashMap<DefId, DefId> = objects.iter().copied().collect();
    for (_, rep) in &mut objects {
        while let Some(&r) = to.get(rep) {
            *rep = r;
        }
    }
    *cache = seeded(ty, twins, &base, &mapped, solved);
    let replaced = (unions.iter().copied())
        .filter(|d| {
            let t = ty.intern(TyKind::Adt(*d, vec![]));
            mapped.contains_key(&t) || replaced.contains(&t)
        })
        .collect();
    let merged = renumbering(ty, twins, cache, defs, &unions);
    Plan {
        merged,
        replaced,
        objects,
    }
}

/// Where each variant of the concrete unions goes once the program is erased with `cache`: to
/// the variant of the union def its type is from then on (itself, a def it was merged into, or
/// the solved type of one holding error-set variables) whose payload is the first with its
/// erased type. Defs whose variants all stay put are left out, and so are unions that are one
/// member from then on (their variants are the whole value).
fn renumbering(
    ty: &mut Types,
    twins: &FxHashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    cache: &mut FxHashMap<TyId, TyId>,
    defs: &[Option<Def>],
    unions: &[DefId],
) -> Merged {
    let mut merged = Merged::default();
    for &d in unions {
        let t = ty.intern(TyKind::Adt(d, vec![]));
        let f = erase_ty(ty, twins, cache, t);
        let TyKind::Adt(r, args) = ty.kind(f).clone() else {
            continue;
        };
        let (Some(Def::Enum(e)), Some(Def::Enum(re))) = (&defs[d.0 as usize], &defs[r.0 as usize])
        else {
            continue;
        };
        if !args.is_empty() || !re.is_union {
            continue;
        }
        let payloads = |e: &crate::hir::EnumDef, ty: &mut Types, cache: &mut FxHashMap<_, _>| {
            (e.variants.iter())
                .map(|v| v.payload.first().map(|p| erase_ty(ty, twins, cache, *p)))
                .collect::<Vec<_>>()
        };
        let (mine, theirs) = (payloads(e, ty, cache), payloads(re, ty, cache));
        let perm: Option<Vec<u32>> = (mine.iter())
            .map(|p| theirs.iter().position(|q| q == p).map(|i| i as u32))
            .collect();
        let Some(perm) = perm else {
            continue;
        };
        if r != d || perm.iter().enumerate().any(|(i, &v)| i as u32 != v) {
            merged.insert(d, (r, perm));
        }
    }
    merged
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
