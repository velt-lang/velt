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
mod analysis;
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
mod canon_key;
mod glue_reach;
pub mod ide;
mod iface_method_checks;
mod iface_method_writes;
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
mod object_widen;
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
mod rounds;
mod script_locals;
mod script_tdz;
mod source_module;
mod source_scans;
mod std_load;
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
pub use std_load::{LoadFn, Loads, PrebuiltFn, StdBatch, StdLoads};
pub use throws::closure_site_defs;
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
/// recovered modules (their syntax errors come first). Never builds a program. `loads` serves
/// the std modules the check demands, as for a check of a program that loaded cleanly: the load
/// left them to it (`velt_common::std_hooks::ON_DEMAND_MODULES`), and without it a `hookCall`
/// reached would report its hook as not loaded (#3150).
pub fn check_recovered(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
    loads: Option<Loads>,
) -> Diagnostics {
    generating::on_sema_thread(|budget| {
        check_on_current_thread(modules, root, opts, recovered, None, loads, budget).1
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
    let chunks = timings::pass("rewrite: module_chunks", || {
        module_chunks::split(modules, root)
    });
    let modules = chunks.modules.as_deref().unwrap_or(modules);
    let asserted = timings::pass("rewrite: asserted_targets", || {
        asserted_targets::rewrite(modules)
    });
    let modules = asserted.modules.as_deref().unwrap_or(modules);
    let mut merge_diags = chunks.diags.clone();
    merge_diags.extend(timings::pass("rewrite: call_sigs::check", || {
        call_sigs::check(modules)
    }));
    let destructured = timings::pass("rewrite: module_destructure", || {
        module_destructure::rewrite(modules)
    });
    let modules = destructured.as_deref().unwrap_or(modules);
    let global_ns = timings::pass("rewrite: global_namespaces", || {
        namespaces::global_namespaces(modules)
    });
    let (flat, ns_private, diags) = timings::pass("rewrite: namespaces::flatten", || {
        namespaces::flatten(modules)
    });
    merge_diags.extend(diags);
    let modules = flat.as_deref().unwrap_or(modules);
    let (merged, diags) = timings::pass("rewrite: interface_merge", || {
        interface_merge::rewrite(modules)
    });
    merge_diags.extend(diags);
    let modules = merged.as_deref().unwrap_or(modules);
    let (callable, diags) = timings::pass("rewrite: call_sigs", || call_sigs::rewrite(modules));
    merge_diags.extend(diags);
    let modules = callable.as_deref().unwrap_or(modules);
    let lifted = timings::pass("rewrite: generic_arrows", || generic_arrows::lift(modules));
    let modules = lifted.as_ref().map_or(modules, |l| &l.modules[..]);
    let overloaded = timings::pass("rewrite: overloads", || overloads::rewrite(modules));
    let modules = overloaded.as_ref().map_or(modules, |o| &o.modules[..]);
    let (rewritten, alias_blocked) = timings::pass("rewrite: recursive_aliases", || {
        recursive_aliases::rewrite(modules)
    });
    let modules = rewritten.as_deref().unwrap_or(modules);
    let (global_props, global_diags, globals) =
        timings::pass("rewrite: global_this", || global_this::rewrite(modules));
    merge_diags.extend(global_diags);
    let modules = globals.as_deref().unwrap_or(modules);
    let (accessor_diags, accessors) = timings::pass("rewrite: defined_accessors", || {
        defined_accessors::rewrite(modules)
    });
    merge_diags.extend(accessor_diags);
    let modules = accessors.as_deref().unwrap_or(modules);
    let (brands, branded) =
        timings::pass("rewrite: defined_props", || defined_props::rewrite(modules));
    let modules = branded.as_deref().unwrap_or(modules);
    let closures = timings::pass("rewrite: nested_closures", || {
        nested_closures::rewrite(modules)
    });
    let modules = closures.as_ref().map_or(modules, |c| &c.modules[..]);
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
        let lifted = lifted
            .as_ref()
            .map(|l| (&l.local_fns, &l.all_fns, &l.unsafe_fns));
        let closures = closures.as_ref().map(|c| (&c.diags, &c.kept, &c.groups));
        let rest: [&dyn std::fmt::Debug; 16] = [
            &null_proto,
            &ctor_reads,
            &record_accessors,
            &gen_throw,
            &merge_diags,
            &global_ns,
            &ns_private,
            &alias_blocked,
            &lifted,
            &overloaded.as_ref().map(|o| &o.methods),
            &brands,
            &global_props,
            &chunks.promoted,
            &asserted.rmw,
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
    // std's modules first, which std's unit is checked over alone, then the app's.
    let mut order = (opts.units && units::stage2()).then(|| units::Order::of(modules, root));
    // Where std's unit is kept on disk, when it is (`units::disk`): not for a program that
    // changes what std's stage makes of std.
    let disk = (order.as_ref())
        .and_then(|_| {
            units::disk::path(opts, [null_proto, record_accessors, ctor_reads, gen_throw])
        })
        .filter(|_| !units::disk::shaped_by_program(input, &brands, &global_ns));
    let std_snap: std::cell::RefCell<Option<ctx::Ctx>> = Default::default();
    // What every context of this check holds of the program, which a unit does not save
    // (`units::codec::unit`'s `skip`): the rewrites' facts, the generator, the loads. Installed in
    // a fresh context and in std's unit loaded from disk.
    let install = on_cx(|cx| {
        if opts.eager_std {
            cx.lazy.on = false;
        }
        cx.units = opts.units;
        cx.lazy.native_executor = opts.native_executor;
        cx.loads = loads;
        generating::install(cx, gen, generator.as_ref());
        cx.stack_budget = stack_budget;
        // A check that keeps itself records its bodies, for the kept check to check them again.
        cx.body_records = kept::wanted(input).then(Default::default);
        cx.source_facts = facts.clone();
        cx.ns_private = ns_private.clone();
        cx.global_namespaces = global_ns.clone();
        cx.gen_throw = gen_throw;
        cx.alias_blocked = alias_blocked.clone();
        if let Some(l) = &lifted {
            cx.generic_arrow_fns = l.local_fns.clone();
            cx.generic_arrow_all = l.all_fns.clone();
            cx.unsafe_arrow_fns = l.unsafe_fns.clone();
        }
        if let Some(o) = &overloaded {
            cx.has_overloads = true;
            cx.overloaded_methods = o.methods.clone();
        }
        cx.objects.null_proto_records = null_proto;
        cx.objects.record_accessors = record_accessors;
        cx.objects.ctor_reads = ctor_reads;
        cx.objects.brands = brands.clone();
        cx.global_props = global_props.clone();
        cx.recovered = (recovered.iter())
            .map(|&(m, src)| (m, collect::exports::words(src)))
            .collect();
        cx.promoted_consts = chunks.promoted.clone();
        cx.asserted_rmw = asserted.rmw.clone();
        if let Some(c) = &closures {
            cx.kept_nested = c.kept.clone();
            cx.set_closure_groups(&c.groups);
        }
    });
    // The rewrites' diagnostics: the program's, so not std's unit's (a unit on disk serves other
    // programs); the app's copy of it takes them.
    let rewrite_diags = on_cx(|cx| {
        cx.diags.extend(merge_diags.iter().cloned());
        if let Some(c) = &closures {
            cx.diags.extend(c.diags.iter().cloned());
        }
    });
    // std's unit from disk, with the program's facts installed as in a fresh context. Its AST
    // lives as long as the process: the program built from the check keeps references to it.
    let loaded_std = match (&disk, order.take()) {
        (Some(path), Some(o)) => {
            // The arena of a unit that loads is never freed: left as the raw pointer it is read
            // through, so the box is not moved while the context borrows from it.
            let arena = Box::into_raw(Box::<units::codec::Arena>::default());
            // SAFETY: `arena` is the box made above, alive for the load and, when the unit
            // loads, for the rest of the process.
            match unsafe { units::disk::load(path, &*arena, &o.std_modules(modules)) } {
                Some((mut cx, at)) => {
                    let o = o.reorder_std(&at);
                    // std's stage first: `install` adds no generated module to std's unit.
                    cx.units_stage = units::Stage::Std;
                    install(&mut cx);
                    units::remap_module_facts(&mut cx, &|m| o.in_std(m));
                    cx.unit_base = cx.defs.len() as u32;
                    order = Some(o);
                    Some(cx)
                }
                None => {
                    // SAFETY: a unit that does not load gives no context, so nothing borrows
                    // the arena: the box made above is taken back and dropped, with whatever
                    // nodes a damaged unit decoded before it was refused.
                    drop(unsafe { Box::from_raw(arena) });
                    order = Some(o);
                    None
                }
            }
        }
        (_, o) => {
            order = o;
            None
        }
    };
    let new_cx = |round: &'static str,
                  held_borrows: bool,
                  seeds: &throws::FnTypeSeeds,
                  last: bool,
                  views: &union_views::Families| {
        timings::note_round();
        let from_std = match stage.get() {
            units::Stage::App => timings::pass("units: copy std", || std_snap.borrow().clone()),
            _ => None,
        };
        let fresh = from_std.is_none();
        let mut cx = match (from_std, &order) {
            (Some(cx), _) => cx,
            (None, Some(o)) if stage.get() == units::Stage::Std => {
                ctx::Ctx::over(o.std_modules(modules), 0)
            }
            (None, _) => ctx::Ctx::new(modules, root),
        };
        cx.units_stage = stage.get();
        if !fresh {
            cx.union_views.assumed = views.clone();
            cx.fn_sites.reseed(seeds.clone(), last);
            cx.held_borrows = held_borrows;
            // The app's modules after std's unit, with what the rewrites found of them.
            if let Some(o) = &order {
                o.add_app(&mut cx, modules);
                let base = o.app_base(&cx);
                cx.global_props = global_props.clone();
                cx.objects.brands = brands.clone();
                cx.ns_private = ns_private.clone();
                cx.global_namespaces = global_ns.clone();
                cx.recovered = (recovered.iter())
                    .map(|&(m, src)| (m, collect::exports::words(src)))
                    .collect();
                units::remap_module_facts(&mut cx, &|m| o.in_app(m, base));
                rewrite_diags(&mut cx);
                // The generated modules handed in apart, which std's unit does not hold.
                generating::add_sources(&mut cx, gen);
            }
        }
        if fresh {
            cx.units_stage = stage.get();
            install(&mut cx);
            body::lazy_std::assume(&mut cx, &assumed.borrow());
            cx.union_views.assumed = views.clone();
            cx.fn_sites = throws::FnSites::new(seeds.clone(), last);
            cx.held_borrows = held_borrows;
            if stage.get() != units::Stage::Std {
                rewrite_diags(&mut cx);
            }
        }
        if let (true, Some(o), units::Stage::Std) = (fresh, &order, stage.get()) {
            units::remap_module_facts(&mut cx, &|m| o.in_std(m));
        }
        let cx_root = cx.root;
        let entry = timings::pass(round, || analyze(&mut cx, cx_root, root_mod, opts));
        global_this::drop_site_errors(&mut cx);
        if let Some(o) = &overloaded {
            overloads::report_misfits(&mut cx.diags, &o.sig_spans);
            overload_adapter::check_identity(&mut cx, &o.adapted);
        }
        std_load::report_added(&mut cx);
        (cx, entry)
    };
    // A check with error-set variables that reports anything is checked again without them,
    // for its diagnostics (`FnSites::doubted`); not one that refuses script variables it then
    // moves into `main`, which is checked again anyway (`script_locals`).
    let given = merge_diags.len() + closures.as_ref().map_or(0, |c| c.diags.len());
    let doubt = |cx: &ctx::Ctx| {
        let moves = script_locals::moves_more(cx, &cx.modules[cx.root].ast);
        cx.diags.len() > given && !moves && !throws::first_check_only()
    };
    // The check checked again, kept while that one runs: when that one reports an error this
    // one did not, this one's diagnostics stay the program's (`throws::adds_errors`).
    if opts.units && units::stage2() {
        let std_cx = match loaded_std {
            Some(cx) => cx,
            None => {
                stage.set(units::Stage::Std);
                let run = rounds::run(&new_cx, &|_: &ctx::Ctx| false, &assumed, false, None);
                let mut std_cx = match run {
                    Ok((cx, _, _)) => cx,
                    Err(stopped) => return *stopped,
                };
                units::finish_std(&mut std_cx);
                if let Some(path) = &disk {
                    units::disk::save(path, &std_cx);
                }
                std_cx
            }
        };
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
    // Not with std as a unit of its own: a kept check reloads over the load's module order.
    let kept = (kept::wanted(input) && none_generated && order.is_none())
        .then(|| kept::detach(cx.clone()));
    let checked = (modules.as_ptr(), modules.len());
    let (program, diags) = program::build_checked(cx, entry, generated);
    if let Some(kept) = kept {
        let vec = |v: Option<Vec<SourceModule>>| -> kept::Stage {
            (v.as_ref().map(|m| m.as_ptr()), Box::new(v))
        };
        let stages = vec![
            vec(chunks.modules),
            vec(asserted.modules),
            vec(destructured),
            vec(flat),
            vec(merged),
            vec(callable),
            (
                lifted.as_ref().map(|l| l.modules.as_ptr()),
                Box::new(lifted),
            ),
            (
                overloaded.as_ref().map(|o| o.modules.as_ptr()),
                Box::new(overloaded),
            ),
            vec(rewritten),
            vec(globals),
            vec(accessors),
            vec(branded),
            (
                closures.as_ref().map(|c| c.modules.as_ptr()),
                Box::new(closures),
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

pub(crate) use analysis::{
    analyze, analyze_after_phase, analyze_bodies, demanded, error_count, retry_sharing,
};

/// `f`, a function of one check's contexts (their modules' lifetime fixed: a closure's own
/// parameter would get a lifetime of its own).
fn on_cx<'m, F: Fn(&mut ctx::Ctx<'m>)>(f: F) -> F {
    f
}
