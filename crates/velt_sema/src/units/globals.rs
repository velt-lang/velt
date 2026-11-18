//! EXPERIMENT (#2943): the std modules of the globals the program didn't ask for
//! (`CheckOptions::unloaded_globals`). Lazy std loads a global loaded on demand
//! (`std/prelude/global/*`) only when the program's own code asks for it, and with it the module
//! it imports; std's unit has every module. A check by units answers sema's "did std load this
//! global's class?" (`Ctx::prelude_adt`) as lazy std does: no for these.
//!
//! Only a global's own module (its direct import, with the files of that module's folder) is
//! hidden, and only from type lookups: lazy std also loads a hook's module whenever a construct
//! demands that hook, whether or not a global imports it, so hooks (`Ctx::std_def`) stay.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;

/// The std modules the `unloaded` globals import directly, and the modules in the folder of one
/// of those (a module `velt/lang/this_fn` is `std/lang/this_fn/index` and its files), unless a
/// module other than such a global or another of these imports it (the program, a global it
/// asked for, another std module): then lazy std may have it, the cautious answer.
pub(crate) fn hidden(cx: &Ctx, unloaded: &[String]) -> FxHashSet<usize> {
    let mut hidden = FxHashSet::default();
    if unloaded.is_empty() {
        return hidden;
    }
    let n = cx.modules.len();
    let by_path: FxHashMap<&str, usize> =
        (0..n).map(|m| (cx.modules[m].path.as_str(), m)).collect();
    let imports_of = |m: usize| {
        (cx.modules[m].imports.iter()).filter_map(|(_, p)| by_path.get(p.as_str()).copied())
    };
    let globals: FxHashSet<usize> = (unloaded.iter())
        .filter_map(|p| by_path.get(p.as_str()).copied())
        .collect();
    // The globals' own modules, and their folders (`std/lang/this_fn/index` -> `std/lang/this_fn/`).
    let targets: FxHashSet<usize> = globals.iter().flat_map(|&g| imports_of(g)).collect();
    let folders: Vec<String> = (targets.iter())
        .filter_map(|&t| cx.modules[t].path.strip_suffix("/index"))
        .map(|dir| format!("{dir}/"))
        .collect();
    let candidates: FxHashSet<usize> = (0..n)
        .filter(|&m| cx.modules[m].is_std && !globals.contains(&m))
        .filter(|&m| {
            targets.contains(&m) || folders.iter().any(|f| cx.modules[m].path.starts_with(f))
        })
        .collect();
    // Imported from outside (not a global left out, not one of the candidates): stays.
    let mut outside: FxHashSet<usize> = FxHashSet::default();
    for m in 0..n {
        if globals.contains(&m) || candidates.contains(&m) {
            continue;
        }
        outside.extend(imports_of(m).filter(|t| candidates.contains(t)));
    }
    hidden.extend(candidates.into_iter().filter(|m| !outside.contains(m)));
    if std::env::var_os("VELT_TMP_G").is_some() {
        // TEMP-DEBUG (#2943 this_fn in the harness's release runs)
        let names: Vec<&str> = (hidden.iter()).map(|&m| cx.modules[m].path.as_str()).collect();
        let tf = unloaded.iter().any(|g| g.ends_with("/this_fn"));
        eprintln!("TMP-G unloaded={} this_fn_unloaded={tf} hidden={names:?}", unloaded.len());
    }
    hidden
}
