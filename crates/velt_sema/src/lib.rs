//! Semantic analysis: name resolution, type checking/inference, ownership analysis → [`hir::Program`].
//! `hir.rs` and the `check` signature are contracts (maintainer-owned).
//!
//! Pipeline inside `check` (after [`generic_arrows`] rewrites module-level generic arrows):
//! 1. [`collect`]: a definition per module-level item of every module (prelude modules'
//!    exports are visible everywhere), type shapes, signatures, vtables, interface impls.
//! 2. [`body`]: module constants, defaults, then every function body: resolve names, type-check
//!    bidirectionally (with type-argument inference) and desugar into HIR. Closures become
//!    their own function defs. [`instantiation_cycles`] then rejects generic recursion whose
//!    type arguments grow (it would have infinitely many instantiations).
//! 3. [`ownership`]: infer which params / receivers / bindings take ownership (fixpoint) and
//!    patch call sites; string moves become soft (strings are values); reject moves out of
//!    borrowed places.
//! 4. [`throws`]: infer each function's thrown type (fixpoint over the call graph); then
//!    [`record_keys`] (every `Record` key type, per instantiation), [`json`] (what JSON glue is
//!    generated for) and [`void_fields`] (no `void` fields).
//! 5. [`moves`]: flow-sensitive use-after-move / use-before-init analysis over the HIR; soft moves
//!    (async-call arguments, strings) used again become clones (`ownership::clone_reused`); then exclusive
//!    access per call (`ownership::check_exclusive`), thread boundaries, and what `Mutex.with`
//!    callbacks let past the lock (`ownership::check_locked`).
//! 6. `main` validation, [`finalize`] into a `hir::Program`.
//!
//! [`ide::check_for_ide`] runs steps 1–5 with side tables recorded for editors instead.
//!
//! Semantic decisions other stages rely on are documented on [`body`] and the modules above.

#[cfg(test)] // by its name, for the integration tests' loader (`script_locals_tests.rs`)
extern crate self as velt_sema;
pub mod hir;
pub mod js_protos;

mod absent_fields;
mod ctx_lookup;
mod lib_dom_names;
mod not_found_notes;
mod reserved;
pub(crate) use reserved::{reserved_key, reserved_key_message, reserved_prefixes};
mod alias_expand;
mod anon;
mod any;
mod any_iface_sites;
mod asserted_targets;
mod assigned_fields;
mod ast_walk;
mod atomic_copies;
mod augment_scope;
mod body;
mod brands;
mod call_sigs;
mod callables;
mod check_work;
mod collect;
mod copy_flow;
mod copy_types;
mod creation_notes;
mod ctx;
mod decorators;
mod defined_accessors;
mod defined_props;
mod defs;
mod discriminants;
mod dispatch;
mod display;
pub mod effects;
mod enum_keys;
mod expansive_types;
mod finalize;
mod flow;
mod fn_types;
mod fresh_returns;
pub mod generated;
mod generating;
mod generic_arrows;
mod generic_fns;
mod generic_override_sites;
mod generic_spread;
mod global_this;
#[cfg(test)]
mod hasher_tests;
mod holds_promise;
mod hook_calls;
mod hooks;
pub mod value_params;
#[doc(hidden)]
pub use hook_calls::hook_args;
pub use hooks::NAMES as HOOK_NAMES;
pub use units::disk::{read_stubs as read_std_stubs, UNIT_NEEDED as STD_UNIT_NEEDED};
mod canon_key;
mod glue_reach;
pub mod ide;
mod iface_overloads;
mod iface_writes;
mod index_signatures;
mod infer;
mod inst_sites;
mod instantiation_cycles;
pub mod interface;
mod interface_merge;
mod intersection_class_parts;
mod intersection_members;
mod intersection_records;
mod intersections;
mod intrinsic_decls;
mod js_api;
mod json;
mod kept;
mod kept_consts;
mod kept_reload;
mod key_groups;
mod known;
mod layout_merge;
mod linked;
mod literals;
mod main_fn;
mod module_chunks;
mod module_destructure;
mod module_scripts;
mod module_state;
mod moves;
mod namespaces;
mod nested_closures;
mod object_assigns;
mod object_call_on;
mod object_copies;
mod object_ifaces;
mod object_views;
mod options;
mod overload_adapter;
mod overloads;
mod ownership;
mod par;
mod program;
mod promise_copies;
pub mod property_order;
mod readonly;
mod record_keys;
mod recursive_aliases;
mod recursive_unions;
mod replaced_holders;
mod resolve;
mod rewrite_facts;
mod rewrites;
mod rounds;
mod script_locals;
mod script_tdz;
mod source_module;
mod source_scans;
mod std_load;
mod std_unit;
mod struct_tuples;
mod suggest;
mod symbols;
mod synth_ifaces;
mod thread_locals;
mod throws;
mod timings;
mod ts_protocol;
mod type_defaults;
mod type_level;
mod type_meanings;
mod type_queries;
mod type_reach;
mod typeof_generic;
mod types;
mod undefined_args;
mod union_layouts;
mod union_views;
mod unions;
mod unique_symbols;
mod units;
mod utility_types;
mod visit;
mod visit_ref;
mod void_fields;
mod weak_threads;
mod written_types;

use velt_common::{Diagnostic, Diagnostics, Span};

pub use check_work::{check_work, CheckWork};
pub use dispatch::{instantiation_work, InstantiationWork};
pub use generating::{check_generating, check_generating_kept, Checked};
pub use kept::{KeptCheck, PendingKept, Refused};
pub use kept_reload::member_name;
pub use options::CheckOptions;
pub use source_module::SourceModule;
pub use std_load::{LoadFn, Loads, StdBatch, StdLoads};
pub use timings::check_rounds;
pub use visit::rebase_detached_locals;

/// Stack for the checking thread: the passes recurse over the AST/HIR (bounded by the parser's
/// nesting limit), which can exceed the 1 MB main-thread stack on Windows.
pub(crate) const SEMA_STACK_BYTES: usize = 64 << 20;

/// How much of the checking thread's stack inferring return types may use: the rest is left for
/// checking the deepest body (`body::returns::ret_of`).
pub(crate) const SEMA_STACK_BUDGET: usize = 48 << 20;

/// The budget when the checking thread could not be spawned and the caller's stack (of unknown
/// size, at least the 1 MB of a Windows main thread) is used instead.
pub(crate) const FALLBACK_STACK_BUDGET: usize = 512 << 10;

/// The address of a local of the caller's frame: how deep the stack is here (it grows down on
/// every supported target).
#[inline(never)]
pub(crate) fn stack_address() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// CONTRACT: check a whole program. `modules[root]` must define `main`.
/// Returns `Some(program)` iff there are no errors; warnings may accompany either outcome.
pub fn check(modules: &[SourceModule], root: usize) -> (Option<hir::Program>, Diagnostics) {
    check_with(modules, root, CheckOptions::default())
}

/// [`check`] with options (`velt check` checks library modules, which have no `main`).
pub fn check_with(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
) -> (Option<hir::Program>, Diagnostics) {
    let (out, diags) = generating::on_sema_thread(|budget| {
        check_on_current_thread(modules, root, opts, &[], None, None, budget)
    });
    generating::program_or_demand_error(out, diags)
}

/// [`check_with`] of modules some of which the parser had to recover (`recovered`: their
/// indices in `modules` with their source text), for `velt check` to report the errors of the
/// other modules too. A name imported from a recovered module (directly or through re-exports)
/// that it doesn't declare but whose source has that word may be a declaration the parser
/// dropped: importing and using it reports nothing. The caller drops the diagnostics located in
/// recovered modules (their syntax errors come first). Never builds a program.
pub fn check_recovered(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
) -> Diagnostics {
    generating::on_sema_thread(|budget| {
        check_on_current_thread(modules, root, opts, recovered, None, None, budget).1
    })
}

fn check_on_current_thread(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
    gen: Option<generated::GenArgs>,
    loads: Option<Loads>,
    stack_budget: usize,
) -> (Option<Checked>, Diagnostics) {
    timings::start();
    let out = timings::pass("check", || {
        script_locals::check(modules, root, |m| {
            check_timed(m, root, opts, recovered, gen, loads, stack_budget)
        })
    });
    timings::finish();
    if opts.units {
        ctx::def_table::report();
    }
    out
}

/// [`check_on_current_thread`], its passes timed (`timings`).
fn check_timed(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
    gen: Option<generated::GenArgs>,
    loads: Option<Loads>,
    stack_budget: usize,
) -> script_locals::Checked {
    // Outlives every `Ctx` below, with the modules the driver generates during the check.
    let generator = gen.and_then(|g| g.generate).map(generated::Generator::new);
    let input = modules;
    let rewrite_start = std::time::Instant::now();
    // By units with `VELT_UNITS_SPLIT` (#2943 M2): the rewrites run once over std alone (std's
    // stage) and once over the app with std's modules as stubs (`units::stubs`).
    let (stubbed, std_input) = units::split_inputs(opts, input);
    let rw = rewrites::run(stubbed.as_deref().unwrap_or(input), root);
    let std_rw = std_input.as_deref().map(|s| rewrites::run(s, s.len()));
    let modules = rw.modules(stubbed.as_deref().unwrap_or(input));
    let std_mods = (std_rw.as_ref())
        .zip(std_input.as_deref())
        .map(|(r, i)| r.modules(i));
    let merged = std_rw.as_ref().map(|s| units::stubs::merge(s, &rw, input));
    let lifted = merged
        .as_ref()
        .map_or(rw.lifted.as_ref(), |m| m.lifted.as_ref());
    let overloaded = (merged.as_ref()).map_or(rw.overloaded.as_ref(), |m| m.overloaded.as_ref());
    let Some(root_mod) = modules.get(root) else {
        let d = Diagnostic::error("no root module to check", Span::DUMMY);
        return ((None, vec![d]), Default::default());
    };
    timings::record("source rewrites", rewrite_start.elapsed());
    let source_scans::Found {
        null_proto,
        ctor_reads,
        record_accessors,
        gen_throw,
    } = timings::pass("source scans", || source_scans::program(modules));
    // What a kept check's reload compares with the edited program's (`kept::probe`), which
    // stops here.
    let facts = kept::wants_facts(input).then(|| {
        let lifted = rw
            .lifted
            .as_ref()
            .map(|l| (&l.local_fns, &l.all_fns, &l.unsafe_fns));
        let closures = rw.closures.as_ref().map(|c| (&c.diags, &c.kept, &c.groups));
        let rest: [&dyn std::fmt::Debug; 16] = [
            &null_proto,
            &ctor_reads,
            &record_accessors,
            &gen_throw,
            &rw.merge_diags,
            &rw.global_ns,
            &rw.ns_private,
            &rw.alias_blocked,
            &lifted,
            &rw.overloaded.as_ref().map(|o| &o.methods),
            &rw.brands,
            &rw.global_props,
            &rw.chunks.promoted,
            &rw.asserted.rmw,
            &closures,
            &root,
        ];
        std::sync::Arc::new(kept::Facts::of(modules, &rest))
    });
    if kept::probed(&facts.as_deref().cloned()) {
        return ((None, vec![]), Default::default());
    }
    // What a round found it needed once it was too late to demand or check it.
    let assumed = std::cell::RefCell::new(body::lazy_std::Assumed::default());
    // A check by units with std as a check of its own (`units::stage2`): the stage the next
    // round checks, and std's frozen unit the app's rounds start from.
    let stage = std::cell::Cell::new(units::Stage::Both);
    let order = (opts.units && units::stage2()).then(|| units::Order::of(modules, root));
    // std's unit kept between checks (`std_unit`): its key, the cache (locked until every
    // context made from it is dropped, so before them), and the copies of std's modules a unit
    // made by this check is checked over and owns.
    let found = rewrite_facts::Found {
        ns_private: &rw.ns_private,
        global_ns: &rw.global_ns,
        gen_throw,
        alias_blocked: merged
            .as_ref()
            .map_or(&rw.alias_blocked, |m| &m.alias_blocked),
        lifted,
        overloaded,
        null_proto,
        record_accessors,
        ctor_reads,
        brands: &rw.brands,
        global_props: &rw.global_props,
        recovered,
        merge_diags: &rw.merge_diags,
        promoted: &rw.chunks.promoted,
        rmw: &rw.asserted.rmw,
        closures: rw.closures.as_ref(),
    };
    // std's stage takes std's own facts when the rewrites ran per unit, with the app's flags.
    let std_found = std_rw.as_ref().map(|s| units::stubs::std_found(s, &found));
    let std_key = (order.as_ref())
        .zip(opts.std_key)
        .map(|(o, k)| found.std_key(k, opts, o, modules));
    let mut held = std_unit::hold(std_key);
    let mut std_copies = held
        .as_ref()
        .map(|_| std_unit::copies(std_mods.unwrap_or(modules)));
    // SAFETY: the copies' contents stay at their heap address until the check ends: moved into
    // the cache with their box (`std_unit::keep`), or dropped after every context.
    let copies: Option<&[SourceModule]> =
        (std_copies.as_deref()).map(|c| unsafe { &*(c as *const [SourceModule]) });
    // A unit loaded from disk keeps its AST here (`units::disk`): it outlives every context.
    // With the experiment on, never freed: the process ends after the check, and freeing a
    // whole std's AST costs more than a millisecond.
    let kept_arena = units::codec::Arena::default();
    let disk_arena: &units::codec::Arena = match std::env::var_os("VELT_UNITS_DISK") {
        Some(_) => Box::leak(Box::default()),
        None => &kept_arena,
    };
    let disk = units::disk::path(opts, (order.is_some()).then_some(&found));
    // std's modules for its unit: std's own run's, or the loader's stubs as they are.
    let unit_mods: Vec<&SourceModule> = match std_mods {
        Some(m) => m.iter().collect(),
        None => modules.iter().filter(|m| units::in_first_unit(m)).collect(),
    };
    // SAFETY: `disk_arena` is declared before every context and so outlives them.
    let load_unit =
        || (disk.as_deref()).and_then(|p| unsafe { units::disk::load(p, disk_arena, &unit_mods) });
    let disk_loaded = load_unit();
    let from_disk = disk_loaded.is_some();
    if opts.std_stubbed && disk_loaded.is_none() {
        let d = Diagnostic::error(units::disk::UNIT_NEEDED, Span::DUMMY);
        return ((None, vec![d]), Default::default());
    }
    let order = match (order, &disk_loaded) {
        (Some(o), Some((_, at))) => Some(o.reorder_std(at)),
        (o, _) => o,
    };
    let std_snap: std::cell::RefCell<Option<ctx::Ctx>> = Default::default();
    let new_cx = |round: &'static str,
                  held_borrows: bool,
                  seeds: &throws::FnTypeSeeds,
                  last: bool,
                  views: &union_views::Families| {
        timings::note_round();
        let from_std = match stage.get() {
            // A unit from disk is taken by the first round, and decoded again for another.
            units::Stage::App if from_disk => timings::pass("units: take std", || {
                (std_snap.borrow_mut().take())
                    .or_else(|| units::app_unit(load_unit(), stack_budget))
            }),
            units::Stage::App => timings::pass("units: copy std", || std_snap.borrow().clone()),
            _ => None,
        };
        let fresh = from_std.is_none();
        let mut cx = match (from_std, &order) {
            (Some(cx), _) => cx,
            (None, Some(o)) if stage.get() == units::Stage::Std => match (copies, std_mods) {
                (Some(c), _) => ctx::Ctx::over(c.iter().collect(), 0),
                (None, Some(m)) => ctx::Ctx::over(m.iter().collect(), 0),
                (None, None) => ctx::Ctx::over(o.std_modules(modules), 0),
            },
            (None, _) => ctx::Ctx::new(modules, root),
        };
        cx.units_stage = stage.get();
        if !fresh {
            cx.union_views.assumed = views.clone();
            cx.fn_sites.reseed(seeds.clone(), last);
            cx.held_borrows = held_borrows;
            // The app's modules after std's unit, with what the rewrites found of them.
            if let Some(o) = &order {
                // std's stage sealed the layouts after its passes: the app's conversions may put
                // its own families in use until its round seals them (`collect::extended_layouts`).
                cx.objects.layouts.sealed = false;
                o.add_app(&mut cx, modules);
                let base = o.app_base(&cx);
                generating::install(&mut cx, gen, generator.as_ref());
                found.install(&mut cx, true);
                units::remap_module_facts(&mut cx, &|m| o.in_app(m, base));
            }
        }
        if fresh {
            cx.units_stage = stage.get();
            if opts.eager_std {
                cx.lazy.on = false;
            }
            cx.units = opts.units;
            cx.lazy.native_executor = opts.native_executor;
            body::lazy_std::assume(&mut cx, &assumed.borrow());
            cx.loads = loads;
            // std's unit takes nothing of a check's generator (`std_unit`).
            if stage.get() != units::Stage::Std {
                generating::install(&mut cx, gen, generator.as_ref());
            }
            cx.union_views.assumed = views.clone();
            cx.fn_sites = throws::FnSites::new(seeds.clone(), last);
            cx.stack_budget = stack_budget;
            // A check that keeps itself records its bodies, for the kept check to check them again.
            cx.body_records = kept::wanted(input).then(Default::default);
            cx.source_facts = facts.clone();
            cx.held_borrows = held_borrows;
            match (&std_found, stage.get()) {
                (Some(f), units::Stage::Std) => f.install(&mut cx, false),
                _ => found.install(&mut cx, stage.get() != units::Stage::Std),
            }
        }
        if let (true, Some(o), units::Stage::Std, None) = (fresh, &order, stage.get(), &std_found) {
            units::remap_module_facts(&mut cx, &|m| o.in_std(m));
        }
        let cx_root = cx.root;
        let entry = timings::pass(round, || analyze(&mut cx, cx_root, root_mod, opts));
        global_this::drop_site_errors(&mut cx);
        if let Some(o) = overloaded {
            overloads::report_misfits(&mut cx.diags, &o.sig_spans);
            overload_adapter::check_identity(&mut cx, &o.adapted);
        }
        std_load::report_added(&mut cx);
        (cx, entry)
    };
    // A check with error-set variables that reports anything is checked again without them,
    // for its diagnostics (`FnSites::doubted`); not one that refuses script variables it then
    // moves into `main`, which is checked again anyway (`script_locals`).
    let given = rw.merge_diags.len() + rw.closures.as_ref().map_or(0, |c| c.diags.len());
    let doubt = |cx: &ctx::Ctx| {
        let moves = script_locals::moves_more(cx, &modules[root].ast);
        cx.diags.len() > given && !moves && !throws::first_check_only()
    };
    // The check checked again, kept while that one runs: when that one reports an error this
    // one did not, this one's diagnostics stay the program's (`throws::adds_errors`).
    if order.is_some() {
        // SAFETY: `disk_arena` is declared before every context and so outlives them.
        let loaded = disk_loaded.map(|(cx, _)| cx);
        let std_cx = match loaded.or_else(|| std_unit::take(&held, std_key)) {
            Some(mut cx) => {
                (cx.units, cx.units_stage, cx.stack_budget) =
                    (true, units::Stage::Std, stack_budget);
                cx.std_from_disk = from_disk;
                cx
            }
            None => {
                stage.set(units::Stage::Std);
                let run = rounds::run(&new_cx, &|_: &ctx::Ctx| false, &assumed, false, None);
                let mut std_cx = match run {
                    Ok((cx, _, _)) => cx,
                    Err(stopped) => return *stopped,
                };
                units::finish_unit(&mut std_cx);
                if let (Some(p), Some(k)) = (&disk, opts.std_key) {
                    units::disk::save(p, &std_cx, stubbed.as_deref(), k);
                }
                if let (Some(k), Some(c)) = (std_key, std_copies.take()) {
                    // SAFETY: std's stage was checked over `c` alone (`copies`), with no
                    // generator, loads or diagnostics of the check installed.
                    unsafe { std_unit::keep(&mut held, k, std_cx.clone(), c) };
                }
                std_cx
            }
        };
        #[cfg(test)]
        let std_cx = units::codec::hook::std_unit(std_cx);
        let std_cx = units::round_trip(std_cx);
        ctx::def_table::touch::begin(std_cx.unit_base as usize, std_cx.ty.table.len());
        *std_snap.borrow_mut() = Some(std_cx);
        stage.set(units::Stage::App);
    }
    let (cx, entry, generated) = match rounds::run(
        &new_cx,
        &doubt,
        &assumed,
        !recovered.is_empty(),
        generator.as_ref(),
    ) {
        Ok(done) => done,
        Err(stopped) => return *stopped,
    };
    // A kept check (`kept`) of a check in which no generator ran.
    let none_generated = generator.as_ref().is_none_or(|g| g.kept().next().is_none());
    let kept = (kept::wanted(input) && none_generated && order.is_none())
        .then(|| kept::detach(cx.clone()));
    let checked = (modules.as_ptr(), modules.len());
    units::touch_phase(&cx, "the app's check (rounds)", true);
    let (program, diags) = program::build_checked(cx, entry, generated);
    ctx::def_table::touch::phase("the program's build", false);
    if let Some(kept) = kept {
        let vec = |v: Option<Vec<SourceModule>>| -> kept::Stage {
            (v.as_ref().map(|m| m.as_ptr()), Box::new(v))
        };
        let stages = vec![
            vec(rw.chunks.modules),
            vec(rw.asserted.modules),
            vec(rw.destructured),
            vec(rw.flat),
            vec(rw.merged),
            vec(rw.callable),
            (
                rw.lifted.as_ref().map(|l| l.modules.as_ptr()),
                Box::new(rw.lifted),
            ),
            (
                rw.overloaded.as_ref().map(|o| o.modules.as_ptr()),
                Box::new(rw.overloaded),
            ),
            vec(rw.rewritten),
            vec(rw.globals),
            vec(rw.accessors),
            vec(rw.branded),
            (
                rw.closures.as_ref().map(|c| c.modules.as_ptr()),
                Box::new(rw.closures),
            ),
        ];
        let loads = loads.map(|l| l.kept.id());
        let sources = gen.map_or(&[][..], |g| g.sources);
        kept::offer(kept, input, sources, checked, stages, loads);
    }
    (
        (Some(Checked::Program(Box::new(program))), diags),
        Default::default(),
    )
}

/// How many times at most a program is checked again for the error types of function types
/// written without `throws`. The sets are propagated within one check's inference, so one more
/// check is usually enough (a `catch` that rethrows its variable throws what its `try` body's
/// sources throw in that inference, `body::expr::errors`, so a chain of such closures needs no
/// check per closure); another one is needed when the sets change what a check infers from them
/// before inference (an async function's rejection type, a `catch` variable narrowed by its
/// type).
pub(crate) const FN_SITE_ROUNDS: usize = 8;

/// Rounds of checking that add families of union views (`union_views`): one per family a
/// view is first made of in a `<view>` arm (converting a view to a wider union) at most; after
/// the last, a use without an arm is an error.
pub(crate) const VIEW_ROUNDS: usize = 4;

/// Steps 1–5 and the checks of the whole program: the entry point, if there is one.
fn analyze(
    cx: &mut ctx::Ctx,
    root: usize,
    root_mod: &SourceModule,
    opts: CheckOptions,
) -> Option<hir::DefId> {
    check_definitions(cx);
    // A check that keeps itself keeps this point too, once the generated code is checked: a
    // reload checks edited bodies again from here and runs the rest (`reanalyze`), the
    // generated code's check only when the edit demands other instances of it.
    if cx.body_records.is_some() {
        generated_phase(cx);
        // What the phase demanded last; nothing when it had no generated code to check.
        cx.kept_demand = cx.gen.binding.last_found.clone();
        kept::stash(cx);
        analyze_after_phase(cx);
    } else {
        analyze_after_bodies(cx);
    }
    // std's unit alone has no root of its own (`crate::units`), but checks its own aliases.
    let entry = (cx.units_stage != units::Stage::Std)
        .then(|| main_fn::check_main(cx, root, root_mod, opts.require_main))
        .flatten();
    alias_expand::check_unused_aliases(cx);
    entry
}

/// Should the program be checked again with held closures and `const me = this` sharing?
/// Borrowing in them only changes the cost, never which programs are accepted
/// (https://veltlang.com/docs/reference/functions/ "Captures"). So when some of them borrowed and a pass from
/// `demote_local_closures` on reported an error, the program is checked again with them
/// sharing, as it was before. Errors from the earlier passes (type errors) don't depend on
/// borrowing and never cause a second check. The CLI and the IDE both use this rule.
pub(crate) fn retry_sharing(cx: &ctx::Ctx) -> bool {
    cx.held_borrows_used && cx.borrow_pass_errors
}

pub(crate) fn error_count(cx: &ctx::Ctx) -> usize {
    cx.diags.iter().filter(|d| d.is_error()).count()
}

/// Steps 1–5: every definition and body checked ([`check_definitions`]), then ownership and
/// throws inferred and moves checked ([`analyze_after_bodies`]).
fn analyze_bodies(cx: &mut ctx::Ctx) {
    check_definitions(cx);
    analyze_after_bodies(cx);
    // EXPERIMENT (`VELT_UNITS=2`): the passes above ran with std frozen.
    cx.thaw_defs();
}

/// Every definition collected and every body checked.
fn check_definitions(cx: &mut ctx::Ctx) {
    if cx.units {
        units::check_definitions(cx);
        return;
    }
    timings::pass("script_tdz::check", || script_tdz::check(cx));
    timings::pass("collect::collect", || collect::collect(cx));
    cx.collected_defs = cx.defs.len();
    timings::pass("decorators::check", || decorators::check(cx, 0));
    timings::pass("global_this::declare", || global_this::declare(cx));
    timings::pass("build_fill_bodies", || {
        body::expr::virtual_defaults::build_fill_bodies(cx, 0)
    });
    timings::pass("body::check_bodies", || body::check_bodies(cx));
}

/// What follows the bodies' check: generated code, ownership and throws inferred, moves checked.
pub(crate) fn analyze_after_bodies(cx: &mut ctx::Ctx) {
    generated_phase(cx);
    analyze_after_phase(cx);
}

/// The generated code collected, checked and bound ([`generated::phase`]).
fn generated_phase(cx: &mut ctx::Ctx) {
    // Code is generated for a program (`crate::units`): std's unit holds no generated module,
    // so it outlives the check's generator; the app's stage generates for std's demands too.
    // It still walks its bodies for the demand (`generated::demand::Walked`): the app's copy
    // then walks only its own.
    if cx.units_stage == units::Stage::Std {
        timings::pass("generated::demand", || generated::demand::compute(cx));
        return;
    }
    timings::pass("generated::phase", || generated::phase::run(cx));
}

/// The instances of generated functions the checked code reaches, without where.
fn demanded(cx: &mut ctx::Ctx) -> Vec<(hir::DefId, Vec<hir::TyId>)> {
    let found = generated::demand::compute(cx);
    found.into_iter().map(|(g, targs, _)| (g, targs)).collect()
}

/// [`analyze_after_bodies`] after the generated phase.
fn analyze_after_phase(cx: &mut ctx::Ctx) {
    timings::pass("global_this::check_sites", || global_this::check_sites(cx));
    timings::pass("module_scripts::declare", || module_scripts::declare(cx));
    timings::pass("make_dynamic_impls", || {
        body::expr::dynamic_fn_views::make_dynamic_impls(cx)
    });
    timings::pass("check_adapter_writes", || cx.check_adapter_writes());
    timings::pass("check_adapted_array_reads", || {
        cx.check_adapted_array_reads()
    });
    timings::pass("absent_fields::check", || absent_fields::check(cx));
    timings::pass("fresh_returns::check", || fresh_returns::check(cx));
    timings::pass("generic_override_sites::check", || {
        generic_override_sites::check(cx)
    });
    timings::pass("generic_fns::mark_shared_values", || {
        generic_fns::mark_shared_values(cx)
    });
    // Growing generic recursion, and generic types that refer to themselves at a growing type
    // argument, have infinitely many instantiations: the passes below would never finish.
    if timings::pass("instantiation_cycles::check", || {
        instantiation_cycles::check(cx) || expansive_types::check(cx)
    }) {
        return;
    }
    // After the cycles: it carries calls and conversions through instantiations.
    timings::pass("any_iface_sites::check", || any_iface_sites::check(cx));
    // Before the passes that read instantiated types: deferred types reduced per instance.
    timings::pass("type_level::instances::reach", || {
        type_level::instances::reach(cx)
    });
    timings::pass("hooks::check_shapes", || hooks::check_shapes(cx));
    body::lazy_std::finish_live(cx);
    let before = error_count(cx);
    generated::bind::fix_modes(cx);
    timings::pass("ownership_passes", || ownership_passes(cx));
    if cx.fn_sites.again() {
        return;
    }
    cx.borrow_pass_errors = error_count(cx) > before;
    kept_consts::keep_readers(cx);
    // After `ownership_passes`: the JSON pass records the types `JSON.stringify` writes.
    // Conversions are all made: a check below widens no layout (`collect::extended_layouts`).
    cx.objects.layouts.sealed = true;
    layout_merge::check_looks(cx);
    timings::pass("object_copies::check", || object_copies::check(cx));
    timings::pass("replaced_holders::check", || replaced_holders::check(cx));
    cx.module_slots = module_state::order(cx);
    timings::pass("module_state::check_locked_access", || {
        module_state::check_locked_access(cx)
    });
}

/// The passes from `demote_local_closures` on: the ones whose errors may come from a held
/// closure or `const me = this` borrowing ([`retry_sharing`]).
fn ownership_passes(cx: &mut ctx::Ctx) {
    timings::pass("ownership::demote_local_closures", || {
        ownership::demote_local_closures(cx)
    });
    timings::pass("ownership::infer_local_async", || {
        ownership::infer_local_async(cx)
    });
    timings::pass("ownership::infer_modes", || ownership::infer_modes(cx));
    timings::pass("ownership::check_crossing_changes", || {
        ownership::check_crossing_changes(cx)
    });
    timings::pass("throws::infer_all", || throws::infer_all(cx));
    // The program is checked again with the error types of function types written without
    // `throws` that this check found (`throws::fn_types`): the rest would be redone.
    if cx.fn_sites.again() {
        return;
    }
    timings::pass("hooks::check_throws", || hooks::check_throws(cx));
    decorators::export_throws(cx);
    let uses = timings::pass("record_keys::check_instantiations", || {
        record_keys::check_instantiations(cx)
    });
    timings::pass("json::check_json_types", || json::check_json_types(cx));
    timings::pass("void_fields::check_instantiations", || {
        void_fields::check_instantiations(cx)
    });
    timings::pass("struct_tuples::check_instantiations", || {
        struct_tuples::check_instantiations(cx);
        struct_tuples::check_generic_bodies(cx, &uses);
        struct_tuples::check_field_writes(cx, &uses);
        struct_tuples::check_proxy_targets(cx, &uses)
    });
    timings::pass("undefined_args::check_instantiations", || {
        undefined_args::check_instantiations(cx)
    });
    timings::pass("ownership::soften_moves", || ownership::soften_moves(cx));
    timings::pass("ownership::validate_moves", || {
        ownership::validate_moves(cx)
    });
    let moved = timings::pass("moves::check_all", || moves::check_all(cx));
    timings::pass("ownership::clone_reused", || {
        ownership::clone_reused(cx, &moved.reused)
    });
    timings::pass("atomic_copies::check", || atomic_copies::check(cx));
    timings::pass("thread_locals::check", || thread_locals::check(cx));
    timings::pass("ownership::box_cells", || {
        ownership::box_cells(cx, &moved.boxed, &moved.per_iteration)
    });
    timings::pass("ownership::check_exclusive", || {
        ownership::check_exclusive(cx)
    });
    timings::pass("ownership::check_boundaries", || {
        ownership::check_boundaries(cx)
    });
    let weak = timings::pass("ownership::check_locked", || ownership::check_locked(cx));
    timings::pass("ownership::record_changed_captures", || {
        ownership::record_changed_captures(cx)
    });
    timings::pass("ownership::check_many_threads", || {
        ownership::check_many_threads(cx)
    });
    timings::pass("weak_threads::check", || weak_threads::check(cx, weak));
}
