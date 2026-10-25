//! `extend<T> X implements I<…> { … }` (retroactive implements): the block's methods implement
//! `I` for every type its target matches, so `X` satisfies `I` as a bound and its values convert
//! to `I` as a class's would. Each block and interface becomes an entry in `Program::impls`
//! (generic when the block is), recorded in `Ctx::ext_impls`; its type parameters' bounds go to
//! `Program::impl_bounds`, so an impl fits a type only where they hold.
//!
//! Several blocks may implement one interface for overlapping targets: a type gets the most
//! specific of those that fit it (`Ctx::find_impl`, velt_vir `find_impl`), by the rule that picks
//! among extension methods (target instance of the other's, then bounds over none). A blanket
//! block (`extend<T extends Comparable<T>> T implements InlineKey`) gives every type that meets
//! its bounds an impl, type parameters included (`Ctx::blanket_bounds`), and a block for one type
//! (`extend u64 implements InlineKey`) replaces it for that type. Two blocks that are equally
//! specific are an error, as is a block for an interface the type already implements.

use velt_common::Diagnostic;

use super::impls::check_method_sig;
use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, ImplBound, ImplDef, TyId};

pub(super) fn build(cx: &mut Ctx) {
    for e in 0..cx.extensions.len() {
        for (b, span) in cx.extensions[e].implements.clone() {
            if let Some(imp) = check(cx, e, &b, span) {
                if let Some((other, from_block)) = conflict(cx, e, &imp) {
                    report_conflict(cx, e, &b, span, other, from_block);
                    continue;
                }
                cx.impls.push(imp);
                cx.ext_impls.insert(cx.impls.len() as u32 - 1, e);
            }
        }
    }
}

/// The impl of `b` by extension `e`, after checking its methods against the interface's.
fn check(cx: &mut Ctx, e: usize, b: &Bound, span: velt_common::Span) -> Option<ImplDef> {
    let (target, n) = (cx.extensions[e].target, cx.extensions[e].generics.len());
    let iface = cx.iface(b.iface)?;
    let (iname, imethods, has_fields) = (
        iface.name.clone(),
        iface.methods.clone(),
        !iface.fields.is_empty(),
    );
    let tn = target_name(cx, e);
    if has_fields {
        cx.err(
            format!(
                "`extend {tn}` cannot implement `{iname}`: `{iname}` declares fields, and an `extend` block cannot add fields"
            ),
            span,
        );
        return None;
    }
    let owner_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let mut full_args = b.args.clone();
    full_args.push(target);
    let mut methods = vec![];
    for m in &imethods {
        let own = cx.extensions[e]
            .methods
            .get(&m.name)
            .copied()
            .filter(|r| !r.is_static);
        match (own, m.default) {
            (Some(r), _) => {
                check_method_sig(cx, r.def, &owner_args, m, &full_args, &iname);
                // Called through `Callee::ParamMethod`: the interface's borrow ABI.
                cx.fn_info_mut(r.def).fixed_modes = true;
                methods.push(r.def);
            }
            (None, Some(def)) => methods.push(def),
            (None, None) => {
                let mut d = Diagnostic::error(
                    format!(
                        "`extend {tn}` is missing method `{}` required by `{iname}`",
                        m.name
                    ),
                    span,
                )
                .with_label(m.span, "declared here");
                if let Some(r) = cx.extensions[e].methods.get(&m.name).copied() {
                    let at = cx.fn_info(r.def).name_span;
                    d = d
                        .with_label(at, "a static method")
                        .with_note("a static method does not implement an interface method");
                }
                cx.error(d);
                return None;
            }
        }
    }
    Some(ImplDef {
        ty: target,
        generics: n as u32,
        iface: b.iface,
        iface_args: b.args.clone(),
        methods,
    })
}

/// The impl `imp` (of extension `e`) clashes with, as its first method (`None` for an impl
/// without methods) and whether it is an `extend` block's: an impl of the same interface for
/// the same target from a class's `implements` (or an implicit `Comparable` / `Iterable` one),
/// or an earlier block that is exactly as specific (the same target pattern, both with bounds or
/// both without).
fn conflict(cx: &mut Ctx, e: usize, imp: &ImplDef) -> Option<(Option<DefId>, bool)> {
    let bounded = has_bounds(cx, e);
    for (i, other) in cx.impls.iter().enumerate() {
        if other.iface != imp.iface || other.iface_args.len() != imp.iface_args.len() {
            continue;
        }
        if other.ty != imp.ty || other.generics != imp.generics {
            continue;
        }
        let (clashes, from_block) = match cx.ext_impls.get(&(i as u32)) {
            Some(&o) => (has_bounds(cx, o) == bounded, true),
            None => (true, false),
        };
        if clashes {
            return Some((other.methods.first().copied(), from_block));
        }
    }
    None
}

fn report_conflict(
    cx: &mut Ctx,
    e: usize,
    b: &Bound,
    span: velt_common::Span,
    other: Option<DefId>,
    from_block: bool,
) {
    let iname = cx
        .iface(b.iface)
        .map(|i| i.name.clone())
        .unwrap_or_default();
    let mut d = if from_block {
        Diagnostic::error(
            format!("conflicting implementations of `{iname}`: another one is exactly as specific"),
            span,
        )
        .with_note("a type gets the most specific implementation that fits it: make one block's target or bounds narrower, or remove one")
    } else {
        let tn = target_name(cx, e);
        Diagnostic::error(format!("`{tn}` already implements `{iname}`"), span)
    };
    if let Some(def) = other {
        d = d.with_label(cx.fn_info(def).name_span, "the other implementation");
    }
    cx.error(d);
}

/// Does extension `e` bound any of its type parameters?
pub(crate) fn has_bounds(cx: &Ctx, e: usize) -> bool {
    cx.extensions[e]
        .generics
        .bounds
        .iter()
        .any(|b| !b.is_empty())
}

fn target_name(cx: &mut Ctx, e: usize) -> String {
    let names = cx.extensions[e].generics.names.clone();
    let saved = std::mem::replace(&mut cx.display_params, names);
    let tn = cx.display(cx.extensions[e].target);
    cx.display_params = saved;
    tn
}

/// `Program::extension_impls`: each `extend … implements` impl with its type parameters' bounds.
pub(crate) fn hir_bounds(cx: &Ctx) -> rustc_hash::FxHashMap<u32, Vec<ImplBound>> {
    cx.ext_impls
        .iter()
        .map(|(&i, &e)| {
            let bounds = cx.extensions[e]
                .generics
                .bounds
                .iter()
                .enumerate()
                .flat_map(|(p, bs)| {
                    bs.iter().map(move |b| ImplBound {
                        param: p as u32,
                        iface: b.iface,
                        args: b.args.clone(),
                    })
                })
                .collect();
            (i, bounds)
        })
        .collect()
}
