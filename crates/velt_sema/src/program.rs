//! The checked context turned into the HIR program (step 6 of `crate`), and a debug check that a
//! clone of the context builds the same program (`VELT_SEMA_CLONE_CHECK`, below). The dev host
//! will keep a clone of the checked context to check edited bodies again in
//! (docs/internals/design/dev-warm-front-end.md step 3, "C. A kept checking context"); what a
//! clone shares with the original is listed on [`Ctx`].
//!
//! What the check shows is narrow: the clone holds everything the finishing steps read, and
//! nothing those steps change is shared between the clone and the original. It doesn't show that
//! a clone is independent in general: state shared through a cache keyed by `TyId` would pass it,
//! because both builds intern the same types in the same order. A body checked again in a clone
//! needs a check of its own (the design note, C3).

use velt_common::Diagnostics;

use crate::ctx::Ctx;
use crate::generated::bind::Bound;
use crate::type_level;
use crate::{anon, any, body, collect, ctx, finalize, hir, layout_merge, module_state, readonly};

/// With this variable set, every program is also built from a clone of the checked context and
/// compared with the one built from the context itself: a difference is an internal error that
/// names the part that differs and shows where, from both sides.
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
/// diagnostics) whose contents differ, maps and sets compared without their order, with the text
/// of each side around the first difference.
fn differs(a: &(hir::Program, Diagnostics), b: &(hir::Program, Diagnostics)) -> Option<String> {
    let ((pa, da), (pb, db)) = (a, b);
    let mut pa = parts(pa);
    let mut pb = parts(pb);
    pa.push(("diagnostics", format!("{da:?}")));
    pb.push(("diagnostics", format!("{db:?}")));
    let (x, y) = pa.iter().zip(&pb).find(|(x, y)| x.1 != y.1)?;
    let (from, to) = around_difference(&x.1, &y.1);
    Some(format!("{}\n  context: …{from}…\n  clone:   …{to}…", x.0))
}

/// The text of `a` and of `b` around the first character where they differ: up to `SHOWN` bytes
/// before it and after it, on character boundaries.
fn around_difference<'s>(a: &'s str, b: &'s str) -> (&'s str, &'s str) {
    const SHOWN: usize = 150;
    let at = (a.char_indices().zip(b.chars()))
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i);
    let window = |s: &'s str| {
        let mut lo = at.saturating_sub(SHOWN);
        while !s.is_char_boundary(lo) {
            lo -= 1;
        }
        let mut hi = (at + SHOWN).min(s.len());
        while !s.is_char_boundary(hi) {
            hi += 1;
        }
        &s[lo..hi]
    };
    (window(a), window(b))
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
        assigned_objects,
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
        fn_wrappers,
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
        ("assigned_objects", set(assigned_objects)),
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
        ("fn_wrappers", set(fn_wrappers)),
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
        assigned_objects,
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
        assigned_objects: assigned_objects.into_iter().collect(),
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
        fn_wrappers: generic_fns.wrappers.into_iter().collect(),
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

#[cfg(test)]
mod tests {
    use super::around_difference;

    #[test]
    fn the_message_shows_both_sides_at_the_first_difference() {
        let a = format!("{}Adt(7){}", "x".repeat(400), "y".repeat(400));
        let b = format!("{}Adt(8){}", "x".repeat(400), "y".repeat(400));
        let (from, to) = around_difference(&a, &b);
        assert!(from.contains("Adt(7)") && to.contains("Adt(8)"));
        assert!(from.len() <= 301 && to.len() <= 301);
        // A difference in length only, and multi-byte characters at the window's edges.
        let (from, to) = around_difference("ééé", "éééé");
        assert_eq!((from, to), ("ééé", "éééé"));
    }
}
