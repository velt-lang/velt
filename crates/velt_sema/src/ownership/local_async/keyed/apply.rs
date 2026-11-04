//! Applying summaries for [`super`], in time linear in what grows: each application of a summary
//! to a shape of arguments remembers how much of the summary it applied, and a summary that grows
//! has only its new entries applied, at only the events that applied it. A call through a
//! function value read from the heap applies one join per function type, the union of the
//! summaries of the functions that may be in it, kept up to date as they grow. An entry says which
//! parameter is stored into from which, not by which key: the keys go over the graph of
//! [`super::flow`], so a summary stays small however many keys reach it.

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};

use super::super::fed::adt_of;
use super::super::targets::value_targets;
use super::{ensure, Arg, Ev, Run, Site, Src};

/// A shape of arguments ([`Arg`] per operand), interned.
pub(super) type Shape = u32;

/// The shape a closure being made has no captures for: it is being called.
pub(super) const CALLED: Shape = u32::MAX;

/// The shapes of arguments seen, each once.
#[derive(Default)]
pub(super) struct Shapes {
    ids: FxHashMap<Rc<[Vec<Arg>]>, Shape>,
    list: Vec<Rc<[Vec<Arg>]>>,
}

impl Shapes {
    pub(super) fn id(&mut self, args: &[Vec<Arg>]) -> Shape {
        if let Some(&id) = self.ids.get(args) {
            return id;
        }
        let rc: Rc<[Vec<Arg>]> = args.into();
        let id = self.list.len() as Shape;
        self.list.push(rc.clone());
        self.ids.insert(rc, id);
        id
    }

    fn get(&self, id: Shape) -> Option<Rc<[Vec<Arg>]>> {
        self.list.get(id as usize).cloned()
    }
}

/// What has a summary: a function, or the join of the function values of a function type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Node {
    Fn(DefId),
    Join(TyId),
}

/// A set that keeps its order of insertion, so that what was added since a point is a suffix.
pub(super) struct Ordered<T> {
    list: Vec<T>,
    set: FxHashSet<T>,
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Ordered {
            list: vec![],
            set: FxHashSet::default(),
        }
    }
}

impl<T: Clone + Eq + std::hash::Hash> Ordered<T> {
    fn insert(&mut self, x: T) {
        if self.set.insert(x.clone()) {
            self.list.push(x);
        }
    }
}

/// A function's stores by key into what its parameters hold (the parameter, and where the value
/// comes from; the keys are in [`super::flow`]), and the calls through its function-typed
/// parameters (the parameter, and the shape of its arguments).
#[derive(Default)]
pub(super) struct Summary {
    stores: Ordered<(u32, Src)>,
    latent: Ordered<(u32, Shape)>,
}

impl Summary {
    fn len(&self) -> (u32, u32) {
        (self.stores.list.len() as u32, self.latent.list.len() as u32)
    }
}

/// The joins of function types: the functions in each, and how much of each one's summary each
/// join has taken in.
#[derive(Default)]
pub(super) struct Joins {
    /// For each function type, whether a call through it is joined (`false`: it may run a value
    /// made outside the program, reported where it is called).
    made: FxHashMap<TyId, bool>,
    of: FxHashMap<DefId, Vec<TyId>>,
    taken: FxHashMap<(TyId, DefId), (u32, u32)>,
}

/// What of a queued function to apply (again): events (all of them the first time), and
/// applications of a summary that grew since (the summary, the shapes of the arguments and of
/// the captures, and the event they are part of).
#[derive(Default)]
pub(super) struct Pending {
    pub events: Vec<u32>,
    pub apps: FxHashSet<(Node, Shape, Shape, u32)>,
}

/// Apply what is pending for function `d`, and notify the users of `d`'s summary if it grew.
pub(super) fn summarize(cx: &mut Ctx, run: &mut Run, d: DefId, pending: Pending) {
    let Some(p) = run.prepared.remove(&d) else {
        return;
    };
    let before = run.sums.get(&Node::Fn(d)).map(Summary::len);
    for &i in &pending.events {
        let Some(ev) = p.events.get(i as usize) else {
            continue;
        };
        let mut b = Body {
            me: d,
            ev: i,
            params: &p.params,
        };
        match ev {
            Ev::Store {
                target,
                key,
                value,
                site,
            } => {
                let from = run.flows.seeded(key);
                b.store(cx, run, target, from, value, site);
            }
            Ev::Call { targets, site } => {
                for &(g, shape) in targets {
                    b.apply(cx, run, Node::Fn(g), shape, CALLED, site);
                }
            }
            Ev::Value { fns, actual, site } => b.call_values(cx, run, fns, *actual, site),
            Ev::Closure { k, caps, site } => b.apply(cx, run, Node::Fn(*k), CALLED, *caps, site),
        }
    }
    let mut apps: Vec<(Node, Shape, Shape, u32)> = pending.apps.into_iter().collect();
    apps.sort_unstable_by_key(|&(n, a, c, e)| (e, a, c, node_order(n)));
    for (n, actual, caps, i) in apps {
        let Some(ev) = p.events.get(i as usize) else {
            continue;
        };
        let mut b = Body {
            me: d,
            ev: i,
            params: &p.params,
        };
        b.apply(cx, run, n, actual, caps, ev.site());
    }
    run.prepared.insert(d, p);
    if run.sums.get(&Node::Fn(d)).map(Summary::len) != before {
        grew(run, Node::Fn(d));
    }
}

/// An order of nodes that does not depend on hashing, so that which store a field's error names
/// does not change from run to run.
fn node_order(n: Node) -> (u8, u32) {
    match n {
        Node::Fn(d) => (0, d.0),
        Node::Join(t) => (1, t.0),
    }
}

/// The summary of `n` grew: the applications of it are queued again, and the joins `n` is in
/// take in what is new.
fn grew(run: &mut Run, n: Node) {
    let mut users: Vec<(DefId, Shape, Shape, u32)> =
        run.users.get(&n).into_iter().flatten().copied().collect();
    users.sort_unstable_by_key(|&(u, a, c, e)| (u.0, e, a, c));
    for (u, a, c, e) in users {
        run.dirty.entry(u).or_default().apps.insert((n, a, c, e));
        if run.queued.insert(u) {
            run.work.push_back(u);
        }
    }
    let Node::Fn(g) = n else {
        return;
    };
    let joins = run.joins.of.get(&g).cloned().unwrap_or_default();
    for t in joins {
        if take_in(run, t, g) {
            grew(run, Node::Join(t));
        }
    }
}

/// Join `t` takes in what is new in function `g`'s summary, as a call (not the making of a
/// closure) applies it: its stores into what it captured are not the call's, and its parameters
/// count from the first after its captures. Did the join grow?
fn take_in(run: &mut Run, t: TyId, g: DefId) -> bool {
    let ncap = run.ncap.get(&g).copied().unwrap_or(0);
    let (s0, l0) = run.joins.taken.get(&(t, g)).copied().unwrap_or((0, 0));
    let Some(s) = run.sums.get(&Node::Fn(g)) else {
        return false;
    };
    let (s1, l1) = s.len();
    if (s0, l0) == (s1, l1) {
        return false;
    }
    run.joins.taken.insert((t, g), (s1, l1));
    let stores: Vec<((u32, Src), (u32, Src))> = (s.stores.list[s0 as usize..s1 as usize].iter())
        .filter_map(|&(q, src)| {
            let into = q.checked_sub(ncap)?;
            let from = match src {
                Src::Param(r) => r.checked_sub(ncap).map_or(Src::Other, Src::Param),
                Src::Other => Src::Other,
            };
            Some(((q, src), (into, from)))
        })
        .collect();
    let latent: Vec<(u32, Shape)> = s.latent.list[l0 as usize..l1 as usize].to_vec();
    let mut added = vec![];
    for (fp, args) in latent {
        let Some(fp) = fp.checked_sub(ncap) else {
            continue;
        };
        let Some(args) = run.shapes.get(args) else {
            continue;
        };
        // A capture among the arguments: what it is is not known at a call.
        let mapped: Vec<Vec<Arg>> = (args.iter())
            .map(|xs| {
                (xs.iter())
                    .filter_map(|x| match x {
                        Arg::Param(m) => m.checked_sub(ncap).map(Arg::Param),
                        other => Some(other.clone()),
                    })
                    .collect()
            })
            .collect();
        added.push((fp, run.shapes.id(&mapped)));
    }
    let j = run.sums.entry(Node::Join(t)).or_default();
    let before = j.len();
    for (_, x) in &stores {
        j.stores.insert(*x);
    }
    for x in added {
        j.latent.insert(x);
    }
    let grew = j.len() != before;
    run.steps += stores.len() as u64 + 1;
    for ((q, src), (into, from)) in stores {
        let a = run.flows.set(Node::Fn(g), q, src);
        let b = run.flows.set(Node::Join(t), into, from);
        run.flows.edge(a, b);
    }
    grew
}

/// Event `ev` of function `me` being applied.
struct Body<'a> {
    me: DefId,
    ev: u32,
    params: &'a [TyId],
}

impl Body<'_> {
    /// A store by the keys of `from` ([`super::flow`]) into what `target` may be of what `value`
    /// may be, at `site`.
    fn store(
        &mut self,
        cx: &mut Ctx,
        run: &mut Run,
        target: &[Arg],
        from: u32,
        value: &[Arg],
        site: &Site,
    ) {
        let srcs: Vec<Src> = (value.iter())
            .filter_map(|v| match v {
                Arg::Own | Arg::New(_) | Arg::Fns(_) => None,
                Arg::Param(m) => Some(Src::Param(*m)),
                Arg::Ty(_) | Arg::Other => Some(Src::Other),
            })
            .collect();
        for src in srcs {
            for t in target {
                match t {
                    Arg::Param(p) => {
                        let me = run.sums.entry(Node::Fn(self.me)).or_default();
                        me.stores.insert((*p, src));
                        let to = run.flows.set(Node::Fn(self.me), *p, src);
                        run.flows.edge(from, to);
                        // A parameter of a class type is that class (or a subclass) at every
                        // caller; one the store is given a value that is not new in feeds it now.
                        let ty = self.params.get(*p as usize).copied();
                        if let (Src::Other, Some(ty)) = (src, ty) {
                            if adt_of(cx, ty).is_some() {
                                run.flows.sink(from, ty, site);
                            }
                        }
                    }
                    // A value of a parameter may still be new at the caller; feeding the class
                    // now is the conservative answer. A new object's fields are fed too.
                    Arg::Ty(ty) | Arg::New(ty) => run.flows.sink(from, *ty, site),
                    Arg::Own | Arg::Fns(_) | Arg::Other => {}
                }
            }
        }
    }

    /// The call of `n` (a function, or a join) with arguments of shape `actual` (`this` first
    /// for a method), or (`caps`) the making of closure `n` with captures of that shape: what
    /// its summary added since this event last applied it to these shapes.
    fn apply(
        &mut self,
        cx: &mut Ctx,
        run: &mut Run,
        n: Node,
        actual: Shape,
        caps: Shape,
        site: &Site,
    ) {
        run.steps += 1;
        if let Node::Fn(g) = n {
            ensure(cx, run, g);
        }
        (run.users.entry(n).or_default()).insert((self.me, actual, caps, self.ev));
        let Some(s) = run.sums.get(&n) else {
            return;
        };
        let (s1, l1) = s.len();
        let key = (self.me, n, actual, caps);
        let (s0, l0) = run.applied.get(&key).copied().unwrap_or((0, 0));
        if (s0, l0) == (s1, l1) {
            return;
        }
        // Recorded first: a latent call that leads back here (a function value passed around
        // in a cycle) adds nothing new and stops.
        run.applied.insert(key, (s1, l1));
        run.steps += u64::from(s1 - s0) + u64::from(l1 - l0);
        let stores: Vec<(u32, Src)> = s.stores.list[s0 as usize..s1 as usize].to_vec();
        let latent: Vec<(u32, Shape)> = s.latent.list[l0 as usize..l1 as usize].to_vec();
        let ncap = match n {
            Node::Fn(g) => run.ncap.get(&g).copied().unwrap_or(0),
            Node::Join(_) => 0,
        };
        let actual = run.shapes.get(actual).unwrap_or_else(|| Rc::from([]));
        let caps = run.shapes.get(caps);
        // A closure's captures are bound when it is made, its parameters when it is called:
        // making it applies only its stores into what it captured, calling it only the rest.
        let making = caps.is_some();
        let arg = |q: u32| -> Option<Vec<Arg>> {
            match (q.checked_sub(ncap), &caps) {
                (None, Some(cs)) => cs.get(q as usize).cloned(),
                (None, None) => None,
                (Some(i), None) => actual.get(i as usize).cloned(),
                // A parameter of a closure being made: what it will be given is not known here.
                (Some(_), Some(_)) => Some(vec![Arg::Other]),
            }
        };
        for &(q, src) in &stores {
            if making != (q < ncap) {
                continue;
            }
            let Some(target) = arg(q) else { continue };
            let value = match src {
                Src::Param(r) => arg(r).unwrap_or_else(|| vec![Arg::Other]),
                Src::Other => vec![Arg::Other],
            };
            let from = run.flows.set(n, q, src);
            self.store(cx, run, &target, from, &value, site);
        }
        for (fp, largs) in latent {
            if making != (fp < ncap) {
                continue;
            }
            let (Some(fns), Some(largs)) = (arg(fp), run.shapes.get(largs)) else {
                continue;
            };
            // The latent call's arguments, as the caller sees them.
            let mapped: Vec<Vec<Arg>> = (largs.iter())
                .map(|xs| {
                    let mut out = vec![];
                    for x in xs {
                        match x {
                            Arg::Param(m) => out.extend(arg(*m).unwrap_or_default()),
                            other => out.push(other.clone()),
                        }
                    }
                    out
                })
                .collect();
            let mapped = run.shapes.id(&mapped);
            self.call_values(cx, run, &fns, mapped, site);
        }
    }

    /// A call through a function value that may be `fns`, with arguments of shape `actual`.
    fn call_values(
        &mut self,
        cx: &mut Ctx,
        run: &mut Run,
        fns: &[Arg],
        actual: Shape,
        site: &Site,
    ) {
        for fv in fns {
            match fv {
                Arg::Fns(gs) => {
                    for &g in gs {
                        self.apply(cx, run, Node::Fn(g), actual, CALLED, site);
                    }
                }
                // Resolved by each caller against the function it passes.
                Arg::Param(p) => {
                    let me = run.sums.entry(Node::Fn(self.me)).or_default();
                    me.latent.insert((*p, actual));
                }
                // A function value read from the heap: any of the program's that accept the
                // arguments, as for the check itself, through the join of its type.
                Arg::Ty(ty) => {
                    if join(cx, run, *ty) {
                        self.apply(cx, run, Node::Join(*ty), actual, CALLED, site);
                    }
                }
                Arg::Own | Arg::New(_) | Arg::Other => {}
            }
        }
    }
}

/// Make the join of function type `ty` the first time a call goes through it: the program's
/// function values that accept its parameters, each prepared, and what their summaries hold so
/// far. `false` when no join stands for it (not a function type, or a value made outside the
/// program may be called, which is reported where it is called).
fn join(cx: &mut Ctx, run: &mut Run, ty: TyId) -> bool {
    if let Some(&made) = run.joins.made.get(&ty) {
        return made;
    }
    let want = match cx.ty.kind(ty) {
        TyKind::FnPtr { params, .. } => params.clone(),
        _ => {
            run.joins.made.insert(ty, false);
            return false;
        }
    };
    let Some(targets) = value_targets(cx, run.lits, &want, false, 0) else {
        run.joins.made.insert(ty, false);
        return false;
    };
    run.joins.made.insert(ty, true);
    run.sums.entry(Node::Join(ty)).or_default();
    for (g, _) in targets {
        ensure(cx, run, g);
        run.joins.of.entry(g).or_default().push(ty);
        take_in(run, ty, g);
    }
    true
}
