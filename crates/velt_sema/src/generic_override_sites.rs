//! A generic method of a bound called through a type parameter (`m.apply(f)`, `m: T`,
//! `T extends Mapper`) is resolved statically per instantiation (`Callee::ParamMethod`): with
//! `T = A`, `A`'s method runs. When a subclass of `A` overrides that generic method, a value of
//! type `A` may hold the subclass, whose method JavaScript would run, and generic methods have no
//! vtable slot (issue #1132). Calling the method on an `A` directly is rejected
//! (`body::expr::iface_call` `check_generic_override`); this pass rejects the same call made
//! through an instantiation: at each site that instantiates a generic definition with a class
//! argument, when the definition (or what it passes the parameter on to) calls a generic method
//! that a subclass of that class overrides.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::visit;

/// Type parameter `.1` of definition `.0` (a function, or a class for its methods).
type Node = (DefId, u32);

/// A generic interface method called through a type parameter: the interface and its slot.
type Slot = (DefId, u32);

pub(crate) fn check(cx: &mut Ctx) {
    if cx.generic_overrides.is_empty() {
        return;
    }
    let (mut uses, edges, sites) = collect(cx);
    // What a parameter is passed on to calls through it too, to a fixed point.
    loop {
        let mut grew = false;
        for (from, to) in &edges {
            let add: Vec<Slot> = uses.get(to).into_iter().flatten().copied().collect();
            let set = uses.entry(*from).or_default();
            for s in add {
                grew |= set.insert(s);
            }
        }
        if !grew {
            break;
        }
    }
    // One error per place (`new C<A>(…).m()` is two sites starting there).
    let mut reported = FxHashSet::default();
    for (g, targs, span, args) in sites {
        if reported.contains(&(span.file, span.lo)) {
            continue;
        }
        for (j, t) in targs.iter().enumerate() {
            let Some((c, _)) = cx.class_of(*t) else {
                continue;
            };
            if args.as_ref().is_some_and(|a| exact(cx, g, j as u32, *t, a)) {
                continue;
            }
            let slots: Vec<Slot> = uses
                .get(&(g, j as u32))
                .into_iter()
                .flatten()
                .copied()
                .collect();
            for (iface, slot) in slots {
                if let Some(d) = overridden(cx, c, iface, slot, *t, g, span) {
                    cx.error(d);
                    reported.insert((span.file, span.lo));
                    break;
                }
            }
        }
    }
}

/// The error for a call of generic slot `slot` of `iface` on a `c` when a subclass of `c`
/// overrides the method, if one does.
fn overridden(
    cx: &Ctx,
    c: DefId,
    iface: DefId,
    slot: u32,
    t: TyId,
    g: DefId,
    span: Span,
) -> Option<Diagnostic> {
    let m = cx.iface(iface)?.methods.get(slot as usize)?;
    if m.generics.len() == 0 {
        return None;
    }
    let sub = cx
        .generic_overrides
        .iter()
        .find(|(s, key)| *key == m.name && *s != c && cx.class_extends(*s, c))
        .map(|(s, _)| *s)?;
    let sub = cx.adt(sub).map(|a| a.name.clone()).unwrap_or_default();
    let gname = match cx.try_fn(g) {
        Some(f) => f.name.clone(),
        None => cx.adt(g).map(|a| a.name.clone()).unwrap_or_default(),
    };
    let gname = gname.rsplit("::").next().unwrap_or(&gname).to_string();
    let cn = cx.display(t);
    Some(
        Diagnostic::error(
            format!(
                "generic method `{}` is overridden in `{sub}`, so calling it on a `{cn}` would need dynamic dispatch",
                m.name
            ),
            span,
        )
        .with_note(format!(
            "`{gname}` calls `{}` through a type parameter given `{cn}` here; generic methods are dispatched statically: instantiate it with the concrete class, or make the method non-generic",
            m.name
        )),
    )
}

/// An instantiation site: the generic definition, its type arguments, where, and for a call
/// each argument's type and whether it is a `new` ([`exact`]).
type Site = (DefId, Vec<TyId>, Span, Option<Vec<(TyId, bool)>>);

type Collected = (
    FxHashMap<Node, FxHashSet<Slot>>,
    Vec<(Node, Node)>,
    Vec<Site>,
);

/// Do the values of type parameter `j` (given class type `t`) that call `g` receives all hold
/// exactly that class (`go(new Sq())`), so that its own method is the one JavaScript runs? Each
/// parameter of `g` whose declared type mentions the parameter must be of the parameter's type
/// itself, and given a `new` of `t`. A value inside another type (`get: () => T`, `ys: T[]`,
/// `o: { m: T }`) or read from a variable may be of a subclass, and so may one given to a class
/// instantiated with `t`, whose methods take more later.
fn exact(cx: &Ctx, g: DefId, j: u32, t: TyId, args: &[(TyId, bool)]) -> bool {
    let Some(f) = cx.try_fn(g) else {
        return false;
    };
    if f.this.is_some() || f.params.len() != args.len() {
        return false;
    }
    let mut given = false;
    for (p, (ty, new)) in f.params.iter().zip(args) {
        if matches!(cx.ty.kind(p.ty), TyKind::Param(i) if *i == j) {
            if !(*new && *ty == t) {
                return false;
            }
            given = true;
            continue;
        }
        let mut mentions = false;
        cx.ty.visit(p.ty, &mut |k| {
            mentions |= matches!(k, TyKind::Param(i) if *i == j)
        });
        if mentions {
            return false;
        }
    }
    given
}

/// The slots each parameter is called through directly, the edges from a parameter to the
/// parameters it is passed on as (a call's type argument that is exactly the parameter, a
/// closure's same parameter, a class's method's parameter), and every instantiation site.
fn collect(cx: &mut Ctx) -> Collected {
    let mut uses: FxHashMap<Node, FxHashSet<Slot>> = FxHashMap::default();
    let mut edges = vec![];
    let mut sites = vec![];
    let owners: Vec<Option<DefId>> = (0..cx.defs.len())
        .map(|i| cx.try_fn(DefId(i as u32)).and_then(|f| f.owner))
        .collect();
    let tys = &cx.ty;
    let param = |t: TyId| match tys.kind(t) {
        TyKind::Param(p) => Some(*p),
        _ => None,
    };
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let me = DefId(i as u32);
        // A method's leading parameters are its class's.
        if let Some(o) = owners[i] {
            for p in 0..f.generics {
                edges.push(((o, p), (me, p)));
            }
        }
        let n = f.generics;
        let generic = n > 0;
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Call {
                callee: Callee::ParamMethod { iface, slot, .. },
                args,
            } if generic => {
                if let Some(p) = args.first().and_then(|a| param(a.ty)) {
                    uses.entry((me, p)).or_default().insert((*iface, *slot));
                }
            }
            E::Closure(c) if generic => {
                for p in 0..n {
                    edges.push(((me, p), (*c, p)));
                }
            }
            E::Call {
                callee: Callee::Def(g, targs),
                args,
            }
            | E::New {
                def: g,
                type_args: targs,
                args,
            } if !targs.is_empty() => {
                for (j, t) in targs.iter().enumerate() {
                    if let Some(p) = param(*t).filter(|_| generic) {
                        edges.push(((me, p), (*g, j as u32)));
                    }
                }
                let given = match &e.kind {
                    E::Call { .. } => Some(
                        args.iter()
                            .map(|a| (a.ty, matches!(a.kind, E::New { .. })))
                            .collect(),
                    ),
                    _ => None,
                };
                sites.push((*g, targs.clone(), e.span, given));
            }
            E::FnRef(g, targs)
            | E::AdtLit {
                def: g,
                type_args: targs,
                ..
            } if !targs.is_empty() => {
                for (j, t) in targs.iter().enumerate() {
                    if let Some(p) = param(*t).filter(|_| generic) {
                        edges.push(((me, p), (*g, j as u32)));
                    }
                }
                sites.push((*g, targs.clone(), e.span, None));
            }
            _ => {}
        });
    }
    (uses, edges, sites)
}
