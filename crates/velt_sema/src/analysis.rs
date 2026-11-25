//! What a check does after the source rewrites, in each round: every definition and body
//! checked, then the passes over the checked program (generated code, ownership, throws,
//! moves) and the whole program's checks ([`analyze`]).

use super::*;

/// Steps 1–5 and the checks of the whole program: the entry point, if there is one.
pub(crate) fn analyze(
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
    // std's unit alone has no root of its own (`crate::units`).
    if cx.units_stage == units::Stage::Std {
        return None;
    }
    // The app's passes ran with std's copy frozen, so a write to std is reported
    // (`velt-units:`); what follows builds the program, which writes every definition, as
    // after a check of both units (`units::check_definitions`).
    if cx.units_stage == units::Stage::App {
        cx.thaw_defs();
        // What the app's passes raised of std's definitions, written as a whole-program
        // check had written it during them (`units::raised`).
        units::raised::apply(cx);
    }
    let entry = main_fn::check_main(cx, root, root_mod, opts.require_main);
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
pub(crate) fn analyze_bodies(cx: &mut ctx::Ctx) {
    check_definitions(cx);
    analyze_after_bodies(cx);
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
fn analyze_after_bodies(cx: &mut ctx::Ctx) {
    generated_phase(cx);
    analyze_after_phase(cx);
}

/// The generated code collected, checked and bound ([`generated::phase`]).
fn generated_phase(cx: &mut ctx::Ctx) {
    // Code is generated for a program (`crate::units`): std's unit holds no generated module
    // (`generating::install` adds the ones the driver hands in to the app's copies), so it does not depend
    // on the check's generator; the app's stage generates for std's demands too.
    if cx.units_stage == units::Stage::Std {
        return;
    }
    timings::pass("generated::phase", || generated::phase::run(cx));
}

/// The instances of generated functions the checked code reaches, without where.
pub(crate) fn demanded(cx: &mut ctx::Ctx) -> Vec<(hir::DefId, Vec<hir::TyId>)> {
    let found = generated::demand::compute(cx);
    found.into_iter().map(|(g, targs, _)| (g, targs)).collect()
}

/// [`analyze_after_bodies`] after the generated phase.
pub(crate) fn analyze_after_phase(cx: &mut ctx::Ctx) {
    timings::pass("global_this::check_sites", || global_this::check_sites(cx));
    timings::pass("module_scripts::declare", || module_scripts::declare(cx));
    timings::pass("make_dynamic_impls", || {
        body::expr::dynamic_fn_views::make_dynamic_impls(cx)
    });
    // Every implementation exists now: those of interfaces whose methods are written get their
    // method setters (`iface_method_writes`).
    cx.finish_method_setters();
    timings::pass("iface_method_values::check", || {
        crate::iface_method_values::check(cx)
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
    units::app_reads_shared(cx);
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
