//! Which object types have a field assigned anywhere (`AdtDef::assigned`): sharing such a value
//! must keep one object, while sharing an object type that is never changed in place may copy
//! it field by field (semantics stage 2, hir_encodings.md "Sharing").

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, Def, DefId, Expr, ExprKind as E, TyKind};
use crate::visit;

/// The definitions of every type whose field is the target of an assignment in some body.
pub(crate) fn assigned_objects(cx: &mut Ctx) -> HashSet<DefId> {
    let mut out = HashSet::new();
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                if let E::Field { base, .. } = &place.kind {
                    if let TyKind::Adt(d, _) = cx.ty.kind(base.ty) {
                        out.insert(*d);
                    }
                }
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
    }
    // Anonymous object types are structural: `{ a: U }` at `U = string` and a written
    // `{ a: string }` are different defs holding the same values (`crate::anon`, and `velt_vir`
    // lays them out as one). Defs with the same field names must therefore agree, or the two
    // forms would be shared differently.
    let mut by_names: HashMap<Vec<String>, Vec<DefId>> = HashMap::new();
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
