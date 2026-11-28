//! The live walk of a check by units ([`super`]; #2943): all of std is checked
//! already, so the walk only finds the definitions lazy std would have checked for the program,
//! and the modules it would have loaded.

use rustc_hash::FxHashSet;

use super::roots::{seed_units, seed_units_named};
use super::{fn_module, mark, Live};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::DefId;

/// The definitions of a check by units, by lazy std's live
/// walk from its own roots ([`super::seed`]: the program's code, the module slots, the helpers
/// lowering looks up by name) and the hooks its constructs demand; all of std is checked already,
/// so nothing is checked here. [`super::demanded`] then gives those hooks only.
pub(crate) fn walk_units_program(cx: &mut Ctx, entry: Option<DefId>) -> FxHashSet<DefId> {
    cx.lazy.live = Live {
        units: true,
        ..Live::default()
    };
    if let Some(e) = entry {
        mark(cx, e);
    }
    for d in cx.module_slots.clone() {
        mark(cx, d);
    }
    let base = cx.unit_base as usize;
    // Lazy std loads part of std ([`units_loaded_modules`]) and seeds the by-name functions of
    // what it loaded; a module live code reaches into is loaded then too (`Ctx::demand_std`).
    let mut loaded = units_loaded_modules(cx);
    cx.lazy.live.units_loaded = loaded.clone();
    seed_units(cx, base, &loaded);
    super::super::units_restart(cx);
    // Lazy std's loop: the walk, the hooks the constructs demand, and those that wait for what
    // the program does (the executor's, the record and number-text hooks, C's exports).
    loop {
        super::super::reach(cx);
        let mut new = vec![false; loaded.len()];
        let mut any = false;
        let live: Vec<DefId> = cx.lazy.live.live.iter().copied().collect();
        for d in live {
            let module = match &cx.info[d.0 as usize] {
                DefInfo::Fn(f) => f.module,
                DefInfo::Global(g) => g.module,
                DefInfo::Adt(a) => a.module,
                _ => continue,
            };
            if module < loaded.len() && !loaded[module] {
                (loaded[module], new[module], any) = (true, true, true);
            }
        }
        if !any {
            break;
        }
        units_loaded_more(cx, &loaded, &new);
        seed_units_named(cx, base, &new);
    }
    // The program's definitions are also the classes, structs and interfaces live code has
    // values of: the passes that build the program (readonly erasure writing the solved error
    // sets into field types, the layouts) read their definitions.
    let mut live = cx.lazy.live.live.clone();
    live.extend(cx.lazy.live.adts.iter().copied());
    live.extend(cx.lazy.live.ifaces.iter().copied());
    if std::env::var_os("VELT_UNITS_REACH_STATS").is_some() {
        eprintln!(
            "velt-units: live walk: {} defs of {}",
            live.len(),
            cx.info.len()
        );
    }
    live
}

/// What lazy std would have checked of std's unit for this program
/// ([`walk_units_program`]), for the analyses whose answer depends on the std code a program
/// has (lazy std's: the code it checked), leaving the check's state as it was.
pub(crate) fn units_live_set(cx: &mut Ctx) -> FxHashSet<DefId> {
    // The passes that ask (generated code's demand, local async, `any`'s members) ask after the
    // bodies are checked: the walk's answer changes only with definitions added since
    // (`Lazy::units_live_memo`).
    let sizes = (cx.defs.len(), cx.info.len());
    if let Some((defs, info, set)) = &cx.lazy.units_live_memo {
        if (*defs, *info) == sizes {
            return set.clone();
        }
    }
    let set = walk_units_live_set(cx);
    cx.lazy.units_live_memo = Some((sizes.0, sizes.1, set.clone()));
    set
}

/// Before an analysis whose answer depends on the std code a program has asks [`walks`]: in the
/// app's stage of a check by units, find that code ([`units_live_set`]).
pub(crate) fn prepare_walk(cx: &mut Ctx) {
    if cx.units_stage == crate::units::Stage::App {
        units_live_set(cx);
    }
}

/// Does an analysis whose answer depends on the std code a program has read definition `d`'s
/// body? In the app's stage of a check by units std's unit holds all of std, of which only what
/// lazy std would have checked is the program's ([`units_live_set`], found by
/// [`prepare_walk`]); otherwise every body the check has.
pub(crate) fn walks(cx: &Ctx, d: DefId) -> bool {
    cx.units_stage != crate::units::Stage::App
        || d.0 >= cx.unit_base
        || (cx.lazy.units_live_memo.as_ref()).is_none_or(|(_, _, live)| live.contains(&d))
}

/// [`units_live_set`]'s walk.
fn walk_units_live_set(cx: &mut Ctx) -> FxHashSet<DefId> {
    let saved = cx.lazy.clone();
    let mut set = walk_units_program(cx, None);
    // A hook waiting for what the program does (a record hook, a fallback) that nothing
    // needed: lazy std leaves it unchecked, so its code is not the program's.
    let needed = |cx: &Ctx, d: &DefId| {
        cx.lazy.record_hooks
            && super::super::RECORD_HOOKS
                .iter()
                .any(|h| cx.prelude_fn(h) == Some(*d))
    };
    let waiting: Vec<DefId> = (cx.lazy.live.waiting.iter())
        .filter(|d| !needed(cx, d))
        .copied()
        .collect();
    for d in waiting {
        set.remove(&d);
    }
    let constructs = cx.lazy.live.present;
    cx.lazy = saved;
    cx.lazy.units_live_constructs = constructs;
    set
}

/// Does the std code a program has in a check by units ([`units_live_set`]) call
/// `catchUnwind`, which demands std's panic path as it does in lazy std ([`super::demanded`])?
pub(crate) fn units_catch(cx: &mut Ctx) -> bool {
    units_live_set(cx);
    cx.lazy.units_live_constructs & velt_common::std_hooks::Construct::CatchUnwind.bit() != 0
}

/// The modules lazy std loads before the check demands more: the program's,
/// the std modules a program module imports directly, and the prelude's (`std/prelude/…`).
pub(crate) fn units_loaded_modules(cx: &Ctx) -> Vec<bool> {
    let mut loaded = vec![false; cx.modules.len()];
    for m in 0..cx.modules.len() {
        let module = &cx.modules[m];
        if module.is_std && m != cx.root {
            loaded[m] |= module.path.starts_with("std/prelude/");
            continue;
        }
        loaded[m] = true;
        for &t in cx
            .import_targets
            .get(m)
            .into_iter()
            .flat_map(|t| t.values())
        {
            if t < loaded.len() {
                loaded[t] = true;
            }
        }
    }
    loaded
}

/// [`walk_units_program`] loaded the `new` modules: the methods there of the live classes and
/// structs that are live with them ([`super::class_methods`]) are live now, and the
/// implementations there are considered again.
fn units_loaded_more(cx: &mut Ctx, loaded: &[bool], new: &[bool]) {
    cx.lazy.live.units_loaded = loaded.to_vec();
    cx.lazy.live.opened = None;
    let adts: Vec<DefId> = cx.lazy.live.adts.iter().copied().collect();
    let mut found = vec![];
    for d in adts {
        let ms = super::class_methods::with_class(cx, d);
        found.extend(ms.into_iter().filter(|&m| {
            fn_module(cx, m).is_some_and(|module| new.get(module).copied().unwrap_or(false))
        }));
    }
    for m in found {
        mark(cx, m);
    }
}
