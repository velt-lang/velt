//! Conversions of an `any`, `unknown` or `object` to an interface type (`Intrinsic::AnyTo`) whose
//! generic method the program calls through interface values, when a subclass of a class
//! implementing the interface overrides that method (#943): the conversion uses the table of the
//! type the value was given to the `any` as, so a `Base` holding a `Sub` would run `Base`'s
//! method. Reported at the conversion, as that of a class value is
//! (`body::expr::iface_call` `check_generic_override_dyn`).
//!
//! Conversions are found in function bodies, the initializers of module constants and static
//! fields, and those of fields. One whose target is still a type parameter's (`u as T` in `conv<T>`) is carried, like
//! `generic_override_sites::check` carries calls through type parameters, to every site that
//! instantiates its definition (substituted, through the generic definitions in between, to a
//! fixed point), and reported at a site where the target has become such an interface.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::visit;

/// A unit of code and what it holds: its conversions (target type, where) and the definitions it
/// instantiates (with their type arguments, none for a closure, which shares the unit's).
#[derive(Default)]
struct Unit {
    convs: Vec<(TyId, Span)>,
    sites: Vec<(DefId, Vec<TyId>, Span)>,
}

pub(crate) fn check_any_ifaces(cx: &mut Ctx) {
    if cx.generic_overrides.is_empty() {
        return;
    }
    let units = collect(cx);
    let mut reported: FxHashSet<Span> = FxHashSet::default();
    let mut checked: FxHashSet<(TyId, Span)> = FxHashSet::default();
    // The targets with type parameters each definition's instances convert to.
    let mut needs: FxHashMap<DefId, Vec<TyId>> = FxHashMap::default();
    for (d, u) in &units {
        for (t, span) in &u.convs {
            match cx.has_type_params(*t) {
                true => needs.entry(*d).or_default().push(*t),
                false => report(cx, *t, *span, &mut reported, &mut checked),
            }
        }
    }
    loop {
        let mut grew = false;
        for (me, u) in &units {
            for (g, targs, span) in &u.sites {
                let Some(ts) = needs.get(g).cloned() else {
                    continue;
                };
                for t in ts {
                    let s = match targs.is_empty() {
                        true => t,
                        false => cx.subst(t, targs),
                    };
                    if !cx.has_type_params(s) {
                        report(cx, s, *span, &mut reported, &mut checked);
                        continue;
                    }
                    let n = needs.entry(*me).or_default();
                    if !n.contains(&s) {
                        n.push(s);
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
}

/// The units of the program: each function, each module constant's initializer and each type's
/// field initializers (keyed by the type, which `new` and literals instantiate).
fn collect(cx: &mut Ctx) -> Vec<(DefId, Unit)> {
    let ctors: FxHashMap<DefId, DefId> = (0..cx.info.len())
        .filter_map(|i| {
            let d = DefId(i as u32);
            let ctor = cx.adt(d)?.ctor?;
            Some((d, ctor))
        })
        .collect();
    let scan = |u: &mut Unit, e: &mut Expr| match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::AnyTo),
            ..
        } => u.convs.push((e.ty, e.span)),
        E::Closure(c) => u.sites.push((*c, vec![], e.span)),
        E::New {
            def: g,
            type_args: targs,
            ..
        }
        | E::AdtLit {
            def: g,
            type_args: targs,
            ..
        } if !targs.is_empty() => {
            u.sites.push((*g, targs.clone(), e.span));
            if let Some(c) = ctors.get(g) {
                u.sites.push((*c, targs.clone(), e.span));
            }
        }
        E::Call {
            callee: Callee::Def(g, targs),
            ..
        }
        | E::FnRef(g, targs)
            if !targs.is_empty() =>
        {
            u.sites.push((*g, targs.clone(), e.span))
        }
        _ => {}
    };
    let mut out = vec![];
    let mut seen = FxHashSet::default();
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let mut u = Unit::default();
        match d {
            Some(Def::Fn(f)) => visit::exprs_mut(&mut f.body.block, &mut |e| scan(&mut u, e)),
            Some(Def::Global(g)) => visit::exprs_in(&mut g.init, &mut |e| scan(&mut u, e)),
            _ => continue,
        }
        seen.insert(i);
        out.push((DefId(i as u32), u));
    }
    for (i, info) in cx.info.iter_mut().enumerate() {
        // A module constant (or a static field) whose initializer is not moved to `defs` yet.
        if let DefInfo::Global(g) = info {
            if let (Some(init), false) = (g.init.as_mut(), seen.contains(&i)) {
                let mut u = Unit::default();
                visit::exprs_in(init, &mut |e| scan(&mut u, e));
                out.push((DefId(i as u32), u));
            }
            continue;
        }
        let DefInfo::Adt(a) = info else { continue };
        let mut u = Unit::default();
        let start = a.own_fields_start;
        let own = a.fields[start..]
            .iter_mut()
            .filter_map(|x| x.default.as_mut());
        let redefined = a.redefined.iter_mut().filter_map(|r| r.init.as_mut());
        for e in own.chain(redefined) {
            visit::exprs_in(e, &mut |e| scan(&mut u, e));
        }
        out.push((DefId(i as u32), u));
    }
    out
}

/// Reports the conversion to the concrete type `ty` at `span` if it reaches an interface whose
/// generic method is overridden below an implementing class (once per place).
fn report(
    cx: &mut Ctx,
    ty: TyId,
    span: Span,
    reported: &mut FxHashSet<Span>,
    checked: &mut FxHashSet<(TyId, Span)>,
) {
    if !checked.insert((ty, span)) {
        return;
    }
    let mut ifaces = vec![];
    cx.ty.visit(ty, &mut |k| {
        if let TyKind::Dyn(i, _) = k {
            ifaces.push(*i);
        }
    });
    if let Some(d) = ifaces.into_iter().find_map(|i| overridden(cx, i, span)) {
        if reported.insert(span) {
            cx.error(d);
        }
    }
}

/// The error for a conversion of a dynamic value to `iface`, if one of its generic methods
/// called through interface values is overridden below an implementing class.
fn overridden(cx: &Ctx, iface: DefId, span: Span) -> Option<Diagnostic> {
    let i = cx.iface(iface)?;
    for m in i.methods.iter().filter(|m| !m.generics.names.is_empty()) {
        if !cx.generic_called.reaches(iface, &m.name) {
            continue;
        }
        for imp in cx.impls.iter().filter(|x| x.iface == iface) {
            let Some((c, _)) = cx.class_of(imp.ty) else {
                continue;
            };
            let below = (cx.generic_overrides.iter())
                .find(|(s, key)| *key == m.name && *s != c && cx.class_extends(*s, c));
            let Some((sub, _)) = below else { continue };
            let name = |d: DefId| cx.adt(d).map(|a| a.name.clone()).unwrap_or_default();
            let (cn, sn, iname, key) = (name(c), name(*sub), i.name.clone(), &m.name);
            return Some(
                Diagnostic::error(
                    format!("generic method `{key}` is overridden in `{sn}`, so a dynamic value converted to `{iname}` would need dynamic dispatch"),
                    span,
                )
                .with_note(format!("the conversion runs the methods of the type the value had when it became `any`, `unknown` or `object`: a `{cn}` holding a `{sn}` would run `{cn}`'s `{key}`"))
                .with_note(format!("generic methods are dispatched statically: convert the class instance to `{iname}` before it becomes dynamic, or make the method non-generic")),
            );
        }
    }
    None
}
