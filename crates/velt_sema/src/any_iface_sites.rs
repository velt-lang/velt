//! Conversions of an `any`, `unknown` or `object` to an interface type (`Intrinsic::AnyTo`) whose
//! generic method the program calls through interface values, when a subclass of a class
//! implementing the interface overrides that method (#943): the conversion uses the table of the
//! type the value was given to the `any` as, so a `Base` holding a `Sub` would run `Base`'s
//! method. Reported at the conversion, as that of a class value is
//! (`body::expr::iface_call` `check_generic_override_dyn`).
//!
//! Conversions are found in function bodies, the initializers of module constants and static
//! fields, and those of fields. One whose target is still a type parameter's (`u as T` in
//! `conv<T>`) is carried, like `generic_override_sites::check` carries calls through type
//! parameters, to every site that instantiates its definition (substituted, through the generic
//! definitions in between, with a worklist), and reported at a site where the target has become
//! such an interface. The sites are calls, function values, `new` and literals (also of the
//! constructor), closures, a class's `extends` clause (its base's initializers and constructor),
//! and a generic interface method called through an interface value or a bound (the method of
//! each implementing class).
//!
//! The same carrying gives the interfaces an object literal held by an `any` may be converted to
//! at run time ([`instantiated_conversions`], `object_ifaces.rs`, #1909).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::visit;

/// A generic interface method called through an interface value or a bound, in unit `.0`: the
/// interface, the slot, the interface arguments, the method's own type arguments, where.
type IfaceCall = (usize, DefId, u32, Vec<TyId>, Vec<TyId>, Span);

/// A unit of code and what it holds: its conversions (target type, where), the definitions it
/// instantiates (with their type arguments, none for a closure, which shares the unit's), and
/// its calls of generic interface methods through a bound (the receiver's type, the interface,
/// the slot, where).
#[derive(Default)]
struct Unit {
    convs: Vec<(TyId, Span)>,
    /// The types of the values converted to `any`: a function in one (also in its fields,
    /// elements and options) converts the arguments of a call through `any` to its parameter
    /// types ([`instantiated_conversions`]).
    held: Vec<TyId>,
    sites: Vec<(DefId, Vec<TyId>, Span)>,
    bounds: Vec<(TyId, DefId, u32, Span)>,
}

/// The sites instantiating each definition: the unit they are in, the type arguments, where.
type Callers = FxHashMap<DefId, Vec<(DefId, Vec<TyId>, Span)>>;

/// The checks of generic methods reached through interface tables that need every
/// instantiation: which interfaces a call through a bound reaches (module docs), the adapters
/// (`generic_override_sites::check_adapters`), and the conversions of dynamic values.
pub(crate) fn check(cx: &mut Ctx) {
    let bound_calls = !cx.generic_called.on_params.is_empty();
    let overrides = !cx.generic_overrides.is_empty();
    if !bound_calls && !overrides {
        crate::generic_override_sites::check_adapters(cx);
        return;
    }
    let units = collect(cx);
    let callers = callers_of(&units);
    // A generic method called through a bound runs the method of the type the bound's parameter
    // is given; it goes through the interface's table only where that is an interface value.
    let mut names: Vec<String> = vec![];
    let mut calls = vec![];
    for (d, u) in &units {
        for (recv, iface, slot, span) in &u.bounds {
            let m = cx.iface(*iface).and_then(|i| i.methods.get(*slot as usize));
            if let Some(m) = m.filter(|m| !m.generics.names.is_empty()) {
                names.push(m.name.clone());
                calls.push((*d, *recv, names.len() - 1, *span));
            }
        }
    }
    carry(cx, &callers, calls, &mut |cx, t, k, _| {
        if let TyKind::Dyn(i, _) = cx.ty.kind(t) {
            let call = (*i, names[k].clone());
            cx.generic_called.on.insert(call);
        }
    });
    crate::generic_override_sites::check_adapters(cx);
    if !overrides {
        return;
    }
    let mut reported: FxHashSet<Span> = FxHashSet::default();
    let mut checked: FxHashSet<(TyId, Span)> = FxHashSet::default();
    let convs = (units.iter())
        .flat_map(|(d, u)| u.convs.iter().map(move |(t, span)| (*d, *t, 0, *span)))
        .collect();
    carry(cx, &callers, convs, &mut |cx, t, _, span| {
        report(cx, t, span, &mut reported, &mut checked)
    });
}

/// The targets of the conversions of dynamic values (`Intrinsic::AnyTo`) to interfaces written
/// with type parameters that an object converted to `any` may fill, as each site instantiating
/// their definitions makes them (substituted, through the generic definitions in between): the
/// interfaces such a conversion reaches at run time (`object_ifaces::make_dynamic_object_impls`,
/// #1909).
pub(crate) fn instantiated_conversions(cx: &mut Ctx) -> Vec<TyId> {
    let units = collect(cx);
    let callers = callers_of(&units);
    let mut convs: Vec<(DefId, TyId, usize, Span)> = (units.iter())
        .flat_map(|(d, u)| u.convs.iter().map(move |(t, span)| (*d, *t, 0, *span)))
        .collect();
    for (d, u) in &units {
        for &t in &u.held {
            if !cx.has_type_params(t) {
                continue;
            }
            let params = fn_params_in(cx, t);
            convs.extend(params.into_iter().map(|p| (*d, p, 0, Span::DUMMY)));
        }
    }
    convs.retain(|(_, t, ..)| cx.may_fill_generic(*t));
    let mut out = vec![];
    carry(cx, &callers, convs, &mut |_, t, _, _| out.push(t));
    out.sort_unstable_by_key(|t| t.0);
    out.dedup();
    out
}

/// The parameter types of the functions in a value of type `t`: itself, its options, elements
/// and map values, and the fields of an object type (with its arguments).
fn fn_params_in(cx: &mut Ctx, t: TyId) -> Vec<TyId> {
    let mut out = vec![];
    let mut seen: FxHashSet<TyId> = FxHashSet::default();
    let mut todo = vec![t];
    while let Some(t) = todo.pop() {
        if !seen.insert(t) || seen.len() > 4096 {
            continue;
        }
        match cx.ty.kind(t).clone() {
            TyKind::FnPtr { params, .. } => out.extend(params),
            TyKind::Array(e) | TyKind::Option(e, _) => todo.push(e),
            TyKind::Map(_, v) => todo.push(v),
            TyKind::Adt(d, args) if cx.is_object_type(t) => {
                let fields: Vec<TyId> = (cx.adt(d).into_iter())
                    .flat_map(|a| a.fields.iter().map(|f| f.ty))
                    .collect();
                for f in fields {
                    todo.push(match args.is_empty() {
                        true => f,
                        false => cx.subst(f, &args),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The sites instantiating each definition, in `units`.
fn callers_of(units: &[(DefId, Unit)]) -> Callers {
    let mut callers: Callers = FxHashMap::default();
    for (d, u) in units {
        for (g, targs, span) in &u.sites {
            callers
                .entry(*g)
                .or_default()
                .push((*d, targs.clone(), *span));
        }
    }
    callers
}

/// Carries each of `start` (a unit, a type in its terms, a tag, where) to the sites instantiating
/// the unit, substituted, through the generic definitions in between, with a worklist; `done`
/// gets each one once it has no type parameters, with the tag and the site.
fn carry(
    cx: &mut Ctx,
    callers: &Callers,
    start: Vec<(DefId, TyId, usize, Span)>,
    done: &mut dyn FnMut(&mut Ctx, TyId, usize, Span),
) {
    let mut needs: FxHashMap<DefId, FxHashSet<(TyId, usize)>> = FxHashMap::default();
    // Each target carried: its definition, its size, and the one it was carried from.
    let mut path: Vec<(DefId, u32, Option<usize>)> = vec![];
    let mut work: Vec<(DefId, TyId, usize, usize)> = vec![];
    for (d, t, k, span) in start {
        match cx.has_type_params(t) {
            true if needs.entry(d).or_default().insert((t, k)) => {
                path.push((d, size(cx, t), None));
                work.push((d, t, k, path.len() - 1));
            }
            true => {}
            false => done(cx, t, k, span),
        }
    }
    while let Some((g, t, k, at)) = work.pop() {
        let Some(sites) = callers.get(&g) else {
            continue;
        };
        for (me, targs, span) in sites {
            let s = match targs.is_empty() {
                true => t,
                false => cx.subst(t, targs),
            };
            if !cx.has_type_params(s) {
                done(cx, s, k, *span);
                continue;
            }
            // A target that comes back to a definition it was carried through, larger
            // (`Box<W<T>>` from `Box<T>` in `f<T>` calling `f<W<T>>`), grows at each
            // instantiation: the instantiation cycle `instantiation_cycles` reports. Carrying it
            // on would multiply the work at each round (with several such calls), up to
            // `MAX_DEPTH`.
            let n = size(cx, s);
            if depth(cx, s, 0) > MAX_DEPTH || grows(&path, at, *me, n) {
                continue;
            }
            if needs.entry(*me).or_default().insert((s, k)) {
                path.push((*me, n, Some(at)));
                work.push((*me, s, k, path.len() - 1));
            }
        }
    }
}

/// Was a smaller target than one of size `n` carried through definition `me` on the way to
/// `path[at]` ([`carry`])?
fn grows(path: &[(DefId, u32, Option<usize>)], at: usize, me: DefId, n: u32) -> bool {
    let mut cur = Some(at);
    while let Some(i) = cur {
        let (d, m, up) = path[i];
        if d == me && m < n {
            return true;
        }
        cur = up;
    }
    false
}

/// The number of types `t` is made of (bounded by `MAX_SIZE`).
fn size(cx: &Ctx, t: TyId) -> u32 {
    let mut n = 0;
    let mut todo = vec![t];
    while let Some(t) = todo.pop() {
        n += 1;
        if n >= MAX_SIZE {
            break;
        }
        todo.extend(crate::types::children(cx.ty.kind(t)));
    }
    n
}

/// The size [`size`] counts up to.
const MAX_SIZE: u32 = 4096;

/// How deeply a carried target may nest types ([`check_any_ifaces`]).
const MAX_DEPTH: u32 = 12;

/// How deeply `t` nests types (bounded by `MAX_DEPTH + 1`).
fn depth(cx: &Ctx, t: TyId, d: u32) -> u32 {
    if d > MAX_DEPTH {
        return d;
    }
    let kids = crate::types::children(cx.ty.kind(t));
    kids.into_iter()
        .map(|c| depth(cx, c, d + 1))
        .max()
        .unwrap_or(d)
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
    // A generic interface method called through an interface value or a bound may run the
    // method of each class implementing it: the interface, the slot, the interface arguments,
    // the method's own type arguments.
    let mut iface_calls: Vec<IfaceCall> = vec![];
    // Each instance of a generic class or struct made (`new C<A>`, a literal), in unit `.0`.
    let mut made: Vec<(usize, DefId, Vec<TyId>, Span)> = vec![];
    // The receivers and slots of the calls through class vtables.
    let mut virtuals: Vec<(TyId, u32)> = vec![];
    let tys = &cx.ty;
    let mut scan = |at: usize, u: &mut Unit, e: &mut Expr| match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::AnyTo),
            ..
        } => u.convs.push((e.ty, e.span)),
        E::ToDyn { expr, .. } => u.held.push(expr.ty),
        E::Call {
            callee:
                Callee::Dyn {
                    slot,
                    method_type_args,
                },
            args,
        } => {
            if let Some(TyKind::Dyn(i, iargs)) = args.first().map(|a| tys.kind(a.ty)) {
                let call = (
                    at,
                    *i,
                    *slot,
                    iargs.clone(),
                    method_type_args.clone(),
                    e.span,
                );
                iface_calls.push(call);
            }
        }
        E::Call {
            callee:
                Callee::ParamMethod {
                    iface,
                    iface_args,
                    slot,
                    method_type_args,
                },
            args,
        } => {
            let call = (
                at,
                *iface,
                *slot,
                iface_args.clone(),
                method_type_args.clone(),
                e.span,
            );
            iface_calls.push(call);
            if let Some(recv) = args.first() {
                u.bounds.push((recv.ty, *iface, *slot, e.span));
            }
        }
        E::Call {
            callee: Callee::Virtual { slot },
            args,
        } => {
            if let Some(recv) = args.first() {
                virtuals.push((recv.ty, *slot));
            }
        }
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
            made.push((at, *g, targs.clone(), e.span));
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
        let at = out.len();
        match d {
            Some(Def::Fn(f)) => visit::exprs_mut(&mut f.body.block, &mut |e| scan(at, &mut u, e)),
            Some(Def::Global(g)) => visit::exprs_in(&mut g.init, &mut |e| scan(at, &mut u, e)),
            _ => continue,
        }
        seen.insert(i);
        out.push((DefId(i as u32), u));
    }
    for (i, info) in cx.info.iter_mut().enumerate() {
        let at = out.len();
        // A module constant (or a static field) whose initializer is not moved to `defs` yet.
        if let DefInfo::Global(g) = info {
            if let (Some(init), false) = (g.init.as_mut(), seen.contains(&i)) {
                let mut u = Unit::default();
                visit::exprs_in(init, &mut |e| scan(at, &mut u, e));
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
            visit::exprs_in(e, &mut |e| scan(at, &mut u, e));
        }
        out.push((DefId(i as u32), u));
    }
    // The names of the methods called through a vtable, an interface table or a bound: what an
    // instance's methods may be called as later.
    let mut dispatched: FxHashSet<String> = FxHashSet::default();
    for (_, iface, slot, ..) in &iface_calls {
        if let Some(m) = cx.iface(*iface).and_then(|i| i.methods.get(*slot as usize)) {
            dispatched.insert(m.name.clone());
        }
    }
    for (ty, slot) in virtuals {
        let Some((c, _)) = cx.class_of(ty) else {
            continue;
        };
        let names = cx.adt(c).map(|a| &a.vslots).into_iter().flatten();
        dispatched.extend(names.filter(|(_, s)| **s == slot).map(|(n, _)| n.clone()));
    }
    for (at, iface, slot, iargs, margs, span) in iface_calls {
        let sites = iface_sites(cx, iface, slot, &iargs, &margs);
        let u = &mut out[at].1;
        u.sites
            .extend(sites.into_iter().map(|(m, targs)| (m, targs, span)));
    }
    // An instance made is what its methods run on, whichever way they are called (directly,
    // through a vtable or an interface table): each method of the class and of its bases is
    // instantiated with the instance's arguments, mapped to the method's class.
    for (at, c, targs, span) in made {
        let sites = class_method_sites(cx, c, &targs, &dispatched);
        let u = &mut out[at].1;
        u.sites
            .extend(sites.into_iter().map(|(m, targs)| (m, targs, span)));
    }
    // A class extending a generic class instantiates its initializers, constructor and methods
    // (and what they call) with the `extends` clause's arguments.
    for (d, u) in out.iter_mut() {
        let d = *d;
        let Some(TyKind::Adt(h, hargs)) = cx.adt(d).and_then(|a| a.base).map(|b| cx.ty.kind(b))
        else {
            continue;
        };
        let (h, hargs) = (*h, hargs.clone());
        if hargs.is_empty() {
            continue;
        }
        let span = cx.adt(d).map_or(Span::DUMMY, |a| a.span);
        let mut sites = vec![(h, hargs.clone())];
        if let Some(c) = ctors.get(&h) {
            sites.push((*c, hargs.clone()));
        }
        sites.extend(class_method_sites(cx, h, &hargs, &dispatched));
        u.sites
            .extend(sites.into_iter().map(|(m, targs)| (m, targs, span)));
    }
    out
}

/// The arguments of class `owner` that the class type `ty` is or extends: `ty`'s arguments mapped
/// through the `extends` clauses up to it.
fn args_at(cx: &mut Ctx, ty: TyId, owner: DefId) -> Option<Vec<TyId>> {
    let mut cur = ty;
    for _ in 0..64 {
        let TyKind::Adt(d, args) = cx.ty.kind(cur).clone() else {
            return None;
        };
        if d == owner {
            return Some(args);
        }
        let base = cx.adt(d)?.base?;
        cur = cx.subst(base, &args);
    }
    None
}

/// The type arguments method `m` runs with on an instance of class type `ty`: those of its class
/// (`args_at`), then `own`, or `error` for its own type parameters without `own`.
fn method_args(cx: &mut Ctx, ty: TyId, m: DefId, own: Option<&[TyId]>) -> Option<Vec<TyId>> {
    let f = cx.try_fn(m)?;
    let (owner, n) = (f.owner?, f.generics.len());
    let mut targs = args_at(cx, ty, owner)?;
    match own {
        Some(own) => targs.extend(own.iter().copied()),
        None => targs.resize(n.max(targs.len()), cx.ty.error),
    }
    Some(targs)
}

/// The methods of class `c` and of its bases named in `dispatched` (those called through a
/// table, which direct calls don't instantiate), instantiated for an instance of `c<args>`.
fn class_method_sites(
    cx: &mut Ctx,
    c: DefId,
    args: &[TyId],
    dispatched: &FxHashSet<String>,
) -> Vec<(DefId, Vec<TyId>)> {
    let ty = cx.ty.intern(TyKind::Adt(c, args.to_vec()));
    let mut methods = vec![];
    let mut cur = Some(c);
    for _ in 0..64 {
        let Some(d) = cur else { break };
        let Some(a) = cx.adt(d) else { break };
        let mut own: Vec<DefId> = (a.methods.iter())
            .filter(|(name, m)| !m.is_static && dispatched.contains(*name))
            .map(|(_, m)| m.def)
            .collect();
        own.sort_unstable_by_key(|m| m.0);
        methods.extend(own);
        cur = match a.base.map(|b| cx.ty.kind(b)) {
            Some(TyKind::Adt(b, _)) => Some(*b),
            _ => None,
        };
    }
    methods.dedup();
    let mut out = vec![];
    for m in methods {
        if let Some(targs) = method_args(cx, ty, m, None).filter(|t| !t.is_empty()) {
            out.push((m, targs));
        }
    }
    out
}

/// The methods a call of generic slot `slot` of `iface<iargs>` with own type arguments `margs`
/// may run: that of each class implementing the interface (which may be inherited from a base
/// class), with the type arguments of the method's class the interface's arguments give
/// (`error` for the others), then `margs`.
fn iface_sites(
    cx: &mut Ctx,
    iface: DefId,
    slot: u32,
    iargs: &[TyId],
    margs: &[TyId],
) -> Vec<(DefId, Vec<TyId>)> {
    let impls: Vec<_> = (cx.impls.iter())
        .filter(|i| i.iface == iface)
        .map(|i| {
            (
                i.methods.get(slot as usize).copied(),
                i.generics,
                i.iface_args.clone(),
                i.ty,
            )
        })
        .collect();
    let mut out = vec![];
    for (m, n, pattern, pattern_ty) in impls {
        let Some(m) = m else { continue };
        let mut slots = vec![None; n as usize];
        for (p, a) in pattern.iter().zip(iargs) {
            cx.match_ty(*p, *a, &mut slots);
        }
        let error = cx.ty.error;
        let targs: Vec<TyId> = slots.into_iter().map(|s| s.unwrap_or(error)).collect();
        let ty = cx.subst(pattern_ty, &targs);
        // Not a class's method (a generic closure's): its implementation's arguments.
        let targs = match method_args(cx, ty, m, Some(margs)) {
            Some(t) => t,
            None => [targs.as_slice(), margs].concat(),
        };
        if !targs.is_empty() {
            out.push((m, targs));
        }
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
