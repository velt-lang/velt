//! Which closures change what they captured, for the run time (`FnDef::changes_held`). A function
//! in `shared(...)` runs each call on a copy of what it captured, so a change would be lost. Where
//! [`super`] follows the function to `shared(...)` that is refused; where it can't (out of a `Map`,
//! a field, a function's result), the many-threads check panics on this flag (velt_vir
//! glue/many.rs). The walk is [`super::changes`]'s, once per closure, so it only runs for a program
//! that puts a function in `shared(...)`.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::visit;

use super::changes::{callee_changes, changes, in_library, Memo, Root, Roots};

/// Is function `d` a typed array's method that writes its elements through the array's buffer, so
/// that neither its receiver's mode nor its parameter says so?
pub(super) fn writer(cx: &Ctx, d: DefId) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
        return false;
    };
    let name = f.name.rsplit("::").next().unwrap_or(&f.name);
    let Some((class, method)) = name.rsplit_once('.') else {
        return false;
    };
    f.name.contains("typed_array")
        && class.ends_with("Array")
        && matches!(method, "fill" | "copyWithin" | "reverse" | "sort" | "set")
}

/// Set `changes_held` on every closure and class `clone()` (module docs).
pub(crate) fn record_changed_captures(cx: &mut Ctx) {
    if !shares_a_function(cx) {
        return;
    }
    let closures: Vec<DefId> = (0..cx.defs.len())
        .map(|i| DefId(i as u32))
        .filter(|d| matches!(&cx.defs[d.0 as usize], Some(Def::Fn(f)) if !f.captures.is_empty()))
        .collect();
    let lits = std::rc::Rc::new(super::targets::Literals::scan(cx));
    let keyed = super::keyed::keyed_fields(cx, &lits, &closures);
    let fed = std::rc::Rc::new(super::fed::fed_fields(cx, keyed.keys().copied().collect()));
    let fed_sites = std::rc::Rc::new(keyed);
    let memo = || Memo {
        fed: fed.clone(),
        fed_sites: fed_sites.clone(),
        lits: lits.clone(),
        ..Memo::default()
    };
    for c in closures {
        let assigned = cx.try_fn(c).is_some_and(|i| i.assigns_capture);
        let Some(Def::Fn(mut f)) = cx.defs[c.0 as usize].take() else {
            continue;
        };
        let roots: Roots = (f.captures.iter())
            .filter(|k| !f.body.locals[k.inner.0 as usize].name.starts_with('#'))
            .map(|k| {
                let ty = f.body.locals[k.inner.0 as usize].ty;
                (k.inner, (Root { local: k.inner, ty }, true))
            })
            .collect();
        let mut found = vec![];
        if !assigned {
            let mut m = memo();
            changes(cx, &f, &roots, &FxHashMap::default(), 0, &mut m, &mut found);
        }
        f.changes_held = assigned || !found.is_empty();
        cx.defs[c.0 as usize] = Some(Def::Fn(f));
    }
    let clones: Vec<(DefId, TyId)> = (0..cx.defs.len())
        .map(|i| DefId(i as u32))
        .filter_map(|d| match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) if f.name.ends_with(".clone") && f.self_ty.is_some() => {
                f.self_ty.map(|t| (d, t))
            }
            _ => None,
        })
        .collect();
    for (m, this) in clones {
        let changes = !in_library(cx, m) || {
            let mut mm = memo();
            callee_changes(cx, m, 0, (true, this), 1, &mut mm).is_some()
        };
        if let Some(Def::Fn(f)) = &mut cx.defs[m.0 as usize] {
            f.changes_held = changes;
        }
    }
}

/// Does the program put a value that may hold a function in `shared(...)`, outside a lock?
fn shares_a_function(cx: &mut Ctx) -> bool {
    let mut tys: Vec<TyId> = vec![];
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: crate::hir::Callee::Intrinsic(crate::hir::Intrinsic::SharedNew),
                args,
            } = &e.kind
            {
                tys.extend(args.iter().map(|a| a.ty));
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
    }
    let mut seen = FxHashSet::default();
    tys.into_iter().any(|t| may_hold_fn(cx, t, &mut seen))
}

fn may_hold_fn(cx: &mut Ctx, t: TyId, seen: &mut FxHashSet<TyId>) -> bool {
    if !seen.insert(t) {
        return false;
    }
    match cx.ty.kind(t).clone() {
        TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Dyn(..) | TyKind::Param(_) => true,
        TyKind::Shared(_) => false,
        TyKind::Adt(d, _) if Some(d) == cx.mutex_ty() || Some(d) == cx.rwlock_ty() => false,
        k => cx.parts_of(k).into_iter().any(|p| may_hold_fn(cx, p, seen)),
    }
}
