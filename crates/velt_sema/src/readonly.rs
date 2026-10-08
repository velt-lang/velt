//! Object types that are one type for lowering (docs/internals/design/shared-models.md):
//! `readonly` fields in object types are a check, not a layout, and a field-only interface's
//! object type is the anonymous object type of its fields.
//!
//! `{ readonly id: number }` and `{ id: number }` are different types while bodies are checked
//! (an assignment through the first is an error), and a value converts between them with an
//! `Upcast` that keeps its identity. Before the program goes to lowering, every object type with
//! readonly fields, and every field-only interface's object type, is replaced by its twin
//! ([`Ctx::readonly_twins`]), so lowering sees one type per layout: the conversion is a no-op
//! and both views share one object.

use std::collections::HashMap;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};
use crate::types::Types;
use crate::visit;

/// Replace every readonly object type in the finished program by its plain twin.
pub(crate) fn erase(cx: &mut Ctx) {
    if cx.readonly_twins.is_empty() && cx.brands.is_empty() {
        return;
    }
    let twins = cx.readonly_twins.clone();
    // A branded type is its primitive from here on (`crate::brands`).
    let mut cache: HashMap<TyId, TyId> = HashMap::new();
    for (&d, &base) in &cx.brands {
        let brand = cx.ty.intern(TyKind::Adt(d, vec![]));
        cache.insert(brand, base);
    }
    let Ctx {
        ty, defs, impls, ..
    } = cx;
    let mut e = |t: TyId| erase_ty(ty, &twins, &mut cache, t);
    // A twin must know a field is assigned if its readonly view's is (it never is today).
    let assigned: Vec<(DefId, bool)> = twins
        .iter()
        .map(|(ro, plain)| {
            let a = matches!(&defs[ro.0 as usize], Some(Def::Adt(a)) if a.assigned);
            (plain.0, a)
        })
        .collect();
    for (plain, a) in assigned {
        if let Some(Def::Adt(d)) = &mut defs[plain.0 as usize] {
            d.assigned |= a;
        }
    }
    for def in defs.iter_mut().flatten() {
        match def {
            Def::Fn(f) => {
                for p in &mut f.params {
                    p.ty = e(p.ty);
                }
                f.ret = e(f.ret);
                f.self_ty = f.self_ty.map(&mut e);
                f.throws = f.throws.map(&mut e);
                for l in &mut f.body.locals {
                    l.ty = e(l.ty);
                }
                visit::map_block_types(&mut f.body.block, &mut e);
            }
            Def::ExternFn(f) => {
                for p in &mut f.params {
                    *p = e(*p);
                }
                f.ret = e(f.ret);
            }
            Def::Adt(a) => {
                for f in &mut a.fields {
                    f.ty = e(f.ty);
                    if let Some(d) = &mut f.default {
                        visit::map_expr_types(d, &mut e);
                    }
                }
                a.base = a.base.map(&mut e);
            }
            Def::Enum(en) => {
                for v in &mut en.variants {
                    for p in &mut v.payload {
                        *p = e(*p);
                    }
                }
            }
            Def::Global(g) => {
                g.ty = e(g.ty);
                visit::map_expr_types(&mut g.init, &mut e);
            }
            Def::Interface(i) => {
                for f in &mut i.fields {
                    f.ty = e(f.ty);
                }
            }
        }
    }
    for imp in impls.iter_mut() {
        imp.ty = e(imp.ty);
        for a in &mut imp.iface_args {
            *a = e(*a);
        }
    }
}

/// `t` with every readonly object type replaced by its twin, at any depth.
pub(crate) fn erase_ty(
    ty: &mut Types,
    twins: &HashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    cache: &mut HashMap<TyId, TyId>,
    t: TyId,
) -> TyId {
    if let Some(&r) = cache.get(&t) {
        return r;
    }
    let sub =
        |ty: &mut Types, cache: &mut HashMap<TyId, TyId>, x: TyId| erase_ty(ty, twins, cache, x);
    let k = ty.kind(t).clone();
    let nk = match k {
        TyKind::Adt(d, args) => {
            let args: Vec<TyId> = args.iter().map(|a| sub(ty, cache, *a)).collect();
            match twins.get(&d) {
                None => TyKind::Adt(d, args),
                Some((twin, None)) => TyKind::Adt(*twin, args),
                Some((twin, Some(template))) => {
                    let args = template
                        .iter()
                        .map(|p| {
                            let p = ty.subst(*p, &args);
                            sub(ty, cache, p)
                        })
                        .collect();
                    TyKind::Adt(*twin, args)
                }
            }
        }
        TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|a| sub(ty, cache, *a)).collect()),
        TyKind::Array(x) => TyKind::Array(sub(ty, cache, x)),
        TyKind::Map(a, b) => TyKind::Map(sub(ty, cache, a), sub(ty, cache, b)),
        TyKind::Tuple(xs) => TyKind::Tuple(xs.iter().map(|a| sub(ty, cache, *a)).collect()),
        TyKind::Option(x) => TyKind::Option(sub(ty, cache, x)),
        TyKind::Result(a, b) => TyKind::Result(sub(ty, cache, a), sub(ty, cache, b)),
        TyKind::Promise(v, x) => TyKind::Promise(sub(ty, cache, v), sub(ty, cache, x)),
        TyKind::Shared(x) => TyKind::Shared(sub(ty, cache, x)),
        TyKind::FnPtr {
            params,
            ret,
            throws,
        } => TyKind::FnPtr {
            params: params.iter().map(|a| sub(ty, cache, *a)).collect(),
            ret: sub(ty, cache, ret),
            throws: sub(ty, cache, throws),
        },
        _ => {
            cache.insert(t, t);
            return t;
        }
    };
    let r = ty.intern(nk);
    cache.insert(t, r);
    r
}
