//! Units in sema (#2943 M1, docs/internals/design/build-architecture.md): with
//! `CheckOptions::units` (`VELT_UNITS` set for `velt check`),
//! std is collected and every std body checked as a unit of its own, before anything of the app;
//! then std's definitions are frozen ([`crate::ctx::DefTable`]) and the app is collected and its
//! bodies checked against them. A write to a std definition while the app is checked is a place
//! where the program still changes std, counted by location (`VELT_UNITS_FREEZE`) and printed at
//! the end of the check.
//!
//! With `VELT_UNITS=1` the passes after the bodies (generated code, ownership, throws) still run
//! once over every definition, with the freeze lifted. With `VELT_UNITS=2` std is a check of its
//! own ([`Stage::Std`]: its rounds and its passes after the bodies), frozen, and each of the
//! app's rounds starts from a copy of it ([`Stage::App`]); the app's passes run with std's copy
//! frozen, and std is thawed only for building the program. With all of std checked, the driver
//! loads every std file (`velt check`'s `with_all_std`) and nothing is checked lazily
//! (`body::lazy_std`).
//!
//! A copy of std's context shares its code generator (`Ctx`'s `Clone`): an app round that
//! generates code writes into the generator std's stage and the other rounds use, as rounds of
//! one check already share the driver's. That is where a leak between rounds would come from.

pub(crate) mod codec;
pub(crate) mod disk;
pub(crate) mod modes;

use crate::ctx::Ctx;

/// Which part of a check by units a context checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// std, then the app, in one context (`VELT_UNITS=1`).
    Both,
    /// std alone, as a check of its own with its rounds (`VELT_UNITS=2`).
    Std,
    /// The app, from a copy of std's frozen unit (`VELT_UNITS=2`).
    App,
}

/// What a finished unit's check keeps for a later unit's: each pass's summary of the unit's
/// bodies, which the later unit's check starts from instead of reading those bodies again.
#[derive(Clone, Default)]
pub struct Summaries {
    /// What each generic function of the unit needs of the types it is given, for `json`'s
    /// check (converted, printed or read without a lock through a type parameter).
    pub(crate) json_needs: crate::json::JsonNeeds,
    /// What each body and initializer of the unit does with module state
    /// (`module_state::check_locked_access`).
    pub(crate) state_facts: crate::module_state::StateFacts,
    /// The modules each module of the unit names in its code (`crate::linked`).
    pub(crate) module_edges: crate::linked::ModuleEdges,
    /// The unit's functions that hand a parameter to a channel, with those parameters
    /// (`ownership::boundary`).
    pub(crate) senders: rustc_hash::FxHashMap<crate::hir::DefId, Vec<usize>>,
    /// The unit's functions whose bodies read a `shared` value (`Ctx::frozen_reads`), and
    /// whether code outside a function does ([`app_reads_shared`]).
    pub(crate) shared_readers: rustc_hash::FxHashSet<crate::hir::DefId>,
    pub(crate) shared_read_outside: bool,
    /// The unit's functions whose modes its inference raised, with the raised modes: the unit
    /// itself holds their declared modes, for the app's bodies (`modes`).
    pub(crate) raised_modes: Vec<modes::Modes>,
}

// The encoding of a finished unit (`codec`): every field, so a new one fails to build.
codec::structs! {
    Summaries {
        json_needs, state_facts, module_edges, senders, shared_readers, shared_read_outside,
        raised_modes,
    }
}

/// Before the app's passes after the bodies (`VELT_UNITS=2`): the program reads a `shared` value
/// (`Ctx::frozen_reads`) when its own code does, or the std code it has
/// (`lazy_std::units_live_set`) does. std's stage reads all of std, so its own answer is not the
/// program's ([`finish_std`] drops it).
pub(crate) fn app_reads_shared(cx: &mut Ctx) {
    if cx.units_stage != Stage::App || cx.frozen_reads {
        return;
    }
    crate::body::lazy_std::prepare_walk(cx);
    let s = &cx.unit_sums;
    cx.frozen_reads = s.shared_read_outside
        || (s.shared_readers.iter()).any(|&d| crate::body::lazy_std::walks(cx, d));
}

/// `check_definitions` ([`crate::analysis`]) by units: std collected and checked, frozen, then
/// the app.
pub(crate) fn check_definitions(cx: &mut Ctx) {
    let app: Vec<usize> = (0..cx.modules.len())
        .filter(|&m| !cx.modules[m].is_std && !cx.is_generated_module(m))
        .collect();
    if cx.units_stage != Stage::App {
        crate::timings::pass("units: std", || {
            crate::collect::collect_std(cx);
            // What a hint may name (`throws::hints`): the definitions collected, before the
            // bodies make closures.
            cx.collected_defs = cx.defs.len();
            crate::decorators::check(cx, 0);
            crate::body::expr::virtual_defaults::build_fill_bodies(cx, 0);
            crate::body::check_bodies(cx);
        });
    }
    if cx.units_stage == Stage::Std {
        modes::declared(cx);
        // The passes after the bodies run for std alone (`crate::analyze`); it is frozen once
        // its rounds are done (`crate::rounds`).
        return;
    }
    if cx.units_stage == Stage::Both {
        cx.freeze_defs();
        #[cfg(test)]
        codec::hook::std_unit(cx);
    }
    crate::timings::pass("units: app", || {
        crate::script_tdz::check(cx);
        let from = crate::collect::collect_app(cx, &app);
        cx.collected_defs = cx.defs.len();
        crate::decorators::check(cx, from.defs);
        crate::global_this::declare(cx);
        crate::body::check_added(cx, from);
        crate::body::finish_bodies(cx, from.defs);
    });
    if cx.units_stage == Stage::App {
        modes::restore(cx);
    }
    if cx.units_stage == Stage::Both {
        cx.thaw_defs();
    }
}

/// End std's stage (`VELT_UNITS=2`): what only std's own bodies and passes read, and the memos
/// built over std's definitions alone, are dropped (the unit does not save them, `codec::unit`),
/// so the app's rounds start from the unit a load from disk gives (`disk`); then freeze it.
pub(crate) fn finish_std(cx: &mut Ctx) {
    cx.thread_local_uses.clear();
    cx.throw_checks.clear();
    cx.loop_roots.clear();
    cx.closure_defs.clear();
    cx.rec = Default::default();
    // std's own check sealed the extended layouts (`collect::extended_layouts`): the app's
    // conversions widen the layouts of its own types, until its check seals them again.
    cx.objects.layouts.sealed = false;
    // Whether std's code read a `shared` is the program's question: its std code, not all of
    // std's (`app_reads_shared`).
    cx.frozen_reads = false;
    // The app's bodies see std's modes as declared, as in a whole-program check (`modes`).
    modes::keep_raised(cx);
    codec::unit::clear_memos(cx);
    cx.freeze_defs();
    cx.unit_base = cx.defs.len() as u32;
}

/// `VELT_UNITS=2`: std checked as a check of its own (its rounds and passes), then
/// the app's rounds from a copy of it.
pub(crate) fn stage2() -> bool {
    std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2")
}

/// The modules of a check by units in the order its contexts hold them (`VELT_UNITS=2`): std's
/// first, which std's unit is checked over alone, then the app's, added to a copy of std's frozen
/// unit. A module of generated code is the app's (`generating::install` adds none to std's
/// unit), also one generated for a std module's declarations (`<generated>/std/…`, which the
/// driver hands in when an earlier build made it): which ones a check gets, and what is in them,
/// depends on the program and on what was built before. A unit over std's own modules alone
/// depends on neither, so a later step can keep it for another program.
pub(crate) struct Order {
    std: Vec<usize>,
    app: Vec<usize>,
    root: usize,
}

impl Order {
    /// The order of `modules` (a load's, the root at `root`).
    pub(crate) fn of(modules: &[crate::SourceModule], root: usize) -> Order {
        let (std, app) = (0..modules.len()).partition(|&m| modules[m].is_std);
        Order { std, app, root }
    }

    /// std's modules, which std's unit is checked over.
    pub(crate) fn std_modules<'m>(
        &self,
        modules: &'m [crate::SourceModule],
    ) -> Vec<&'m crate::SourceModule> {
        self.std.iter().map(|&m| &modules[m]).collect()
    }

    /// Add the app's modules to `cx`, a copy of std's unit; the load's root becomes its root.
    pub(crate) fn add_app<'m>(&self, cx: &mut Ctx<'m>, modules: &'m [crate::SourceModule]) {
        let base = cx.modules.len();
        for &m in &self.app {
            let module = &modules[m];
            cx.modules.push(module);
            cx.scopes.push(crate::ctx::ModuleScope {
                is_std: module.is_std,
                is_library: module.is_std || module.is_dependency,
                ..Default::default()
            });
            cx.lazy.add_module();
        }
        cx.import_targets = crate::collect::exports::import_targets(&cx.modules);
        if let Some(k) = self.app.iter().position(|&m| m == self.root) {
            cx.root = base + k;
        }
    }

    /// With std's modules in the order std's unit has them: `at[k]` is where (among this
    /// order's std modules) the unit's module `k` is (`disk::load`).
    pub(crate) fn reorder_std(self, at: &[usize]) -> Order {
        let std = at.iter().map(|&k| self.std[k]).collect();
        Order { std, ..self }
    }

    /// Where the load's module `m` is in std's unit (`None`: an app module).
    pub(crate) fn in_std(&self, m: usize) -> Option<usize> {
        self.std.iter().position(|&s| s == m)
    }

    /// Where the load's module `m` is once the app's modules were added at `base`.
    pub(crate) fn in_app(&self, m: usize, base: usize) -> Option<usize> {
        self.in_std(m)
            .or_else(|| self.app.iter().position(|&a| a == m).map(|k| base + k))
    }

    /// Where the app's modules start in `cx` (after std's and the generated ones).
    pub(crate) fn app_base(&self, cx: &Ctx) -> usize {
        cx.modules.len() - self.app.len()
    }
}

/// Put the facts the source rewrites found per module (by the load's module indices) in `cx`'s
/// order: the load's module `m` is `at(m)`, and one `at` drops is not in `cx`.
pub(crate) fn remap_module_facts(cx: &mut Ctx, at: &dyn Fn(usize) -> Option<usize>) {
    cx.recovered = std::mem::take(&mut cx.recovered)
        .into_iter()
        .filter_map(|(m, w)| Some((at(m)?, w)))
        .collect();
    let old = std::mem::take(&mut cx.ns_private);
    let mut ns = vec![Default::default(); cx.modules.len()];
    for (m, p) in old.into_iter().enumerate() {
        if let Some(slot) = at(m).and_then(|i| ns.get_mut(i)) {
            *slot = p;
        }
    }
    cx.ns_private = ns;
    for ms in cx.global_namespaces.values_mut() {
        *ms = ms.iter().filter_map(|&m| at(m)).collect();
    }
    cx.global_props.remap(at);
    cx.objects.brands.remap(at);
}
