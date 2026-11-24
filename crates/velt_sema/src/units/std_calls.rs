//! Std's calls of the methods a program's overrides gave a slot (#3684). Std's bodies are checked
//! before the program exists (`crate::units`), so where std calls a method that had no slot then
//! (`new Map(entries)` calling `this.set`, `Error`'s `toString` reading `this.name`), the call is
//! direct. Once the program adds the slot (`collect::program_slots`) and the freeze lifts
//! (`raised::apply`), such a call dispatches through it, as a whole-program check had made it:
//! `Callee::Def` becomes `Callee::Virtual`. It stays direct when it can't reach the program's
//! override: a `super.m()` call, a receiver just constructed (`new C().m()`), and a receiver
//! whose class no overriding program class extends. Without a program slot nothing changes.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as H};

/// Makes std's direct calls of the methods in `added` (origin class, method key: the slots the
/// program's overrides added) dispatch where a program override can run.
pub(crate) fn redispatch(cx: &mut Ctx, added: &[(DefId, String)]) {
    if added.is_empty() {
        return;
    }
    let targets = targets(cx, added);
    if targets.is_empty() {
        return;
    }
    let report = std::env::var_os("VELT_UNITS_RAISED").is_some();
    let std_fns: Vec<DefId> = (0..cx.defs.len() as u32)
        .map(DefId)
        .filter(|&d| matches!(cx.defs[d.0 as usize], Some(Def::Fn(_))))
        .filter(|&d| cx.try_fn(d).is_some_and(|f| cx.scopes[f.module].is_std))
        .collect();
    for d in std_fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut changed = vec![];
        crate::visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let Some((slot, m)) = dispatches(cx, &targets, e) {
                if let H::Call { callee, .. } = &mut e.kind {
                    *callee = Callee::Virtual { slot };
                    changed.push(m);
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        if report {
            for m in changed {
                let (at, of) = (&cx.fn_info(d).name, &cx.fn_info(m).name);
                eprintln!("velt-units-raised: dispatch {at} -> {of}");
            }
        }
    }
}

/// (std class, the method a direct call on it runs) to the slot the program added, for every
/// std class that an overriding program class extends.
fn targets(cx: &Ctx, added: &[(DefId, String)]) -> FxHashMap<(DefId, DefId), u32> {
    let keys: FxHashSet<&str> = added.iter().map(|(_, k)| k.as_str()).collect();
    let mut out = FxHashMap::default();
    for p in 0..cx.info.len() as u32 {
        let p = DefId(p);
        let Some(a) = cx
            .adt(p)
            .filter(|a| a.decl.is_some() && !cx.scopes[a.module].is_std)
        else {
            continue;
        };
        // The keys program class `p` overrides with a method of its own.
        let own: Vec<&str> = (a.methods.iter())
            .filter(|(k, m)| {
                keys.contains(k.as_str()) && !m.is_static && cx.fn_info(m.def).owner == Some(p)
            })
            .map(|(k, _)| k.as_str())
            .collect();
        let mut at = crate::collect::base_class(cx, p);
        let mut seen = 0;
        while let (Some(c), true) = (at, seen < 64) {
            if cx.declared_in_std(c) {
                for k in &own {
                    let Some(slot) = cx.vslot(c, k) else { continue };
                    if let Some(&m) = cx.vtable_of(c).and_then(|v| v.get(slot as usize)) {
                        out.insert((c, m), slot);
                    }
                }
            }
            at = crate::collect::base_class(cx, c);
            seen += 1;
        }
    }
    out
}

/// The slot and method of `e` when it is a direct call of std that should dispatch.
fn dispatches(
    cx: &Ctx,
    targets: &FxHashMap<(DefId, DefId), u32>,
    e: &Expr,
) -> Option<(u32, DefId)> {
    let H::Call {
        callee: Callee::Def(m, _),
        args,
    } = &e.kind
    else {
        return None;
    };
    if cx.super_calls.contains(&e.span) {
        return None;
    }
    // The receiver before its upcast to the method's class: its static class decides.
    let recv = args.first()?;
    let inner = match &recv.kind {
        H::Upcast(inner) => &**inner,
        _ => recv,
    };
    if matches!(inner.kind, H::New { .. }) {
        return None;
    }
    let (c, _) = cx.class_of(inner.ty)?;
    let slot = *targets.get(&(c, *m))?;
    Some((slot, *m))
}
