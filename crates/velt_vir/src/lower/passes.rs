//! The lowering passes over a whole program: lowering repeats until the counted types stop
//! growing (boxing/), and the last pass is the program.

use super::*;

/// Most lowering passes the counted-type fixpoint may take (boxing/): each pass adds types,
/// and the closure computed after a pass already contains everything its facts imply.
const MAX_BOXING_PASSES: usize = 8;
/// Passes `lower_program` may take: the fixpoint's, and one more without the `await` steps.
const MAX_PASSES: usize = MAX_BOXING_PASSES + 1;

/// Lower a whole checked program (see [`crate::lower`]). Lowering repeats while a pass finds
/// shares of types that were not counted yet (boxing/); the type table carries over, so type
/// ids in the counted set stay valid.
///
/// The passes until the counted types stop growing only find them (`Cx::facts_only`): their
/// functions leave out the versions of view loops, which find no shares and were most of a
/// pass's time in large programs with array views. The pass that finds no more is the program:
/// the functions it left unversioned are versioned then (deferred_versions.rs), each as its
/// own `finish` would have.
pub(crate) fn lower_program(
    hir: &hir::Program,
    opts: &LowerOptions,
) -> (vir::Program, Vec<velt_common::Diagnostic>) {
    let (program, diags, _) = lower_program_seeded(hir, opts, false);
    (program, diags)
}

/// What [`lower_program_kept`] gives: the program, its diagnostics, its counted types, and the
/// final pass's context.
type Kept<'h> = (
    vir::Program,
    Vec<velt_common::Diagnostic>,
    Option<CountedSeed>,
    Option<Cx<'h>>,
);

/// [`lower_program`], starting from `opts.counted_seed` and returning the final counted types
/// as a seed when `want_seed` (boxing/seed.rs).
pub(crate) fn lower_program_seeded(
    hir: &hir::Program,
    opts: &LowerOptions,
    want_seed: bool,
) -> (
    vir::Program,
    Vec<velt_common::Diagnostic>,
    Option<CountedSeed>,
) {
    let (p, d, s, _) = lower_program_kept(hir, opts, want_seed, false);
    (p, d, s)
}

/// [`lower_program_seeded`], also returning the final pass's context when `keep` (kept.rs: a
/// `velt dev` reload lowers edited functions into it).
pub(super) fn lower_program_kept<'h>(
    hir: &'h hir::Program,
    opts: &LowerOptions,
    want_seed: bool,
    keep: bool,
) -> Kept<'h> {
    let mut types = hir.types.clone();
    let mut counted = boxing::Boxing::initial(hir);
    let mut no_await_steps = false;
    let mut value_params = None;
    // `VELT_LOWER_WHOLE`: every pass whole, as before facts-only passes (tests compare both).
    let facts_only = std::env::var_os("VELT_LOWER_WHOLE").is_none();
    let profile = std::env::var_os("VELT_LOWER_PROFILE").is_some();
    // One more pass when the `await` steps turn out to be unobservable (await_order.rs).
    // A pass that only finds more names called through `any` doesn't count (a finite set: the
    // string literals of the program).
    let mut next_pass = 0;
    let mut counted_passes = 0;
    let mut seeded = false;
    while counted_passes < MAX_PASSES {
        let pass = next_pass;
        next_pass += 1;
        lower_profile::start_pass();
        let pass_start = std::time::Instant::now();
        let mut cx = Cx::new(hir, types, counted.clone());
        cx.value_params = value_params.take();
        cx.native_inits = opts.native_inits.to_vec();
        cx.cell_checks = opts.cell_checks;
        cx.target = opts
            .target
            .map_or_else(crate::system::host_triple, str::to_string);
        cx.cpu_checks.no_extensions = opts.no_cpu_extensions;
        cx.jit = opts.jit;
        cx.no_await_steps = no_await_steps;
        cx.facts_only = facts_only;
        cx.glue_slots.keep_all = want_seed || glue::demand_off();
        cx.shapes.wanted = shape::wanted(opts.shapes);
        cx.shapes.on = cx.shapes.wanted;
        cx.locs = opts
            .source_map
            .map(|sm| srcloc::LocMap::new(sm, opts.std_root));
        cx.debug = (opts.debug_info && cx.locs.is_some()).then(Default::default);
        if let (0, Some(seed)) = (pass, opts.counted_seed) {
            seeded = !seed.is_empty();
            counted = cx.with_seed(counted, seed);
            cx.boxing = counted.clone();
        }
        cx.seed_functions();
        cx.drain(false);
        cx.build_iface_glues();
        cx.seed_c_exports();
        // `velt_main` last: whether it selects the per-core scheduler depends on everything else;
        // then module state's accesses and the static atomics' release (`Cx::drain`).
        cx.func(Work::Main);
        cx.drain(true);
        cx.build_iface_glues();
        cx.build_fn_tables();
        lower_profile::end_pass(pass, pass_start.elapsed());
        cx.shapes_close();
        let next = cx.close_boxing();
        if next == counted && seeded && next.locks_may_follow_seed() {
            return lower_program_kept(hir, &boxing::without_seed(opts), want_seed, keep);
        }
        if next == counted {
            // The counted types are complete: this pass's functions are the program's, once
            // those left unversioned are versioned.
            cx.version_deferred();
        }
        if profile {
            let kind = if facts_only { "facts only" } else { "whole" };
            let (n, t) = (cx.versioned, view_unswitch_plain::take_terms());
            eprintln!(
                "velt-lower: pass {pass} {kind}: view loops versioned in {n} functions, {t} terms kept"
            );
        }
        if let Some(mb) = lower_profile::peak_mb().filter(|_| profile) {
            eprintln!("velt-lower: peak {mb} MB after pass {pass}");
        }
        if profile && cx.shapes.on {
            let (shared, per_type) = cx.shapes.counts();
            eprintln!("velt-lower: pass {pass}: {shared} shared instances for {per_type} per type");
        }
        if profile && next != counted {
            eprintln!("velt-lower: pass {pass} grew: {}", counted.changes(&next));
        }
        if next == counted {
            if cx.facts.unmet {
                ice("a shared type was not counted");
            }
            if std::env::var_os("VELT_DEBUG_COUNTED").is_some() {
                eprintln!("velt: counted types: {}", cx.counted_names().join(", "));
                eprintln!("velt: weak-capable types: {}", cx.weak_names().join(", "));
            }
            if std::env::var_os("VELT_DEBUG_PROXY").is_some() {
                eprintln!("velt: proxiable types: {}", cx.proxiable_names().join(", "));
            }
            let mut diags = cx.nothing_clash_diagnostics();
            diags.extend(cx.fresh_fn_diagnostics());
            diags.extend(cx.counted_atomic_diagnostics());
            diags.extend(cx.counted_lock_diagnostics());
            diags.extend(cx.atomic_copy_diagnostics());
            diags.extend(cx.unavailable_diagnostics());
            diags.extend(cx.unreduced_diagnostics());
            cx.unfreeze_module_reach();
            let seed = want_seed.then(|| cx.seed(&next));
            let types_now = cx.types.clone();
            let (program, cx) = match keep {
                true => (cx.finish_copy(), Some(cx)),
                false => (cx.finish(), None),
            };
            if !no_await_steps && async_fn::await_order::unobservable(&program) {
                // Lower again without the steps: the program then has no step code at all.
                no_await_steps = true;
                types = types_now;
                counted_passes += 1;
                continue;
            }
            return (program, diags, seed, cx);
        }
        value_params = cx.value_params.take();
        types = cx.types;
        if !counted.same_but_call_keys(&next) {
            counted_passes += 1;
        }
        counted = next;
    }
    ice("the counted types did not converge")
}
