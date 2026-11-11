//! The fixed point of [`super::check_generic_bodies`] and [`super::check_struct_given`]: what
//! each function needs of its type arguments (the types it stores in struct fields, or the type
//! parameters a struct can't be given for), in terms of its own parameters, carried to every
//! function that instantiates it, until nothing changes ([`Rounds`]).
//!
//! A function reads each of its sources (a function it calls, a closure it makes, a method that
//! runs for a type it mentions or converts to a dynamic type, a method a param call of it
//! resolves to) through an [`Edge`]. A source's lists only grow, so a revisit reads only the
//! sources that grew since the last one, from where it stopped: a function that reaches many
//! methods growing one per round (an interface call on a generic method with many
//! implementations) isn't read in full each round.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::Span;

use super::{carried, extended_impls, field_types};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{instantiate, Dispatch, Rounds, Target};
use crate::glue_reach::{members_by_name, Reach};
use crate::hir::{DefId, TyId, TyKind};
use crate::inst_sites::{carry_guard, ParamCall, Uses};

/// How a generic function was reached at the span an error is reported at.
#[derive(Clone, Copy)]
pub(super) enum Site {
    Call,
    /// A method that runs for a type mentioned there (`Uses::dyn_calls`, [`crate::glue_reach`]);
    /// `true` if it is one the type an `extend` is on runs, and the function is a method of
    /// that `extend`.
    Dispatched(bool),
}

/// A type a function stores in a struct field, and its guard bits (`inst_sites::carry_guard`).
pub(super) type Need = (TyId, u64);

/// A type reaching a function, with where to report it and how it got there.
type Reached = (Need, Span, Site);

/// What is known so far: the types each function stores in struct fields, in terms of its own
/// parameters; the param calls whose receiver is generic in each function; the tuples found.
#[derive(Default)]
pub(super) struct Found {
    pub(super) needs: FxHashMap<DefId, Vec<Need>>,
    /// Those of `needs` a function's own body brings, not a method the type the `extend` it
    /// belongs to is on runs.
    pub(super) in_body: FxHashSet<(DefId, TyId)>,
    pub(super) pneeds: FxHashMap<DefId, Vec<ParamCall>>,
    /// The type parameters (or types computed from them) each function's conversions to a
    /// dynamic type hold, when the program reads members of one ([`members_by_name`]): what a
    /// caller gives for them reaches its classes' methods by name.
    pub(super) anys: FxHashMap<DefId, Vec<TyId>>,
    /// The interfaces whose values become dynamic ones (`Held::ifaces`).
    pub(super) dyn_ifaces: FxHashSet<DefId>,
    pub(super) bad: Vec<(Span, TyId, Site)>,
    /// Where a limit stopped the check.
    pub(super) limits: Vec<Span>,
    pub(super) work: u64,
}

/// How a source's requirements reach a function.
#[derive(Clone, Copy)]
enum How {
    /// A call or a construction with the edge's arguments, under these guard bits.
    Call(u64),
    /// A closure the function makes, which shares its type parameters, under the guard bits of
    /// the branch it is made in.
    Closure(u64),
    /// A method that runs for a type, with that type's arguments (see [`Site::Dispatched`]).
    Dispatched(bool),
    /// The method a param call resolved to, reached as the param call was.
    Param(Site),
}

/// A source of a function, reached with these type arguments at this span.
#[derive(Clone)]
struct Edge {
    args: Vec<TyId>,
    span: Span,
    how: How,
}

/// The lengths of a source's needs, param calls and dynamic types.
type Lens = [usize; 3];

/// One function's state across its visits.
#[derive(Default)]
struct Unit {
    visited: bool,
    edges: FxHashMap<DefId, Vec<Edge>>,
    /// How much of each source its edges have read.
    read: FxHashMap<DefId, Lens>,
    /// The sources, and interfaces whose values became dynamic ones, changed since the last
    /// visit.
    dirty: FxHashSet<DefId>,
    resolved: FxHashSet<ParamCall>,
    dynamic: FxHashSet<TyId>,
}

/// Upper bound on the param calls one function resolves over the whole fixed point (a method
/// whose param calls grow their own receiver would otherwise never end); past it, it refuses
/// ([`super::LIMIT`]).
const RESOLVE_LIMIT: usize = 10_000;

/// What reaches a function in one visit.
#[derive(Default)]
struct Visit {
    types: Vec<Reached>,
    pending: Vec<(ParamCall, Span, Site)>,
    anys: Vec<(TyId, Span)>,
}

/// The state of [`carry`] besides [`Found`].
struct State {
    units: Vec<Unit>,
    /// The def of each unit.
    defs: Vec<DefId>,
    /// See [`on_target`].
    on_target: FxHashMap<DefId, FxHashSet<Target>>,
    /// See [`members_by_name`].
    by_name: bool,
    /// The functions that read each source (or each interface whose conversions they make).
    readers: FxHashMap<DefId, Vec<usize>>,
    rounds: Rounds,
}

impl State {
    /// `d` (a source, or an interface) changed: its readers read it on their next visit.
    fn changed(&mut self, d: DefId) {
        for &r in self.readers.get(&d).into_iter().flatten() {
            self.units[r].dirty.insert(d);
        }
        self.rounds.changed(d);
    }

    fn reads(&mut self, i: usize, d: DefId) {
        let rs = self.readers.entry(d).or_default();
        if !rs.contains(&i) {
            rs.push(i);
        }
        self.rounds.read(i, d);
    }

    /// Function `i` reads `g` through `e`: what the other edges of `g` read so far, now (the rest
    /// is read with theirs, `g` being dirty); all of it if `g` is new.
    fn add_edge(
        &mut self,
        cx: &mut Ctx,
        s: &mut Found,
        i: usize,
        g: DefId,
        e: Edge,
        v: &mut Visit,
    ) {
        let upto = match self.units[i].read.get(&g) {
            Some(r) => *r,
            None => {
                let l = lens(s, g);
                self.units[i].read.insert(g, l);
                self.reads(i, g);
                l
            }
        };
        s.work += read(cx, s, g, &e, [0; 3], upto, v);
        self.units[i].edges.entry(g).or_default().push(e);
    }
}

fn lens(s: &Found, g: DefId) -> Lens {
    [
        s.needs.get(&g).map_or(0, |n| n.len()),
        s.pneeds.get(&g).map_or(0, |n| n.len()),
        s.anys.get(&g).map_or(0, |n| n.len()),
    ]
}

/// What `e` brings of source `g`'s lists from `from` to `to`; returns the items read.
fn read(cx: &mut Ctx, s: &Found, g: DefId, e: &Edge, from: Lens, to: Lens, v: &mut Visit) -> u64 {
    let needs = s.needs.get(&g).map_or(&[][..], |n| &n[from[0]..to[0]]);
    for (t, need) in needs {
        let r = match e.how {
            How::Call(site) => carry_guard(cx, *need, &e.args, site)
                .map(|guard| ((cx.subst(*t, &e.args), guard), Site::Call)),
            How::Closure(site) => Some(((*t, need | site), Site::Call)),
            How::Dispatched(c) => carry_guard(cx, *need, &e.args, 0).and_then(|guard| {
                instantiate(cx, *t, &e.args).map(|t| ((t, guard), Site::Dispatched(c)))
            }),
            How::Param(site) => Some(((cx.subst(*t, &e.args), 0), site)),
        };
        if let Some((n, site)) = r {
            v.types.push((n, e.span, site));
        }
    }
    let pneeds = s.pneeds.get(&g).map_or(&[][..], |n| &n[from[1]..to[1]]);
    for p in pneeds {
        let r = match e.how {
            How::Call(_) => Some((p.subst(cx, &e.args), Site::Call)),
            How::Closure(_) => Some((p.clone(), Site::Call)),
            How::Dispatched(c) => p.instantiate(cx, &e.args).map(|p| (p, Site::Dispatched(c))),
            How::Param(site) => Some((p.subst(cx, &e.args), site)),
        };
        v.pending.extend(r.map(|(p, site)| (p, e.span, site)));
    }
    let anys = s.anys.get(&g).map_or(&[][..], |n| &n[from[2]..to[2]]);
    for t in anys {
        let t = match e.how {
            How::Call(_) | How::Param(_) => Some(cx.subst(*t, &e.args)),
            How::Closure(_) => Some(*t),
            How::Dispatched(_) => instantiate(cx, *t, &e.args),
        };
        v.anys.extend(t.map(|t| (t, e.span)));
    }
    (to.iter().zip(from).map(|(t, f)| t - f).sum::<usize>()) as u64
}

/// The methods the type each method of an `extend` that implements an interface is on runs
/// for it (`this: Cell<T>` in `extend<T> Cell<T> implements I`): what they store is followed
/// wherever that type is, so it isn't the method's own (see `unprovable`). Another instance
/// of the class (`Cell<T>` in `extend<T> Cell<T[]>`) is the method's own.
fn on_target(
    cx: &mut Ctx,
    reach: &mut Reach,
    s: &mut Found,
) -> FxHashMap<DefId, FxHashSet<Target>> {
    let mut dispatch = Dispatch::default();
    let mut out: FxHashMap<DefId, FxHashSet<Target>> = FxHashMap::default();
    for index in extended_impls(cx) {
        let (ty, methods) = {
            let imp = &cx.impls[index as usize];
            (imp.ty, imp.methods.clone())
        };
        let mut runs: FxHashSet<Target> = dispatch.targets(cx, ty).into_iter().collect();
        runs.extend(reach.hooks(cx, ty));
        for m in methods {
            if matches!(&cx.info[m.0 as usize], DefInfo::Fn(f) if f.owner.is_none()) {
                out.entry(m).or_default().extend(runs.iter().cloned());
            }
        }
    }
    s.work += dispatch.work;
    out
}

/// The conversions in `u` to a dynamic type, or to an interface whose values become dynamic
/// ones (`only`: to that interface alone): the type converted reaches its classes' methods by
/// name.
fn converted(cx: &Ctx, u: &Uses, s: &Found, only: Option<DefId>, v: &mut Visit) {
    for (from, to, span) in &u.converts {
        let into = match (cx.ty.kind(*to), only) {
            (TyKind::Dyn(j, _), Some(only)) => *j == only,
            (TyKind::Dyn(j, _), None) => cx.dynamic(*to).is_some() || s.dyn_ifaces.contains(j),
            _ => false,
        };
        if into {
            v.anys.push((*from, *span));
        }
    }
}

/// The first visit of function `i`: its own sites and every source.
fn first_visit(
    cx: &mut Ctx,
    st: &mut State,
    reach: &mut Reach,
    s: &mut Found,
    i: usize,
    u: &Uses,
) -> Visit {
    let mut v = Visit::default();
    st.units[i].visited = true;
    s.work += u.size() + u.converts.len() as u64;
    v.pending = (u.param_calls.iter())
        .map(|(p, s)| (p.clone(), *s, Site::Call))
        .collect();
    for ((g, targs, span), site) in u.calls.iter().zip(&u.guards) {
        let e = Edge {
            args: targs.clone(),
            span: *span,
            how: How::Call(*site),
        };
        st.add_edge(cx, s, i, *g, e, &mut v);
    }
    for (c, site) in u.closures.iter().zip(&u.closure_guards) {
        let span = cx.def_spans[c.0 as usize];
        let e = Edge {
            args: vec![],
            span,
            how: How::Closure(*site),
        };
        st.add_edge(cx, s, i, *c, e, &mut v);
    }
    let mut dispatched = u.dyn_calls.clone();
    for (t, span) in &u.types {
        let hooks = reach.hooks(cx, *t);
        dispatched.extend(hooks.into_iter().map(|(m, a)| (m, a, *span)));
    }
    let f = st.defs[i];
    for (m, args, span) in dispatched {
        let target = (st.on_target.get(&f)).is_some_and(|t| t.contains(&(m, args.clone())));
        let how = How::Dispatched(target);
        st.add_edge(cx, s, i, m, Edge { args, span, how }, &mut v);
    }
    if st.by_name {
        converted(cx, u, s, None, &mut v);
        for (_, to, _) in &u.converts {
            if let TyKind::Dyn(j, _) = *cx.ty.kind(*to) {
                st.reads(i, j);
            }
        }
    }
    v
}

/// A later visit of function `i`: what its dirty sources added since it last read them.
fn revisit(cx: &mut Ctx, st: &mut State, s: &mut Found, i: usize, u: &Uses) -> Visit {
    let mut v = Visit::default();
    let mut dirty: Vec<DefId> = std::mem::take(&mut st.units[i].dirty).into_iter().collect();
    dirty.sort_unstable();
    for g in dirty {
        if s.dyn_ifaces.contains(&g) && cx.iface(g).is_some() {
            s.work += u.converts.len() as u64;
            converted(cx, u, s, Some(g), &mut v);
        }
        let Some(from) = st.units[i].read.get(&g).copied() else {
            continue;
        };
        let to = lens(s, g);
        let unit = &st.units[i];
        for e in unit.edges.get(&g).into_iter().flatten() {
            s.work += read(cx, s, g, e, from, to, &mut v);
        }
        st.units[i].read.insert(g, to);
    }
    v
}

/// Carries what `s` holds through `uses` until nothing changes (`refused`: methods not to read
/// through a param call, already reported; `bad`: is this type, given for what's carried, an
/// error).
pub(super) fn carry(
    cx: &mut Ctx,
    uses: &[(DefId, Uses)],
    refused: &FxHashSet<DefId>,
    s: &mut Found,
    bad: fn(&mut Ctx, TyId) -> bool,
) {
    let mut reach = Reach::default();
    let by_name = members_by_name(cx);
    let on_target = on_target(cx, &mut reach, s);
    let mut st = State {
        units: (0..uses.len()).map(|_| Unit::default()).collect(),
        defs: uses.iter().map(|(d, _)| *d).collect(),
        on_target,
        by_name,
        readers: FxHashMap::default(),
        rounds: Rounds::new(uses.len(), []),
    };
    while let Some(i) = st.rounds.pop() {
        let (f, u) = &uses[i];
        let mut v = match st.units[i].visited {
            false => first_visit(cx, &mut st, &mut reach, s, i, u),
            true => revisit(cx, &mut st, s, i, u),
        };
        let mut grew = false;
        loop {
            while let Some((t, span)) = v.anys.pop() {
                if !by_name || !st.units[i].dynamic.insert(t) {
                    continue;
                }
                let held = reach.dynamic(cx, t);
                for j in held.ifaces {
                    if s.dyn_ifaces.insert(j) {
                        st.changed(j);
                    }
                }
                // The parameters held, not the type: its other parts are followed here, in
                // terms of them.
                for p in held.params {
                    if !carried(cx, p) {
                        s.limits.push(span);
                        continue;
                    }
                    let n = s.anys.entry(*f).or_default();
                    if !n.contains(&p) {
                        n.push(p);
                        grew = true;
                    }
                }
                // What runs by name runs at whatever reached the conversion.
                for (m, args) in held.methods {
                    let how = How::Dispatched(false);
                    st.add_edge(cx, s, i, m, Edge { args, span, how }, &mut v);
                }
            }
            let pending = std::mem::take(&mut v.pending);
            if pending.is_empty() {
                break;
            }
            for (p, span, site) in pending {
                if p.generic(cx) {
                    let parts = std::iter::once(&p.recv).chain(&p.margs);
                    if parts.into_iter().any(|t| !carried(cx, *t)) {
                        continue;
                    }
                    let n = s.pneeds.entry(*f).or_default();
                    if !n.contains(&p) {
                        n.push(p);
                        grew = true;
                    }
                    continue;
                }
                let unit = &mut st.units[i];
                if unit.resolved.contains(&p) {
                    continue;
                }
                if unit.resolved.len() >= RESOLVE_LIMIT {
                    s.limits.push(span);
                    continue;
                }
                unit.resolved.insert(p.clone());
                let targets = p.targets(cx);
                s.work += 1 + targets.len() as u64;
                for (m, args) in targets {
                    if refused.contains(&m) {
                        continue;
                    }
                    let how = How::Param(site);
                    st.add_edge(cx, s, i, m, Edge { args, span, how }, &mut v);
                }
            }
        }
        for ((t, guard), span, site) in v.types {
            // What the type an `extend` is on runs isn't the `extend`'s own.
            let own = !matches!(site, Site::Dispatched(true));
            if bad(cx, t) {
                if guard == 0 {
                    s.bad.push((span, t, site));
                    continue;
                }
                // Under a primitive guard: the branch is dropped where the guarded parameter
                // is given an object, which only the callers can tell.
                if own {
                    s.in_body.insert((*f, t));
                }
                let n = s.needs.entry(*f).or_default();
                if !n.contains(&(t, guard)) {
                    n.push((t, guard));
                    grew = true;
                }
                continue;
            }
            let mut parts = vec![];
            field_types(cx, t, &mut parts, 0);
            let n = s.needs.entry(*f).or_default();
            for part in parts {
                if own {
                    s.in_body.insert((*f, part));
                }
                if !carried(cx, part) {
                    s.limits.push(span);
                } else if !n.contains(&(part, guard)) {
                    n.push((part, guard));
                    grew = true;
                }
            }
        }
        if grew {
            st.changed(*f);
        }
    }
    s.work += reach.work;
}
