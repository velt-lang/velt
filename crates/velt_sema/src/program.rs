//! The checked context turned into the HIR program (step 6 of `crate`), and the debug check that
//! a clone of the context builds the same program (`VELT_SEMA_CLONE_CHECK`, below): the dev host
//! keeps a clone of the checked context to check edited bodies again in
//! (docs/internals/design/dev-warm-front-end.md step 3, "C. A kept checking context"), which is
//! only sound if the clone is a complete, independent copy.

use velt_common::Diagnostics;

use crate::ctx::Ctx;
use crate::generated::bind::Bound;
use crate::type_level;
use crate::{anon, any, body, collect, ctx, finalize, hir, layout_merge, module_state, readonly};

/// With this variable set, every program is also built from a clone of the checked context and
/// compared with the one built from the context itself: a difference is an internal error.
pub(crate) const CLONE_CHECK_ENV: &str = "VELT_SEMA_CLONE_CHECK";

/// The program of a check that reported no error: its definitions built and finished.
pub(crate) fn build_checked(
    cx: Ctx,
    entry: Option<hir::DefId>,
    generated: Bound,
) -> (hir::Program, Diagnostics) {
    if std::env::var_os(CLONE_CHECK_ENV).is_none() {
        return build(cx, entry, generated);
    }
    let twin = build(cx.clone(), entry, generated.clone());
    let out = build(cx, entry, generated);
    if let Some(what) = differs(&out, &twin) {
        panic!("ICE: a clone of the checked context built another program: {what}");
    }
    out
}

/// What differs between two programs: the first part (a field of the program, or the
/// diagnostics) whose contents differ, maps and sets compared without their order.
fn differs(a: &(hir::Program, Diagnostics), b: &(hir::Program, Diagnostics)) -> Option<String> {
    let ((pa, da), (pb, db)) = (a, b);
    let mut pa = parts(pa);
    let mut pb = parts(pb);
    pa.push(("diagnostics", format!("{da:?}")));
    pb.push(("diagnostics", format!("{db:?}")));
    (pa.iter().zip(&pb))
        .find(|(x, y)| x.1 != y.1)
        .map(|(x, _)| x.0.to_string())
}

/// The contents of each field of a program, as text. Destructured field by field: a new field
/// fails to build until it is compared here.
fn parts(p: &hir::Program) -> Vec<(&'static str, String)> {
    fn one(x: impl std::fmt::Debug) -> String {
        format!("{x:?}")
    }
    fn set<T: std::fmt::Debug>(xs: impl IntoIterator<Item = T>) -> String {
        let mut v: Vec<String> = xs.into_iter().map(|x| format!("{x:?}")).collect();
        v.sort_unstable();
        v.join("\n")
    }
    let hir::Program {
        types,
        defs,
        entry,
        impls,
        anon_shapes,
        union_shapes,
        counted_objects,
        null_proto_records,
        record_fills,
        fn_shapes,
        module_state,
        type_ops,
        any_impl,
        unknown,
        object,
        function,
        super_calls,
        dyn_members,
        array_methods_open,
        any_call_names,
        generator_throw,
        nothing_slots,
        upcasts,
        upcast_impls,
        cpu_targets,
        type_names,
        generic_dyn_methods,
        arg_free_generic_fns,
        fresh_closures,
        extension_impls,
        dyn_bounds,
        lock_free_callbacks,
        unchecked,
        hooks_demanded,
        live,
        any_fn_views,
        generated,
        decorators,
        c_exports,
    } = p;
    let n = types.len() as u32;
    let kinds: Vec<_> = (0..n).map(|t| types.kind(hir::TyId(t))).collect();
    vec![
        ("types", one(kinds)),
        ("defs", one(defs)),
        ("entry", one(entry)),
        ("impls", one(impls)),
        ("anon_shapes", set(anon_shapes)),
        ("union_shapes", set(union_shapes)),
        ("counted_objects", one(counted_objects)),
        ("null_proto_records", one(null_proto_records)),
        ("record_fills", one(record_fills)),
        ("fn_shapes", set(fn_shapes)),
        ("module_state", one(module_state)),
        ("type_ops", set(type_ops)),
        ("any_impl", one((any_impl, unknown, object, function))),
        ("super_calls", set(super_calls)),
        ("dyn_members", one(dyn_members.as_ref().map(set))),
        ("array_methods_open", one(array_methods_open)),
        ("any_call_names", one(any_call_names)),
        ("generator_throw", one((generator_throw, nothing_slots))),
        ("upcasts", set(upcasts)),
        ("upcast_impls", set(upcast_impls)),
        ("cpu_targets", set(cpu_targets)),
        ("type_names", set(type_names)),
        ("generic_dyn_methods", set(generic_dyn_methods)),
        ("arg_free_generic_fns", set(arg_free_generic_fns)),
        ("fresh_closures", set(fresh_closures)),
        ("extension_impls", set(extension_impls)),
        ("dyn_bounds", set(dyn_bounds)),
        ("lock_free_callbacks", set(lock_free_callbacks)),
        ("unchecked", set(unchecked)),
        ("hooks_demanded", one(hooks_demanded)),
        ("live", one(live.as_ref().map(set))),
        ("any_fn_views", one(any_fn_views)),
        ("generated", set(generated)),
        ("decorators", set(decorators)),
        ("c_exports", one(c_exports)),
    ]
}

/// [`build_checked`] without the clone check.
fn build(
    mut cx: Ctx,
    entry: Option<hir::DefId>,
    mut generated: Bound,
) -> (hir::Program, Diagnostics) {
    let unchecked = finalize::build_defs(&mut cx);
    let hooks_demanded = body::lazy_std::hooks_demanded(&cx);
    layout_merge::merge(&mut cx);
    let type_names = readonly::erase(&mut cx, &mut generated);
    cx.debug_distinct_variants();
    let module_state = module_state::build(&mut cx, entry);
    let type_ops = type_level::lowered_ops(&mut cx);
    let anon_shapes = anon::concrete_shapes(&cx);
    let union_shapes = anon::concrete_unions(&cx);
    let dyn_members = any::dyn_members(&mut cx);
    let (array_methods_open, any_call_names) = body::expr::proxy_method_values::finish(&mut cx);
    let (upcasts, upcast_impls) = cx.upcast_tables();
    let extension_impls = collect::ext_impls::hir_bounds(&cx);
    let dyn_bounds = collect::ext_impls::dyn_bounds(&mut cx);
    let live = body::lazy_std::live_defs(&mut cx, &unchecked);
    let ctx::Ctx {
        ty,
        defs,
        diags,
        impls,
        counted_objects,
        objects,
        any,
        gen_throw,
        super_calls,
        cpu_targets,
        generic_called,
        generic_fns,
        lock_free_callbacks,
        fn_views,
        fn_shapes,
        ..
    } = cx;
    let any_fn_views = fn_views.into_program();
    let program = hir::Program {
        types: ty.table,
        defs: (defs.into_iter())
            .map(|d| d.expect("ICE: definition left unfilled without an error"))
            .collect(),
        entry,
        impls,
        // The HIR contract keeps std maps; the checker builds Fx maps.
        anon_shapes: anon_shapes.into_iter().collect(),
        union_shapes: union_shapes.into_iter().collect(),
        counted_objects,
        fn_shapes: fn_shapes.into_iter().collect(),
        null_proto_records: objects.null_proto_records,
        record_fills: objects.record_fills,
        module_state,
        type_ops: type_ops.into_iter().collect(),
        any_impl: any.and_then(|a| a.imp),
        unknown: any.and_then(|a| a.imp.map(|_| a.unknown)),
        object: any.and_then(|a| a.imp.map(|_| a.object)),
        function: any.and_then(|a| a.imp.map(|_| a.function)),
        dyn_members: dyn_members.map(|m| m.into_iter().collect()),
        array_methods_open,
        any_call_names,
        generator_throw: gen_throw,
        nothing_slots: cx.nothing_slots,
        super_calls: super_calls.into_iter().collect(),
        upcasts,
        upcast_impls,
        cpu_targets: cpu_targets.into_iter().collect(),
        type_names,
        generic_dyn_methods: generic_called.names().collect(),
        arg_free_generic_fns: generic_fns.free_of_args.into_iter().collect(),
        fresh_closures: generic_fns.fresh.into_iter().collect(),
        extension_impls: extension_impls.into_iter().collect(),
        dyn_bounds: dyn_bounds.into_iter().collect(),
        lock_free_callbacks: lock_free_callbacks.into_iter().collect(),
        unchecked: unchecked.into_iter().collect(),
        hooks_demanded,
        live: live.map(|l| l.into_iter().collect()),
        any_fn_views,
        generated: generated.into_iter().collect(),
        decorators: std::mem::take(&mut cx.decorator_uses).into_iter().collect(),
        c_exports: std::mem::take(&mut cx.c_exports),
    };
    (program, diags)
}
