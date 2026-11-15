//! The weak maps a program makes (`implicit`'s hooks of weak entries).

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, TyId, TyKind};

/// The key and value types of every `new WeakMap<K, V>()` in the program (8), also of a
/// subclass (`new Cache()` with `class Cache extends WeakMap<K, V>`); a key `None` for one that
/// mentions a type parameter.
pub(crate) fn weak_entries(cx: &mut Ctx) -> Vec<(Option<TyId>, TyId)> {
    struct Find<'a, 'm> {
        cx: &'a Ctx<'m>,
        map: DefId,
        /// The class types made with `new` that are a weak map or extend one.
        made: Vec<TyId>,
    }
    impl crate::visit_ref::Visit for Find<'_, '_> {
        fn expr(&mut self, e: &Expr) {
            if let E::New { def, .. } = &e.kind {
                if self.cx.class_extends(*def, self.map) && !self.made.contains(&e.ty) {
                    self.made.push(e.ty);
                }
            }
        }
    }
    let Some(map) = cx.prelude_adt("WeakMap") else {
        return vec![];
    };
    // A finished unit's are its summary (`Ctx::weak_made`, `crate::units`), when its check
    // walked them: the unit's own bodies are walked, after it.
    let kept = match cx.units_stage {
        crate::units::Stage::App => cx.weak_made.clone(),
        _ => None,
    };
    let from = if kept.is_some() {
        cx.unit_base as usize
    } else {
        0
    };
    let mut f = Find {
        cx,
        map,
        made: kept.unwrap_or_default(),
    };
    for d in cx.defs.iter().skip(from) {
        if let Some(Def::Fn(fd)) = d {
            crate::visit_ref::block(&fd.body.block, &mut f);
        }
    }
    let made = f.made;
    if cx.units_stage == crate::units::Stage::Std {
        cx.weak_made = Some(made.clone());
    }
    let mut out = vec![];
    for t in made {
        let Some((root, _)) = cx.weak_root(t) else {
            continue;
        };
        if let TyKind::Adt(_, args) = cx.ty.kind(root) {
            if let [k, v] = *args.as_slice() {
                let mut ps = vec![];
                crate::types::collect_params(&cx.ty, k, &mut ps);
                let k = ps.is_empty().then_some(k);
                if !out.contains(&(k, v)) {
                    out.push((k, v));
                }
            }
        }
    }
    out
}
