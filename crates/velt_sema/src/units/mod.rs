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
pub(crate) mod disk;
pub(crate) mod reach;
pub(crate) mod stubs;

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

/// The finished units' functions whose bodies dispatch over the program's classes
/// ([`Summaries::dispatch_fns`]), checked again with this unit's classes: what lowering builds of
/// them is this program's (as lazy std checks them with the program's classes). Their bodies
/// are kept with the unit (`codec::ast_refs`).
fn check_dispatch_again(cx: &mut Ctx) {
    // Only what is lowered needs it (not `velt check`), and only with classes of this unit's:
    // without, the finished unit's dispatch already covers every class.
    let base = cx.unit_base as usize;
    let new_class = (base..cx.info.len()).any(|i| {
        matches!(&cx.info[i], crate::defs::DefInfo::Adt(a) if a.kind == crate::hir::AdtKind::Class)
    });
    if cx.check_only || !new_class {
        return;
    }
    for d in cx.unit_sums.dispatch_fns.clone() {
        let Some(info) = cx.try_fn(d) else { continue };
        if info.source.is_none() {
            continue;
        }
        crate::ctx::def_table::crossing();
        cx.defs[d.0 as usize] = None;
        cx.fn_info_mut(d).state = crate::defs::BodyState::Unchecked;
        crate::body::ensure_body(cx, d);
    }
}

/// [`crate::check_definitions`] by units: std collected and checked, frozen, then the app.
pub(crate) fn check_definitions(cx: &mut Ctx) {
    let app: Vec<usize> = (0..cx.modules.len())
        .filter(|&m| !in_first_unit(cx.modules[m]) && !cx.is_generated_module(m))
        .collect();
    // The dependencies in std's unit ([`libraries`]): collected and checked after std's own.
    let libs: Vec<usize> = (0..cx.modules.len())
        .filter(|&m| !cx.modules[m].is_std && in_first_unit(cx.modules[m]))
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
        // Where the dependencies' definitions start: the program of a check from disk is built
        // from them on (`Ctx::program_from`).
        cx.unit_sums.lib_base = cx.defs.len();
        if !libs.is_empty() {
            crate::timings::pass("units: libraries", || {
                let from = crate::collect::collect_app(cx, &libs);
                cx.collected_defs = cx.defs.len();
                cx.groups = None;
                cx.subclasses = None;
                cx.primitive_statics = None;
                cx.classes_below.clear();
                cx.marked_aliases = Default::default();
                cx.native_libraries = Default::default();
                cx.uses_this_fn = Default::default();
                crate::decorators::check(cx, from.defs);
                crate::global_this::declare(cx);
                crate::body::check_added(cx, from);
                crate::body::finish_bodies(cx, from.defs);
            });
        }
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
        // EXPERIMENT (#2943): the program's classes are collected, so the seeds of std's trap
        // sites name them.
        if cx.units_stage == Stage::App {
            cx.seed_trap_vars();
        }
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
        check_dispatch_again(cx);
    });
    if cx.units_stage == Stage::Both {
        cx.thaw_defs();
    }
}

/// EXPERIMENT: `VELT_UNITS=2`, std checked as a check of its own (its rounds and passes), then
/// the app's rounds from a copy of it.
/// EXPERIMENT (#2943 D6): the program's dependencies are in std's unit (the libraries' unit),
/// checked and frozen with it, not with the app. On with `VELT_UNITS_LIB=1`, and with std's unit
/// on disk (`VELT_UNITS_DISK`) unless `VELT_UNITS_LIB=0`.
pub(crate) fn libraries() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(libraries_wanted) && !LIBS_OFF.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`libraries`]' setting (`VELT_UNITS_LIB`, `VELT_UNITS_DISK`).
pub fn libraries_wanted() -> bool {
    match std::env::var_os("VELT_UNITS_LIB") {
        Some(v) => v != "0",
        None => std::env::var_os("VELT_UNITS_DISK").is_some(),
    }
}

/// Set when the dependencies' stage reported an error: the check is done again with them in
/// the app's unit, so that the app's errors are reported with theirs (as a check without
/// [`libraries`] does). For the rest of the process.
static LIBS_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [`LIBS_OFF`]; whether the dependencies were in std's unit until now.
pub(crate) fn libraries_off(modules: &[crate::SourceModule]) -> bool {
    let had = libraries() && modules.iter().any(|m| m.is_dependency);
    LIBS_OFF.store(true, std::sync::atomic::Ordering::Relaxed);
    had
}

/// Whether module `m` is in the first unit (std's, with the dependencies under [`libraries`]).
pub(crate) fn in_first_unit(m: &crate::SourceModule) -> bool {
    m.is_std || (m.is_dependency && libraries())
}

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
    /// A finished unit's calls, for the demand of generated functions (`generated::demand`).
    pub(crate) calls: Option<crate::generated::demand::Calls>,
    /// A finished unit's instantiation sites (`instantiation_cycles`).
    pub(crate) inst_sites: Option<crate::instantiation_cycles::Sites>,
    /// The functions a finished unit names outside its bodies (`ownership::local_async`).
    pub(crate) outside_named: Option<crate::ownership::OutsideNamed>,
    /// The types code outside a finished unit's bodies mentions (`ownership::local_async`).
    pub(crate) outside_tys: Option<Vec<crate::hir::TyId>>,
    /// A finished unit's module-level `let`s (`module_state::order`).
    pub(crate) lets: Option<Vec<(usize, u32, crate::hir::DefId)>>,
    /// How many types there were when the unit was finished.
    pub(crate) types: usize,
    /// A finished unit's functions whose bodies dispatch over the program's classes: checked
    /// again in a later unit's stage ([`check_dispatch_again`]); the unit keeps their bodies.
    pub(crate) dispatch_fns: Vec<crate::hir::DefId>,
    /// Where a finished unit's dependencies' definitions start (its std's end, [`libraries`]).
    pub(crate) lib_base: usize,
    /// A finished unit's scan for the value params (`ownership::exclusive`).
    pub(crate) value_scan: Option<crate::value_params::Scanned>,
    /// A finished unit's classes' slots (`locked_props::Dispatch`).
    pub(crate) virtuals:
        Option<rustc_hash::FxHashMap<(crate::hir::DefId, u32), Vec<crate::hir::DefId>>>,
    /// A finished unit's part of the implicit calls (`replaced_holders::implicit`).
    pub(crate) implicit: Option<crate::replaced_holders::implicit::ImplicitScan>,
    /// A finished unit's host functions (`collect::host_sig`).
    pub(crate) host_fns: Option<Vec<crate::hir::DefId>>,
    /// A finished unit's std functions that read a member of an `any` (`Ctx::dyn_member_fns`):
    /// a later unit's program has `any`'s member slots when it reaches one.
    pub(crate) dyn_member_fns: Vec<crate::hir::DefId>,
    /// A finished unit's roots of a build's program set (`reach::std_roots`): the std
    /// functions lowering looks up by name and every hook.
    pub(crate) reach_roots: Option<Vec<crate::hir::DefId>>,
    /// A finished unit's methods and constructors by owner, with how many definitions it had
    /// (`lazy_std::live::methods`): a later unit's stage adds its own instead of reading every
    /// definition, which decodes all of a unit from disk.
    pub(crate) methods_by_owner: Option<(
        rustc_hash::FxHashMap<crate::hir::DefId, Vec<crate::hir::DefId>>,
        usize,
    )>,
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
        let (std, app) = (0..modules.len()).partition(|&m| in_first_unit(&modules[m]));
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

    /// With std's modules in the order std's unit has them: `at[k]` is the position (in this
    /// order's std modules) of the unit's module `k` (`units::disk::load`).
    pub(crate) fn reorder_std(self, at: &[usize]) -> Order {
        let std = at.iter().map(|&k| self.std[k]).collect();
        Order { std, ..self }
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
    cx.unit_sums.calls = Some(crate::generated::demand::unit_calls(cx));
    cx.unit_sums.inst_sites = Some(crate::instantiation_cycles::unit_sites(cx));
    cx.unit_sums.outside_named = Some(crate::ownership::unit_named_outside_bodies(cx));
    cx.unit_sums.outside_tys = Some(crate::ownership::unit_outside_types(cx));
    cx.unit_sums.lets = Some(crate::module_state::unit_lets(cx));
    cx.unit_sums.types = cx.ty.table.len();
    cx.unit_sums.value_scan = Some(crate::ownership::unit_value_scan(cx));
    cx.unit_sums.virtuals = Some(crate::module_state::locked_props::Dispatch::unit_virtuals(
        cx,
    ));
    cx.unit_sums.implicit = Some(crate::replaced_holders::implicit::unit_scan(cx));
    cx.keep_js_api_modules();
    let mut dispatch: Vec<crate::hir::DefId> = cx.program_dispatch.iter().copied().collect();
    dispatch.sort_unstable();
    cx.unit_sums.dispatch_fns = dispatch;
    cx.unit_sums.host_fns = Some(crate::collect::host_sig::unit_host_fns(cx));
    let mut dyn_fns: Vec<crate::hir::DefId> = cx.dyn_member_fns.iter().copied().collect();
    dyn_fns.sort_unstable();
    cx.unit_sums.dyn_member_fns = dyn_fns;
    cx.unit_sums.reach_roots = Some(reach::std_roots(cx));
    cx.unit_sums.methods_by_owner = Some(crate::body::lazy_std::unit_methods(cx));
    crate::ownership::keep_unit_scans(cx);
}

/// `VELT_UNITS_TOUCH`: report a phase of the app's stage, with what copying the std definitions
/// it read costs (a floor for decoding them from an on-disk unit).
pub(crate) fn touch_phase(cx: &Ctx, phase: &str, more: bool) {
    use crate::ctx::def_table::touch;
    if !touch::on() {
        return;
    }
    eprintln!(
        "velt-touch: {:>6} entries decoded on demand ({phase})",
        crate::ctx::def_table::decoded::get()
    );
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
    let all = (
        cx.defs.items_all().cloned().collect::<Vec<_>>(),
        cx.info.items_all().cloned().collect::<Vec<_>>(),
    );
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
        literals, fed, modules, subclasses, calls, inst_sites, outside_named,
        outside_tys, lets, types, lib_base, dispatch_fns, value_scan,
        virtuals, implicit, host_fns, dyn_member_fns, reach_roots, methods_by_owner,
    }
}

/// `VELT_UNITS_ROUNDTRIP=sizes`: the bytes each summary takes.
fn summary_sizes(s: &Summaries) {
    use codec::Codec;
    let Summaries {
        key_needs,
        async_summaries,
        json_needs,
        tuple_summaries,
        weak_scans,
        state_facts,
        weak_made,
        undefined_facts,
        lock_summaries,
        module_edges,
        groups,
        dispatch,
        generated_fns,
        literals,
        fed,
        modules,
        subclasses,
        calls,
        inst_sites,
        outside_named,
        outside_tys,
        lets,
        types,
        lib_base,
        dispatch_fns,
        value_scan,
        virtuals,
        implicit,
        host_fns,
        dyn_member_fns,
        reach_roots,
        methods_by_owner,
    } = s;
    let mut sizes: Vec<(&str, usize)> = vec![];
    macro_rules! size { ($($f:ident),*) => {$(
        let mut w = codec::Writer::new();
        $f.enc(&mut w);
        sizes.push((stringify!($f), w.finish().0.len()));
    )*} }
    size!(
        key_needs,
        async_summaries,
        json_needs,
        tuple_summaries,
        weak_scans,
        state_facts,
        weak_made,
        undefined_facts,
        lock_summaries,
        module_edges,
        groups,
        dispatch,
        generated_fns,
        literals,
        fed,
        modules,
        subclasses,
        calls,
        inst_sites,
        outside_named,
        outside_tys,
        lets,
        types,
        lib_base,
        dispatch_fns,
        value_scan,
        virtuals,
        implicit,
        host_fns,
        dyn_member_fns,
        reach_roots,
        methods_by_owner
    );
    sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
    for (f, n) in sizes {
        eprintln!("velt-units: {n:>9} bytes  unit_sums.{f}");
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
        summary_sizes(&cx.unit_sums);
        let mut sizes = sizes;
        sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
        for (field, n) in sizes.iter().take(25) {
            eprintln!("velt-units: {n:>9} bytes  {field}");
        }
    }
    let arena: &'static codec::Arena = Box::leak(Box::default());
    let t = std::time::Instant::now();
    // SAFETY: the arena is leaked, so it outlives the context.
    let bytes = std::rc::Rc::new(bytes);
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

/// The rewrites' inputs by units with `VELT_UNITS_SPLIT` (`crate::rewrites`, #2943 M2): the
/// modules with std's as stubs, for the app's run, and std's modules alone, for std's.
pub(crate) fn split_inputs(
    opts: crate::CheckOptions,
    modules: &[crate::SourceModule],
) -> (
    Option<Vec<crate::SourceModule>>,
    Option<Vec<crate::SourceModule>>,
) {
    // std's modules that are stubs already (`CheckOptions::std_stubbed`) need neither.
    let split = opts.units
        && !opts.std_stubbed
        && stage2()
        && std::env::var_os("VELT_UNITS_SPLIT").is_some();
    if !split {
        return (None, None);
    }
    let std = (modules.iter())
        .filter(|m| in_first_unit(m))
        .map(stubs::copy)
        .collect();
    (Some(stubs::stubbed(modules)), Some(std))
}

/// A unit loaded from disk (`units::disk::load`) as an app's round takes it.
pub(crate) fn app_unit(
    loaded: Option<(Ctx<'_>, Vec<usize>)>,
    stack_budget: usize,
) -> Option<Ctx<'_>> {
    let (mut cx, _) = loaded?;
    (cx.units, cx.units_stage, cx.stack_budget) = (true, Stage::Std, stack_budget);
    Some(cx)
}
