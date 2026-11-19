//! The passes that follow the bodies' check, in their order: the generated code's phase, the
//! checks of the whole program, then ownership and throws inferred and moves checked.

use crate::{
    absent_fields, any_iface_sites, atomic_copies, body, ctx, decorators, error_count,
    expansive_types, fresh_returns, generated, generic_fns, generic_override_sites, global_this,
    hir, hooks, instantiation_cycles, json, kept_consts, layout_merge, module_scripts,
    module_state, moves, object_copies, ownership, record_keys, replaced_holders, struct_tuples,
    thread_locals, throws, timings, type_level, undefined_args, units, void_fields, weak_threads,
};

/// What follows the bodies' check: generated code, ownership and throws inferred, moves checked.
pub(crate) fn analyze_after_bodies(cx: &mut ctx::Ctx) {
    generated_phase(cx);
    analyze_after_phase(cx);
}

/// The generated code collected, checked and bound ([`generated::phase`]).
pub(crate) fn generated_phase(cx: &mut ctx::Ctx) {
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
