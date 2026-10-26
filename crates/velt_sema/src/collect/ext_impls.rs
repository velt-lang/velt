//! `extend<T> X implements I<…> { … }` (retroactive implements): the block's methods implement
//! `I` for every type its target matches, so `X` satisfies `I` as a bound and its values convert
//! to `I` as a class's would. Each block and interface becomes an entry in `Program::impls`
//! (generic when the block is), recorded in `Ctx::ext_impls`; its type parameters' bounds go to
//! `Program::extension_impls`, so an impl fits a type only where they hold.
//!
//! A type's own implementation (its class's, or a base class's) comes first; a type without one
//! gets the most specific block that fits it (`Ctx::find_impl`, infer/ext_impls.rs; velt_vir
//! `impl_for`), by the rule that picks among extension methods (target an instance of the
//! other's, then bounds over none). A blanket block (`extend<T extends Comparable<T>> T
//! implements InlineKey`) gives every type that meets its bounds an impl, type parameters
//! included (`Ctx::blanket_bounds`), and a block for one type (`extend u64 implements
//! InlineKey`) replaces it for that type. So that the most specific block is unique, two blocks
//! that can fit one type where neither is more specific are an error, as is a block for an
//! interface its target already implements.
//!
//! Refused targets: `any`, `unknown` (and the other dynamic types) and `never`, which have no
//! implementations of their own; unions, whose values have a member's type; an interface type
//! implementing an interface it extends; and a blanket block bounded by the interface it
//! implements, which no type could get.

use velt_common::Diagnostic;

use super::impls::check_method_sig;
use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, ImplBound, ImplDef, TyId, TyKind};

pub(super) fn build(cx: &mut Ctx) {
    for e in 0..cx.extensions.len() {
        let implements = cx.extensions[e].implements.clone();
        if implements
            .first()
            .is_some_and(|&(_, span)| !bounds_ok(cx, e, span))
        {
            continue;
        }
        for (b, span) in implements {
            if !target_ok(cx, e, &b, span) {
                continue;
            }
            let Some(imp) = check(cx, e, &b, span) else {
                continue;
            };
            let (iface, i) = (imp.iface, cx.impls.len() as u32);
            if let Some((other, args, _)) = cx.own_impl(imp.ty, iface) {
                let def = cx.impls[other as usize].methods.first().copied();
                let theirs = bound_name(cx, e, iface, &args);
                report_conflict(cx, e, &b, span, (def, theirs), None);
                continue;
            }
            cx.impls.push(imp);
            cx.ext_impls.insert(i, e);
            if let Some((other, witness)) = overlapping(cx, i) {
                let o = cx.impls[other as usize].clone();
                let theirs = bound_name(cx, cx.ext_impls[&other], iface, &o.iface_args);
                report_conflict(
                    cx,
                    e,
                    &b,
                    span,
                    (o.methods.first().copied(), theirs),
                    Some(witness),
                );
                cx.ext_impls.remove(&i);
                cx.impls.pop();
                continue;
            }
            cx.ext_by_iface.entry(iface).or_default().push(i);
        }
    }
}

/// Are the bounds of extension `e` (which lists `implements`, at `span`) interfaces an impl
/// lookup can check? A dynamic one (`T extends unknown`, `object`, `Function`, `any`) or a type
/// one (`T extends string`) is refused.
fn bounds_ok(cx: &mut Ctx, e: usize, span: velt_common::Span) -> bool {
    let g = &cx.extensions[e].generics;
    let dynamic = cx.any.map(|d| [d.any, d.unknown, d.object, d.function]);
    let bad = g.names.iter().enumerate().find_map(|(k, name)| {
        let dyn_bound = g
            .bounds
            .get(k)
            .into_iter()
            .flatten()
            .find(|b| dynamic.is_some_and(|ds| ds.contains(&b.iface)));
        match (dyn_bound, g.constraints.get(k).copied().flatten()) {
            (Some(b), _) => Some((name.clone(), Some(b.iface), None)),
            (None, Some(t)) => Some((name.clone(), None, Some(t))),
            _ => None,
        }
    });
    let Some((name, iface, constraint)) = bad else {
        return true;
    };
    let what = match (iface, constraint) {
        (Some(d), _) => cx.iface(d).map(|i| i.name.clone()).unwrap_or_default(),
        (None, Some(t)) => {
            let names = cx.extensions[e].generics.names.clone();
            let saved = std::mem::replace(&mut cx.display_params, names);
            let tn = cx.display(t);
            cx.display_params = saved;
            tn
        }
        _ => String::new(),
    };
    cx.error(
        Diagnostic::error(
            format!("an `extend` block that lists `implements` cannot bound `{name}` by `{what}`"),
            span,
        )
        .with_note("its bounds must be interfaces a type implements (`T extends Comparable<T>`); leave out a bound of `any` or `unknown`, which every type meets"),
    );
    false
}

/// Can extension `e` implement `b` for its target? Reports why not.
fn target_ok(cx: &mut Ctx, e: usize, b: &Bound, span: velt_common::Span) -> bool {
    let target = cx.extensions[e].target;
    let tn = target_name(cx, e);
    let iname = cx
        .iface(b.iface)
        .map(|i| i.name.clone())
        .unwrap_or_default();
    let why = if matches!(cx.ty.kind(target), TyKind::Never) {
        Some("`never` has no values".to_string())
    } else if cx.dynamic(target).is_some() {
        Some(format!(
            "`{tn}` has no implementations of its own: a value of it holds a value of another type, which implements `{iname}` or not"
        ))
    } else if cx.union_def(target).is_some() || matches!(cx.ty.kind(target), TyKind::Option(..)) {
        Some(format!(
            "`{tn}` is a union, and a value of it has the type of one of its members: implement `{iname}` for each member"
        ))
    } else {
        None
    };
    if let Some(why) = why {
        cx.err(
            format!("`extend {tn}` cannot implement `{iname}`: {why}"),
            span,
        );
        return false;
    }
    if let TyKind::Dyn(d, args) = cx.ty.kind(target).clone() {
        if cx.extends_iface(&Bound { iface: d, args }, b) {
            cx.err(format!("`{tn}` already implements `{iname}`"), span);
            return false;
        }
    }
    if let TyKind::Param(k) = *cx.ty.kind(target) {
        let bounds = cx.extensions[e].generics.bounds[k as usize].clone();
        if bounds.iter().any(|x| cx.extends_iface(x, b)) {
            cx.error(
                Diagnostic::error(
                    format!("`extend {tn}` implements `{iname}` only for types that already implement it"),
                    span,
                )
                .with_note(format!("`{tn}` is bounded by `{iname}`, and a type's own implementation comes first: this block never applies")),
            );
            return false;
        }
    }
    true
}

/// The impl of `b` by extension `e`, after checking its methods against the interface's.
fn check(cx: &mut Ctx, e: usize, b: &Bound, span: velt_common::Span) -> Option<ImplDef> {
    let (target, n) = (cx.extensions[e].target, cx.extensions[e].generics.len());
    let iface = cx.iface(b.iface)?;
    let (iname, imethods) = (iface.name.clone(), iface.methods.clone());
    let field = iface
        .fields
        .first()
        .map(|f| (f.name.clone(), f.optional, f.declared, f.span));
    let tn = target_name(cx, e);
    if let Some((name, optional, declared, at)) = field {
        let declared = cx.ty.core(declared).unwrap_or(declared);
        let what = match (optional, cx.ty.kind(declared)) {
            (true, TyKind::FnPtr { .. }) => format!("optional method `{name}`, which is a field"),
            (true, _) => format!("optional field `{name}`"),
            _ => format!("fields (`{name}`)"),
        };
        cx.error(
            Diagnostic::error(
                format!(
                    "`extend {tn}` cannot implement `{iname}`: `{iname}` declares {what}, and an `extend` block cannot add fields"
                ),
                span,
            )
            .with_label(at, "declared here"),
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

/// An earlier `extend` impl of the same interface that can fit a type the new impl `i` fits
/// where neither is more specific, with that type (`None` when it names a type parameter).
fn overlapping(cx: &mut Ctx, i: u32) -> Option<(u32, Option<TyId>)> {
    let iface = cx.impls[i as usize].iface;
    let earlier = cx.ext_by_iface.get(&iface).cloned().unwrap_or_default();
    for j in earlier {
        let Some(witness) = cx.ext_overlap(i, j) else {
            continue;
        };
        if !cx.ext_more_specific(i, j) && !cx.ext_more_specific(j, i) {
            return Some((j, witness));
        }
    }
    None
}

/// `other`: the other implementation's first method and its interface with arguments.
/// `overlap`: `None` for a type that already implements the interface, else the type two
/// blocks fit, if it names no type parameter.
fn report_conflict(
    cx: &mut Ctx,
    e: usize,
    b: &Bound,
    span: velt_common::Span,
    (other, theirs): (Option<DefId>, String),
    overlap: Option<Option<TyId>>,
) {
    let ours = bound_name(cx, e, b.iface, &b.args);
    let mut d = match overlap {
        Some(witness) => {
            let fits = match witness {
                Some(t) => format!(" (both fit `{}`)", cx.display(t)),
                None => String::new(),
            };
            let both = match ours == theirs {
                true => format!("`{ours}`"),
                false => format!("`{ours}` and `{theirs}`"),
            };
            Diagnostic::error(
                format!("conflicting implementations of {both}: neither block is more specific than the other{fits}"),
                span,
            )
            .with_note("a type gets the most specific implementation that fits it, with that block's interface arguments: make one block's target or bounds narrower, or remove one")
        }
        None => {
            let tn = target_name(cx, e);
            let mut d = Diagnostic::error(format!("`{tn}` already implements `{theirs}`"), span);
            if ours != theirs {
                d = d.with_note(format!(
                    "a type implements an interface once, so it cannot also implement `{ours}`"
                ));
            }
            d
        }
    };
    if let Some(def) = other {
        d = d.with_label(cx.fn_info(def).name_span, "the other implementation");
    }
    cx.error(d);
}

/// `iface<args>` as written in extension `e` (its type parameters by name).
fn bound_name(cx: &mut Ctx, e: usize, iface: DefId, args: &[TyId]) -> String {
    let names = cx.extensions[e].generics.names.clone();
    let saved = std::mem::replace(&mut cx.display_params, names);
    let t = cx.ty.intern(TyKind::Dyn(iface, args.to_vec()));
    let s = cx.display(t);
    cx.display_params = saved;
    s
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
