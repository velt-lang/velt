//! Units in sema (#2943 M1, docs/internals/design/build-architecture.md): with
//! `CheckOptions::units` (`VELT_UNITS` set for `velt check`),
//! std is collected and every std body checked as a unit of its own, before anything of the app;
//! then std's definitions are frozen ([`crate::ctx::DefTable`]) and the app is collected and its
//! bodies checked against them. A write to a std definition while the app is checked is a place
//! where the program still changes std, counted by location (`VELT_UNITS_FREEZE`) and printed at
//! the end of the check.
//!
//! This is the first step: the passes after the bodies (generated code, ownership, throws) still
//! run once over every definition, with the freeze lifted. They become per unit next, then the
//! round loop. With all of std checked, the driver loads every std file (`velt check`'s
//! `with_all_std`) and nothing is checked lazily (`body::lazy_std`).

pub(crate) mod codec;

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

/// [`crate::check_definitions`] by units: std collected and checked, frozen, then the app.
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
        // The passes after the bodies run for std alone (`crate::analyze`); it is frozen once
        // its rounds are done (`crate::rounds`).
        return;
    }
    if cx.units_stage == Stage::Both {
        cx.freeze_defs();
    }
    crate::timings::pass("units: app", || {
        crate::timings::pass("units: app tdz", || crate::script_tdz::check(cx));
        let from = crate::timings::pass("units: app collect", || {
            crate::collect::collect_app(cx, &app)
        });
        cx.collected_defs = cx.defs.len();
        // What std's check built on first use from the classes, implementations and `extend`
        // blocks it had: the app's add to them.
        cx.groups = None;
        cx.subclasses = None;
        cx.primitive_statics = None;
        cx.classes_below.clear();
        cx.marked_aliases = Default::default();
        cx.native_libraries = Default::default();
        cx.uses_this_fn = Default::default();
        crate::decorators::check(cx, from.defs);
        crate::global_this::declare(cx);
        crate::timings::pass("units: app bodies", || crate::body::check_added(cx, from));
        crate::timings::pass("units: app finish", || {
            crate::body::finish_bodies(cx, from.defs)
        });
    });
    if cx.units_stage == Stage::Both {
        cx.thaw_defs();
    }
}

/// EXPERIMENT: `VELT_UNITS=2`, std checked as a check of its own (its rounds and passes), then
/// the app's rounds from a copy of it.
pub(crate) fn stage2() -> bool {
    #[cfg(test)]
    if codec::hook::installed() {
        return true;
    }
    std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2")
}

/// What a finished unit's check keeps for a later unit's: each pass's summary, which the later
/// unit's check starts from instead of reading the finished unit's bodies again.
#[derive(Clone, Default)]
pub(crate) struct Summaries {
    /// What each generic function of a finished unit needs of its type arguments to be a valid
    /// record key, and the param calls that reach it (`record_keys`): that unit's summary, which
    /// a later unit's check starts from (`crate::units`).
    pub(crate) key_needs: crate::record_keys::KeyNeeds,
    /// The same for the local async closures' analysis (`ownership::local_async`).
    pub(crate) async_summaries: crate::ownership::AsyncSummaries,
    /// The same for `json`'s check.
    pub(crate) json_needs: crate::json::JsonNeeds,
    /// The same for `struct_tuples`' checks.
    pub(crate) tuple_summaries: crate::struct_tuples::Summaries,
    /// The same for `weak_threads`' check.
    pub(crate) weak_scans: crate::weak_threads::WeakScans,
    /// The same for `module_state::check_locked_access`.
    pub(crate) state_facts: crate::module_state::StateFacts,
    /// The weak map types a finished unit's bodies make (`replaced_holders::implicit`).
    pub(crate) weak_made: Option<Vec<crate::hir::TyId>>,
    /// The same for `undefined_args`' walk of the definitions.
    pub(crate) undefined_facts: crate::undefined_args::UndefinedFacts,
    /// The same for the `with` checks' function summaries (`ownership::locked`).
    pub(crate) lock_summaries: crate::ownership::LockSummaries,
    /// The modules each module of a finished unit names (`crate::linked`).
    pub(crate) module_edges: crate::linked::ModuleEdges,
    /// A finished unit's part of the dispatch groups (`throws::groups`).
    pub(crate) groups: Option<crate::throws::UnitGroups>,
    /// A finished unit's dispatch groups for the modes' joins (`ownership::mutation`).
    pub(crate) dispatch: Option<crate::ownership::UnitDispatch>,
    /// A finished unit's functions with a generator, and how many definitions it has
    /// (`Ctx::generated_fns`).
    pub(crate) generated_fns: Option<(rustc_hash::FxHashSet<crate::hir::DefId>, usize)>,
    /// A finished unit's scan for the changes analysis (`ownership::local_async`), as its
    /// bodies' part, its initializers' part, and how many definitions it covers.
    pub(crate) literals: Option<crate::ownership::KeptLiterals>,
    /// The fields a finished unit's bodies feed with values that are not new (`local_async::fed`).
    pub(crate) fed: Option<(rustc_hash::FxHashSet<(crate::hir::DefId, u32)>, usize)>,
    /// How many modules a finished unit has: later modules are a later unit's (or generated).
    pub(crate) modules: usize,
    /// A finished unit's classes below each class (`Ctx::subclass_map`).
    pub(crate) subclasses: Option<crate::ctx::classes::SubclassMap>,
}

/// std's part of two per-module facts ([`Order::std_facts`]).
pub(crate) type StdFacts<'a> = (
    Vec<Option<&'a crate::namespaces::Private>>,
    Vec<(&'a String, Vec<usize>)>,
);

/// The modules of a check by units in the order its contexts hold them: std's first, which
/// std's unit is checked over alone, then the app's, added to a copy of std's frozen unit (with
/// any generated module std's check added between them).
pub(crate) struct Order {
    std: Vec<usize>,
    app: Vec<usize>,
    root: usize,
}

impl Order {
    pub(crate) fn of(modules: &[crate::SourceModule], root: usize) -> Order {
        let (std, app) = (0..modules.len()).partition(|&m| modules[m].is_std);
        Order { std, app, root }
    }

    pub(crate) fn std_modules<'m>(
        &self,
        modules: &'m [crate::SourceModule],
    ) -> Vec<&'m crate::SourceModule> {
        self.std.iter().map(|&m| &modules[m]).collect()
    }

    /// Add the app's modules to `cx`, std's unit (the root becomes its root).
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

    /// Where original module `m` is in std's unit (`None`: an app module).
    pub(crate) fn in_std(&self, m: usize) -> Option<usize> {
        self.std.iter().position(|&s| s == m)
    }

    /// Where original module `m` is once the app's modules were added at `base`.
    pub(crate) fn in_app(&self, m: usize, base: usize) -> Option<usize> {
        self.in_std(m)
            .or_else(|| self.app.iter().position(|&a| a == m).map(|k| base + k))
    }

    /// std's part of the per-module facts `ns_private` and `global_ns` (by original module),
    /// in std's order, for the key of std's unit (`crate::std_unit`).
    pub(crate) fn std_facts<'a>(
        &self,
        ns_private: &'a [crate::namespaces::Private],
        global_ns: &'a crate::namespaces::GlobalNamespaces,
    ) -> StdFacts<'a> {
        let private = self.std.iter().map(|&m| ns_private.get(m)).collect();
        let mut names: Vec<(&String, Vec<usize>)> = (global_ns.iter())
            .map(|(n, ms)| (n, ms.iter().filter_map(|&m| self.in_std(m)).collect()))
            .collect();
        names.sort();
        (private, names)
    }

    /// Where the app's modules start in `cx` (after std's and the generated ones).
    pub(crate) fn app_base(&self, cx: &Ctx) -> usize {
        cx.modules.len() - self.app.len()
    }
}

/// Put the facts the source rewrites found per module (by the modules' original indices) in
/// `cx`'s order: original module `m` is `at(m)`, and one `at` drops is not in `cx`.
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

/// Finish a unit (std's, as its stage ends): freeze its definitions, and keep the indexes a
/// later unit's stage extends with its own definitions instead of walking every definition again.
pub(crate) fn finish_unit(cx: &mut Ctx) {
    cx.freeze_defs();
    cx.unit_base = cx.defs.len() as u32;
    cx.unit_sums.modules = cx.modules.len();
    let generated = cx.generated_fns();
    cx.unit_sums.generated_fns = Some((generated, cx.info.len()));
    crate::ownership::keep_unit_scans(cx);
}

/// `VELT_UNITS_TOUCH`: report a phase of the app's stage, with what copying the std definitions
/// it read costs (a floor for decoding them from an on-disk unit).
pub(crate) fn touch_phase(cx: &Ctx, phase: &str, more: bool) {
    use crate::ctx::def_table::touch;
    if !touch::on() {
        return;
    }
    let (defs, infos) = (touch::touched("Def>"), touch::touched("DefInfo<'_>"));
    let t = std::time::Instant::now();
    let copied: Vec<_> = defs.iter().map(|&i| cx.defs.items_at(i).clone()).collect();
    let copied_infos: Vec<_> = infos.iter().map(|&i| cx.info.items_at(i).clone()).collect();
    let took = t.elapsed();
    eprintln!(
        "velt-touch: copying the {} bodies and {} infos read takes {:.2} ms",
        copied.len(),
        copied_infos.len(),
        took.as_secs_f64() * 1e3
    );
    drop((copied, copied_infos));
    let t = std::time::Instant::now();
    let all = (cx.defs.items_all().to_vec(), cx.info.items_all().to_vec());
    let took_all = t.elapsed();
    eprintln!(
        "velt-touch: copying every definition ({}) takes {:.2} ms",
        all.0.len(),
        took_all.as_secs_f64() * 1e3
    );
    touch::phase(phase, more);
}

impl Ctx<'_> {
    /// The first module after a finished unit's (0 outside an app's stage by units).
    pub(crate) fn app_module_base(&self) -> usize {
        match self.units_stage {
            Stage::App => self.unit_sums.modules,
            _ => 0,
        }
    }
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    Summaries {
        key_needs, async_summaries, json_needs, tuple_summaries, weak_scans, state_facts,
        weak_made, undefined_facts, lock_summaries, module_edges, groups, dispatch, generated_fns,
        literals, fed, modules, subclasses,
    }
}

/// EXPERIMENT (`VELT_UNITS_ROUNDTRIP=1`, #2943 M2): the app's stage starts from std's unit
/// encoded and decoded in this process, as a unit loaded from disk would be; prints its size and
/// what encoding and decoding took (`velt-units:` lines). The decoded unit's arena is leaked.
pub(crate) fn round_trip(cx: Ctx<'_>) -> Ctx<'_> {
    if std::env::var_os("VELT_UNITS_ROUNDTRIP").is_none() {
        return cx;
    }
    let t = std::time::Instant::now();
    let (bytes, sizes) = codec::encode_unit_sized(&cx);
    let encoded = t.elapsed();
    if std::env::var_os("VELT_UNITS_ROUNDTRIP").is_some_and(|v| v == "sizes") {
        let mut sizes = sizes;
        sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
        for (field, n) in sizes.iter().take(25) {
            eprintln!("velt-units: {n:>9} bytes  {field}");
        }
    }
    let arena: &'static codec::Arena = Box::leak(Box::default());
    let t = std::time::Instant::now();
    // SAFETY: the arena is leaked, so it outlives the context.
    let decoded = unsafe { codec::decode_unit(&bytes, arena, cx.modules.clone()) };
    let took = t.elapsed();
    let Some(mut out) = decoded else {
        eprintln!("velt-units: std's unit did not decode; checking on the unit itself");
        return cx;
    };
    eprintln!(
        "velt-units: std's unit {} bytes, encoded in {:.1} ms, decoded in {:.1} ms",
        bytes.len(),
        encoded.as_secs_f64() * 1e3,
        took.as_secs_f64() * 1e3
    );
    // What the check set when it made std's context, which an app's round keeps (`new_cx`).
    out.units = cx.units;
    out.units_stage = cx.units_stage;
    out.stack_budget = cx.stack_budget;
    out
}
