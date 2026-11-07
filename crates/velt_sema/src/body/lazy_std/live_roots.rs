//! The roots of the live set ([`super`]): what is live whatever else the program does, or once
//! it has a value of a kind.

use rustc_hash::FxHashSet;
use velt_common::std_hooks::HOOKS;

use super::{kind_bit, mark, Gate, DYNAMIC, GATE_KINDS};
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DefId, TyId, TyKind};

/// The roots: the program's own definitions (its root module's, whether or not in std), and the
/// free functions lowering looks up by name ([`seed_std`]). Methods are lowering's only where it
/// has a value of their type: live with it.
pub(super) fn seed(cx: &mut Ctx) {
    let from = std::mem::replace(&mut cx.lazy.live.seeded, cx.info.len());
    // Generated code may add methods.
    cx.lazy.live.methods = None;
    // The root module is the program's even in std (a source generator's).
    let program: Vec<bool> = (cx.modules.iter().enumerate())
        .map(|(m, sm)| !sm.is_std || m == cx.root)
        .collect();
    let any_live_type = !cx.lazy.live.adts.is_empty();
    for i in from..cx.info.len() {
        let (module, owner, slot) = match &cx.info[i] {
            DefInfo::Fn(f) => (f.module, f.owner, false),
            // Lowering runs a module slot's initializer whether or not code reads it.
            DefInfo::Global(g) => (g.module, None, g.stored),
            _ => continue,
        };
        let of_live = any_live_type && owner.is_some_and(|o| cx.lazy.live.adts.contains(&o));
        if program[module] || of_live || slot {
            mark(cx, DefId(i as u32));
        }
    }
    seed_std(cx, from);
}

/// The free functions lowering looks up by name, of the definitions from `from` on: the helpers
/// of the prelude, those of the modules it looks into ([`super::super::BY_NAME`]), and the hooks no
/// construct lists, of which some wait for what the program does (`Live::waiting`). Such a
/// function that takes a value only live code can give (an array, a class instance, an `any`) is
/// live once live code has a value of that type ([`gate`]).
fn seed_std(cx: &mut Ctx, from: usize) {
    let mut hooks = FxHashSet::default();
    for h in HOOKS
        .iter()
        .filter(|h| h.lowered && h.constructs.is_empty())
    {
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            hooks.insert(d);
            if h.fallback || super::super::RECORD_HOOKS.contains(&h.qualified().as_str()) {
                cx.lazy.live.waiting.insert(d);
            }
        }
    }
    for i in from..cx.info.len() {
        let d = DefId(i as u32);
        let DefInfo::Fn(f) = &cx.info[i] else {
            continue;
        };
        let m = &cx.modules[f.module];
        // A method or a static method is live with its type; a closure with its function.
        if !m.is_std
            || f.module == cx.root
            || f.owner.is_some()
            || !matches!(f.kind, FnKind::Free | FnKind::Static)
        {
            continue;
        }
        let named = match cx.lazy.modules[f.module].named {
            super::super::Named::All => true,
            super::super::Named::Helpers => super::super::is_helper(&f.name),
            super::super::Named::None => false,
        };
        if named || hooks.contains(&d) {
            match gate(cx, f) {
                gates if gates.is_empty() => _ = mark(cx, d),
                gates => cx.lazy.live.gated.push((d, gates)),
            }
        }
    }
}

/// What a function lowering calls by name takes or gives that only live code has: the
/// classes, structs and interfaces its signature mentions, or failing those, the kinds (an
/// array, a map, an `any`, a function) in it. Lowering calls `stableSort` for an array's
/// `sort()`, `uncaughtHeader` for an `Error` thrown, `anyArr_push` for an `any`, `classObject` to
/// print a class object (its result mentions `ClassMarker`). Empty when it has only numbers,
/// strings and the like, or type parameters: always live. The functions of `velt/sync/epoch` take
/// nothing: lowering calls them for a reference held by an `Atomic`.
fn gate(cx: &Ctx, f: &crate::defs::FnInfo<'_>) -> Vec<Gate> {
    let sig = (f.this.iter().map(|t| t.ty))
        .chain(f.params.iter().map(|p| p.ty))
        .chain([f.ret]);
    let (mut heads, mut kinds, mut seen) = (vec![], vec![], FxHashSet::default());
    let mut todo: Vec<TyId> = sig.collect();
    while let Some(t) = todo.pop() {
        if !seen.insert(t) {
            continue;
        }
        let kind = cx.ty.kind(t);
        match kind {
            TyKind::Adt(d, _) => heads.push(Gate::Adt(*d)),
            TyKind::Dyn(d, _) if cx.dynamic(t).is_none() => heads.push(Gate::Iface(*d)),
            TyKind::Dyn(..) => kinds.push(Gate::Kind(DYNAMIC)),
            k if kind_bit(k) & GATE_KINDS != 0 => kinds.push(Gate::Kind(kind_bit(k))),
            _ => {}
        }
        todo.extend(crate::types::children(kind));
    }
    let module = &cx.modules[f.module].path;
    if module.starts_with(velt_common::std_module!(sync_epoch)) {
        heads.extend(cx.prelude_adt("Atomic").map(Gate::Adt));
    }
    match heads.is_empty() {
        // Lowering calls what mentions an `any` for an `any`, whatever else it takes.
        true if kinds.iter().any(|k| matches!(k, Gate::Kind(DYNAMIC))) => {
            vec![Gate::Kind(DYNAMIC)]
        }
        true => kinds,
        false => heads,
    }
}
