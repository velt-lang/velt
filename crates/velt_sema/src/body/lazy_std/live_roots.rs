//! The roots of the live set ([`super`]): what is live whatever else the program does, or once
//! it has a value of a kind.

use rustc_hash::FxHashSet;
use velt_common::std_hooks::HOOKS;

use super::{mark, Gate};
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{self, DefId, TyId, TyKind};

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
/// construct lists, of which some wait for what the program does (`Live::waiting`, and the hooks
/// of module state, live with a stored global). Such a
/// function that takes a value only live code can give (an array, a class instance, an `any`) is
/// live once live code has a value of that type ([`gate`]).
fn seed_std(cx: &mut Ctx, from: usize) {
    let hooks = seed_hooks(cx);
    for i in from..cx.info.len() {
        seed_named(cx, DefId(i as u32), &hooks);
    }
}

/// [`seed`] for a check by units, without reading every
/// definition (each read decodes it): the program's from `base` on, and of std, the top-level functions of the modules lowering looks into by name and
/// the hooks. The module slots are marked by the caller.
pub(super) fn seed_units(cx: &mut Ctx, base: usize, loaded: &[bool]) {
    cx.lazy.live.seeded = cx.info.len();
    cx.lazy.live.methods = None;
    for i in base..cx.info.len() {
        let module = match &cx.info[i] {
            DefInfo::Fn(f) => f.module,
            DefInfo::Global(g) => g.module,
            _ => continue,
        };
        if !cx.modules[module].is_std || module == cx.root {
            mark(cx, DefId(i as u32));
        }
    }
    seed_units_named(cx, base, loaded);
}

/// [`seed_units`]' std part: the functions lowering looks up by name in the `loaded` modules
/// (lazy std seeds only the modules it has loaded), and of those modules, the hooks no
/// construct lists.
pub(super) fn seed_units_named(cx: &mut Ctx, base: usize, loaded: &[bool]) {
    let hooks = seed_hooks(cx);
    // A hook too only once its module is loaded: lazy std finds it by name in what it loaded.
    let module_of = |cx: &Ctx, d: DefId| match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => Some(f.module),
        DefInfo::Global(g) => Some(g.module),
        _ => None,
    };
    let mut named: Vec<DefId> = (hooks.iter().copied())
        .filter(|d| module_of(cx, *d).is_some_and(|m| loaded.get(m).copied().unwrap_or(false)))
        .collect();
    for m in 0..cx.modules.len().min(cx.scopes.len()) {
        let module = &cx.modules[m];
        if !module.is_std || m == cx.root || super::super::named_module(&module.path).is_none() {
            continue;
        }
        if !loaded.get(m).copied().unwrap_or(false) {
            continue;
        }
        named.extend(cx.scopes[m].items.values().filter_map(|item| match item {
            crate::ctx::Item::Def(d) if (d.0 as usize) < base => Some(*d),
            _ => None,
        }));
    }
    named.sort_unstable();
    named.dedup();
    for d in named {
        seed_named(cx, d, &hooks);
    }
}

/// The hooks no construct lists ([`seed_std`]), some waiting for what the program does.
fn seed_hooks(cx: &mut Ctx) -> FxHashSet<DefId> {
    let mut hooks = FxHashSet::default();
    // Not module_blocks.velt's (`super::super::module_hooks`): they are live once the program has
    // module state, also where std is loaded up front and holds their module in every program.
    let blocks = velt_common::std_hooks::BLOCKS_MODULE_ENTRY;
    for h in HOOKS
        .iter()
        .filter(|h| h.lowered && h.constructs.is_empty() && h.module_entry != blocks)
    {
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            // A process static (#2779): `velt_main` hands the runtime its address in every
            // program, so it is checked in every program. Checked, not live: its `Atomic<u64>`
            // is no construct of the program's (no integer hook for it).
            if matches!(h.sig, velt_common::hook_sig::Sig::ProcessStatic) {
                super::super::check_found(cx, d);
                continue;
            }
            hooks.insert(d);
            if h.fallback || super::super::RECORD_HOOKS.contains(&h.qualified().as_str()) {
                cx.lazy.live.waiting.insert(d);
            }
        }
    }
    hooks
}

/// Std function `d`, if lowering looks it up by name (a helper of a module it looks into, or one
/// of `hooks`): live, or once live code has a value it takes ([`gate`]).
fn seed_named(cx: &mut Ctx, d: DefId, hooks: &FxHashSet<DefId>) {
    let DefInfo::Fn(f) = &cx.info[d.0 as usize] else {
        return;
    };
    let m = &cx.modules[f.module];
    // A method or a static method is live with its type; a closure with its function.
    if !m.is_std
        || f.module == cx.root
        || f.owner.is_some()
        || !matches!(f.kind, FnKind::Free | FnKind::Static)
    {
        return;
    }
    // A check by units has no lazy module table (`live::walk_units_program`).
    let named = match cx.lazy.modules.get(f.module).map(|m| &m.named) {
        Some(super::super::Named::All) => true,
        Some(super::super::Named::Helpers) => super::super::is_helper(&f.name),
        Some(super::super::Named::None) => false,
        None => match super::super::named_module(&m.path) {
            Some(true) => true,
            Some(false) => super::super::is_helper(&format!("{}::{}", m.path, f.name)),
            None => false,
        },
    };
    if named || hooks.contains(&d) {
        match gate(cx, f) {
            gates if gates.is_empty() => _ = mark(cx, d),
            gates => cx.lazy.live.gated.push((d, gates)),
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

/// The bit of a kind of type an implementation may be for, other than a class or struct (0 for
/// the others).
pub(super) fn kind_bit(kind: &TyKind) -> u32 {
    match kind {
        TyKind::Int(it) | TyKind::Literal(hir::LitValue::Int(it, _)) => 1 << (*it as u32 % 16),
        TyKind::Float(_) | TyKind::Literal(hir::LitValue::Float(..)) => 1 << 16,
        TyKind::Bool | TyKind::Literal(hir::LitValue::Bool(_)) => 1 << 17,
        TyKind::Str | TyKind::Literal(hir::LitValue::Str(_)) => 1 << 18,
        TyKind::Symbol => 1 << 19,
        TyKind::Simd(_) => 1 << 20,
        TyKind::Array(_) => 1 << 21,
        TyKind::Map(..) => 1 << 22,
        TyKind::Dyn(..) => 1 << 27,
        TyKind::Promise(..) => 1 << 24,
        TyKind::Tuple(_) => 1 << 25,
        TyKind::FnPtr { .. } | TyKind::Closure(_) => 1 << 26,
        _ => 0,
    }
}

/// The kinds of [`kind_bit`] that gate a root ([`gate`]): values lowering does not make up.
const GATE_KINDS: u32 = (1 << 21) | (1 << 22) | DYNAMIC | (1 << 24) | (1 << 25) | (1 << 26);

/// The bit of a dynamic type (`any`, `unknown`, `object`, `Function`) among [`kind_bit`]'s.
pub(super) const DYNAMIC: u32 = 1 << 23;
