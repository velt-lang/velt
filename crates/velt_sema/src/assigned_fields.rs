//! Which object types have a field assigned anywhere (`Program::assigned_objects`): sharing such
//! a value must keep one object, while sharing an object type that is never changed in place may
//! copy it field by field (semantics stage 2, hir_encodings.md "Sharing").

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, Def, DefId, Expr, ExprKind as E, TyKind};
use crate::visit;

/// The definitions of every type whose field is the target of an assignment in some body.
pub(crate) fn assigned_objects(cx: &mut Ctx) -> FxHashSet<DefId> {
    let mut out = FxHashSet::default();
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                if let Some(d) = written_object(cx, place) {
                    out.insert(d);
                }
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
    }
    // `x[k] = v` in a generic body, at each instantiation (`Intrinsic::KeyWrite`).
    out.extend(cx.type_level.inline.written.iter().copied());
    // Anonymous object types are structural: `{ a: U }` at `U = string` and a written
    // `{ a: string }` are different defs holding the same values (`crate::anon`, and `velt_vir`
    // lays them out as one). Defs with the same field names must therefore agree, or the two
    // forms would be shared differently.
    let mut by_names: FxHashMap<Vec<String>, Vec<DefId>> = FxHashMap::default();
    for (i, info) in cx.info.iter().enumerate() {
        if let DefInfo::Adt(a) = info {
            if a.kind == AdtKind::Anon {
                let names = a.fields.iter().map(|f| f.name.clone()).collect();
                by_names.entry(names).or_default().push(DefId(i as u32));
            }
        }
    }
    for group in by_names.values() {
        if group.iter().any(|d| out.contains(d)) {
            out.extend(group.iter().copied());
        }
    }
    out
}

/// Object type `d` has a field assigned by a setter made after [`assigned_objects`] ran (a
/// method setter of an upcast's implementation, `crate::iface_method_writes`): it joins
/// `Ctx::assigned_objects`, with the anonymous types of its field names (see above).
pub(crate) fn mark_assigned(cx: &mut Ctx, d: DefId) {
    if !cx.assigned_objects.insert(d) {
        return;
    }
    let names = |a: &crate::defs::AdtInfo<'_>| -> Vec<String> {
        a.fields.iter().map(|f| f.name.clone()).collect()
    };
    let Some(DefInfo::Adt(a)) = cx.info.get(d.0 as usize) else {
        return;
    };
    if a.kind != AdtKind::Anon {
        return;
    }
    let mine = names(a);
    let same: Vec<DefId> = (cx.info.iter().enumerate())
        .filter_map(|(i, info)| match info {
            DefInfo::Adt(b) if b.kind == AdtKind::Anon && names(b) == mine => Some(DefId(i as u32)),
            _ => None,
        })
        .collect();
    cx.assigned_objects.extend(same);
}

/// The object type whose field `place` writes: directly (`x.f = …`) or a part of it held inline,
/// such as a tuple's element (`x.t[0] = …`, `x.t[1][0] = …`, also behind `!` or `as`:
/// `x.t![0] = …`), which changes the field's value.
fn written_object(cx: &Ctx, place: &Expr) -> Option<DefId> {
    let place = match &place.kind {
        E::UnwrapSome(x, _) | E::UnwrapVariant { expr: x, .. } | E::Downcast(x) => {
            return written_object(cx, x)
        }
        _ => place,
    };
    let E::Field { base, .. } = &place.kind else {
        return None;
    };
    match cx.ty.kind(base.ty) {
        TyKind::Adt(d, _) => Some(*d),
        TyKind::Tuple(_) => written_object(cx, base),
        _ => None,
    }
}
