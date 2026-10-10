//! Sync callbacks that HTTP requests call at the same time (#873): an HTTP handler only reads
//! what it captured, but a function value it reaches (`i.onChange("x")`, with
//! `i.onChange = (v) => { last = v; }`) may assign a variable that function captured itself.
//! Requests run on several threads at once and share what the handler captured, so they would
//! all assign that one variable at once. Such a handler is an error, naming the call and the
//! variable, with a fix-it that shares the variable with `shared(...)` as Node shares it.
//!
//! What a handler reaches is followed on the graph of [`super`]: the values flowing into the
//! handler `serve` passes to `Intrinsic::HttpHandler`, the captures of the closures among them,
//! and through the heap by type ([`super::types`]): a sync closure stored in a field, element or
//! map value is reached when a value the handler reaches has a type holding a function type of
//! its shape, written without generic parameters (the prelude's `resolve`, a `(T) => void`,
//! stands for no callback the user stores), and in the direction values flow ([`fits`]: a
//! `() => void` closure is never a `() => string`). A closure stored straight into an object or
//! array that one local of its function holds (or through a method keeping it in `this`), and
//! that never leaves that function, is reached only when that local is ([`held_closures`]):
//! `other.onChange = …` on an `Input` the handler never sees is not one the handler's `Input`
//! may hold, and one only ever stored in fields no code a request may run uses is not reached
//! ([`Bodies`]). Closures made by a request (inside a reached async closure) assign that request's
//! own variables, and the standard library's closures are not the user's to change, so neither
//! is reported. A reached closure's assignments include those of the closures it makes.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, IntTy, Intrinsic, Lit, LocalId, Pat,
    PatKind, Stmt, StmtKind as S, TyId, TyKind, UnOp,
};
use crate::ownership::local_closures::walk::{self, Visit};

use super::graph::{Boundary, Graph, Node, Why, CROSSES, STORED};
use super::types;

/// Report the sync closures HTTP handlers reach that assign a variable they captured (module
/// docs). `flags` are the propagated node flags of [`super`] (`STORED` is used); `handlers`
/// are the sync closures passed to `serve` as handlers, checked as handlers themselves.
pub(super) fn check_handler_callbacks(
    cx: &mut Ctx,
    g: &Graph,
    flags: &[u8],
    handlers: &HashMap<DefId, Option<Why>>,
) {
    let mut work: Vec<(usize, Why)> = g
        .seeds
        .iter()
        .filter(|(_, f, _)| *f == CROSSES)
        .filter_map(|&(n, _, w)| {
            w.filter(|w| w.boundary == Boundary::Handler)
                .map(|w| (n, w))
        })
        .collect();
    let mut roots: Vec<(TyId, Why)> = g
        .roots
        .iter()
        .filter(|(_, w)| w.boundary == Boundary::Handler)
        .copied()
        .collect();
    if work.is_empty() && roots.is_empty() {
        return;
    }
    let (held, homes) = held_closures(cx, g, flags);
    let mut bodies = Bodies::new(cx);
    bodies.homes = homes;
    // Nodes whose values flow somewhere the graph follows.
    let flowing: HashSet<usize> = g.srcs.iter().flatten().map(|(n, _)| *n).collect();
    let mut why: Vec<Option<Why>> = vec![None; g.nodes.len()];
    let mut seen = HashSet::new();
    let mut fns: HashMap<TyId, Why> = HashMap::new();
    let mut rooted = 0;
    loop {
        while let Some((n, w)) = work.pop() {
            if why[n].is_some() {
                continue;
            }
            why[n] = Some(w);
            for &(m, site) in &g.srcs[n] {
                work.push((m, w.through(site)));
            }
            if let Some(caps) = g.captures.get(&n) {
                work.extend(caps.iter().map(|&i| (i, w)));
            }
            let ty = g.tys[n];
            if !matches!(g.nodes[n], Node::Lit(_)) && (g.unknown[n] || !types::fn_like(cx, ty)) {
                roots.push((ty, w));
            }
            for &(t, site) in &g.inflows[n] {
                if t != ty {
                    roots.push((t, w.through(site)));
                }
            }
        }
        types::crossing_fns(cx, &roots[rooted..], &mut seen, &mut fns);
        rooted = roots.len();
        // A closure only ever stored in fields that no code a request may run reads
        // (`w.onClick = …` while the handler reads only `w.name`) is never called by one.
        let reads =
            bodies.request_reads(g.nodes.iter().enumerate().filter_map(|(n, node)| {
                match (node, why[n]) {
                    (Node::Lit(c), Some(_)) => Some(*c),
                    _ => None,
                }
            }));
        let unread = |n: usize, c: DefId| {
            let Some(reads) = &reads else { return false };
            !flowing.contains(&n)
                && bodies.stores.get(&c).is_some_and(|fs| {
                    fs.iter().all(|&(a, k)| {
                        cx.adt(a)
                            .and_then(|i| i.fields.get(k as usize))
                            .is_some_and(|f| !reads.contains(&f.name))
                    })
                })
        };
        for (n, node) in g.nodes.iter().enumerate() {
            if !matches!(node, Node::Lit(_)) || why[n].is_some() || flags[n] & STORED == 0 {
                continue;
            }
            if let Some(&x) = held.get(&n) {
                if let Some(w) = why[x] {
                    work.push((n, w));
                }
                continue;
            }
            if matches!(node, Node::Lit(c) if unread(n, *c)) {
                continue;
            }
            let found = fns
                .iter()
                .filter(|(t, _)| !types::mentions_param(cx, **t) && fits(cx, g.tys[n], **t))
                .map(|(_, w)| *w)
                .min_by_key(|w| (w.std, w.span.file, w.span.lo));
            if let Some(w) = found {
                work.push((n, w));
            }
        }
        if work.is_empty() {
            break;
        }
    }
    let reached: Vec<(DefId, TyId, Why)> = g
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(n, node)| match (node, why[n]) {
            (Node::Lit(c), Some(w)) => Some((*c, g.tys[n], w)),
            _ => None,
        })
        .collect();
    let requests: HashSet<DefId> = reached
        .iter()
        .filter(|(c, ..)| matches!(&cx.defs[c.0 as usize], Some(Def::Fn(f)) if f.is_async))
        .map(|(c, ..)| *c)
        .collect();
    let mut found: Vec<(DefId, TyId, Why, LocalId, String, TyId, Span)> = vec![];
    for &(c, ty, w) in &reached {
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        if f.is_async
            || f.is_generator
            || handlers.contains_key(&c)
            || in_std(cx, c)
            || made_by(g, c, &requests)
        {
            continue;
        }
        for (cap, at) in assigned_captures(cx, f) {
            let local = &f.body.locals[cap.0 as usize];
            // An assignment in a closure made inside another reached closure is reported once.
            if cx.ty.kind(local.ty) != &TyKind::Unit && !found.iter().any(|x| x.6 == at) {
                found.push((c, ty, w, cap, local.name.clone(), local.ty, at));
            }
        }
    }
    for (c, ty, w, cap, name, cap_ty, at) in found {
        let call = call_site(cx, g, &reached, &requests, &bodies, c, ty);
        let init = declaration(cx, g, c, cap);
        report(cx, &name, cap_ty, at, call, init, w);
    }
}

/// Closure literals stored straight into an object or array held by one local of the function
/// making them (`other.onChange = (v) => …`, `cbs.push(() => …)`, `const o = { f: () => … }`,
/// or through a setter, `e.on(() => …)` with `on(f) { this.listeners.push(f); }`), by literal
/// node, with that local's node: when the local never leaves the function (it is not stored,
/// captured, returned or passed to a function of the program, only to the standard library's
/// and as `this` of methods that keep it too), only that local reaches the closure, so it is
/// reached exactly when the local is, not by its type. Also returns, per closure, the nodes of
/// all the locals it is stored into that way (where it is stored, whether or not they leave).
#[allow(clippy::type_complexity)]
fn held_closures(
    cx: &Ctx,
    g: &Graph,
    flags: &[u8],
) -> (HashMap<usize, usize>, HashMap<DefId, Vec<usize>>) {
    let mut outs: HashMap<usize, Vec<usize>> = HashMap::new();
    for (m, srcs) in g.srcs.iter().enumerate() {
        for &(n, _) in srcs {
            outs.entry(n).or_default().push(m);
        }
    }
    let setters = setters(cx);
    let mut out = HashMap::new();
    let mut homes: HashMap<DefId, Vec<usize>> = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = def else { continue };
        if !g.parent.values().any(|p| *p == d) || in_std(cx, d) {
            continue;
        }
        let mut sites = vec![];
        walk::block(&f.body.block, &mut Sites(cx, &setters, &mut sites));
        for (c, x) in sites {
            let (Some(&lit), Some(&local)) =
                (g.ids.get(&Node::Lit(c)), g.ids.get(&Node::Local(d, x)))
            else {
                continue;
            };
            homes.entry(c).or_default().push(local);
            if (x.0 as usize) < f.params.len() {
                continue;
            }
            if flags[local] & STORED == 0
                && stays(cx, g, &outs, flags, &setters, local, &mut HashSet::new())
                && stays(cx, g, &outs, flags, &setters, lit, &mut HashSet::new())
            {
                out.insert(lit, local);
            }
        }
    }
    (out, homes)
}

/// Do the values of node `n` go only to parameters of standard library functions, to setter
/// parameters (which keep them in their `this`), and as `this` to methods of the program where
/// they stay too?
fn stays(
    cx: &Ctx,
    g: &Graph,
    outs: &HashMap<usize, Vec<usize>>,
    flags: &[u8],
    setters: &HashMap<DefId, Vec<usize>>,
    n: usize,
    seen: &mut HashSet<usize>,
) -> bool {
    if !seen.insert(n) || seen.len() > 256 {
        return seen.len() <= 256;
    }
    outs.get(&n)
        .into_iter()
        .flatten()
        .all(|&m| match g.nodes[m] {
            Node::Local(f, _) if in_std(cx, f) => true,
            Node::Local(f, l) => match &cx.defs[f.0 as usize] {
                Some(Def::Fn(ff)) if ff.self_ty.is_some() && ff.captures.is_empty() => {
                    let k = ff.params.iter().position(|p| p.local == l);
                    (k.is_some_and(|k| setters.get(&f).is_some_and(|ks| ks.contains(&k))))
                        || (k == Some(0)
                            && flags[m] & STORED == 0
                            && stays(cx, g, outs, flags, setters, m, seen))
                }
                _ => false,
            },
            _ => false,
        })
}

/// The methods of the program that only keep some of their parameters in what `this` holds
/// (`on(f) { this.listeners.push(f); }`, `set cb(f) { this.f = f; }`), with those parameters'
/// positions (`this` is 0).
fn setters(cx: &Ctx) -> HashMap<DefId, Vec<usize>> {
    let mut out = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = def else { continue };
        if f.self_ty.is_none() || !f.captures.is_empty() || f.params.len() < 2 || in_std(cx, d) {
            continue;
        }
        let this = f.params[0].local;
        // Per parameter: its uses, and those that keep it in `this`.
        let mut uses: HashMap<LocalId, (u32, u32)> = HashMap::new();
        let mut kept = |x: &Expr| {
            if let E::Local(l, _) = x.kind {
                uses.entry(l).or_default().1 += 1;
            }
        };
        let mut all = vec![];
        each_expr(&f.body.block, &mut |e: &Expr| match &e.kind {
            E::Local(l, _) => all.push(*l),
            E::Assign { place, value }
                if root(place) == Some(this) && !matches!(place.kind, E::Local(..)) =>
            {
                kept(value)
            }
            E::Call { callee, args }
                if std_method(cx, callee) && args.first().and_then(root) == Some(this) =>
            {
                args[1..].iter().for_each(&mut kept)
            }
            _ => {}
        });
        for l in all {
            uses.entry(l).or_default().0 += 1;
        }
        let ks: Vec<usize> = (1..f.params.len())
            .filter(|&k| {
                let p = &f.params[k];
                types::fn_like(cx, p.ty)
                    && uses
                        .get(&p.local)
                        .is_some_and(|(n, kept)| *n == *kept && *kept > 0)
            })
            .collect();
        if !ks.is_empty() {
            out.insert(d, ks);
        }
    }
    out
}

/// The closure literals stored straight into what a local holds, with the local (see
/// [`held_closures`]).
struct Sites<'a, 'm>(
    &'a Ctx<'m>,
    &'a HashMap<DefId, Vec<usize>>,
    &'a mut Vec<(DefId, LocalId)>,
);

impl Visit for Sites<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        let S::Let {
            local,
            init: Some(init),
        } = &s.kind
        else {
            return;
        };
        if let E::AdtLit { fields: xs, .. } | E::ArrayLit(xs) | E::New { args: xs, .. } = &init.kind
        {
            for x in xs {
                if let E::Closure(c) = x.kind {
                    self.2.push((c, *local));
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Assign { place, value } => {
                if let (E::Closure(c), false) = (&value.kind, matches!(place.kind, E::Local(..))) {
                    if let Some(x) = root(place) {
                        self.2.push((*c, x));
                    }
                }
            }
            E::Call { callee, args } => {
                let held: Vec<usize> = match callee {
                    _ if std_method(self.0, callee) => (1..args.len()).collect(),
                    Callee::Def(g, _) => self.1.get(g).cloned().unwrap_or_default(),
                    _ => vec![],
                };
                if let Some(x) = args.first().and_then(root).filter(|_| !held.is_empty()) {
                    for k in held {
                        if let Some(E::Closure(c)) = args.get(k).map(|a| &a.kind) {
                            self.2.push((*c, x));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// A method of the standard library (`push`, `set`), which may keep its arguments in what its
/// receiver holds but nowhere else.
fn std_method(cx: &Ctx, callee: &Callee) -> bool {
    match callee {
        Callee::Def(g, _) => {
            in_std(cx, *g)
                && matches!(&cx.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.self_ty.is_some())
        }
        Callee::Intrinsic(Intrinsic::ArrayPush) => true,
        _ => false,
    }
}

/// The local a place is part of: `x`, `x.f`, `x[i]`, `x.f[i].g`.
fn root(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::Field { base, .. } | E::Index { base, .. } => root(base),
        _ => None,
    }
}

/// Is closure `c` made inside one of `requests` (a request's own closure)?
fn made_by(g: &Graph, c: DefId, requests: &HashSet<DefId>) -> bool {
    let mut d = c;
    for _ in 0..64 {
        match g.parent.get(&d) {
            Some(p) if requests.contains(p) => return true,
            Some(p) => d = *p,
            None => return false,
        }
    }
    false
}

fn in_std(cx: &Ctx, d: DefId) -> bool {
    cx.try_fn(d).is_some_and(|i| cx.scopes[i.module].is_std)
}

/// May a closure literal of type `lit` be a function value of type `f`? As
/// [`types::may_be`], and in the direction values flow: a closure returning nothing is never
/// called where a value is expected (`() => void` is not a `() => string`; the reverse is, #776).
fn fits(cx: &Ctx, lit: TyId, f: TyId) -> bool {
    let unit = |t: TyId| matches!(cx.ty.kind(t), TyKind::FnPtr { ret, .. } if cx.ty.kind(*ret) == &TyKind::Unit);
    types::may_be(cx, lit, f) && (!unit(lit) || unit(f))
}

/// Calls `f` on every expression of `b`.
fn each_expr(b: &Block, f: &mut dyn FnMut(&Expr)) {
    struct Exprs<'a>(&'a mut dyn FnMut(&Expr));
    impl Visit for Exprs<'_> {
        fn expr(&mut self, e: &Expr) {
            (self.0)(e);
        }
    }
    walk::block(b, &mut Exprs(f));
}

/// The captured variables closure `f` assigns (`x = …`, `x += …`, `x++`), itself or in a
/// closure it makes (`const bump = () => { x++; }`), with the first assignment of each.
fn assigned_captures(cx: &Ctx, f: &FnDef) -> Vec<(LocalId, Span)> {
    // `f`'s captures, by the local each one is in the function being searched.
    let caps: HashMap<LocalId, LocalId> = f.captures.iter().map(|c| (c.inner, c.inner)).collect();
    let mut out: Vec<(LocalId, Span)> = vec![];
    assignments(cx, f, &caps, &mut out, 0);
    out
}

/// The assignments in `f` (and the closures it makes) to locals of `caps`, as the capture of
/// the outermost closure they stand for.
fn assignments(
    cx: &Ctx,
    f: &FnDef,
    caps: &HashMap<LocalId, LocalId>,
    out: &mut Vec<(LocalId, Span)>,
    depth: u32,
) {
    let mut inner = vec![];
    each_expr(&f.body.block, &mut |e: &Expr| match &e.kind {
        E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
            if let Some(&cap) = match place.kind {
                E::Local(l, _) => caps.get(&l),
                _ => None,
            } {
                if !out.iter().any(|(x, _)| *x == cap) {
                    out.push((cap, e.span));
                }
            }
        }
        E::Closure(k) => inner.push(*k),
        _ => {}
    });
    if depth > 16 {
        return;
    }
    for k in inner {
        let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] else {
            continue;
        };
        let kcaps: HashMap<LocalId, LocalId> = kf
            .captures
            .iter()
            .filter_map(|c| caps.get(&c.outer).map(|&cap| (c.inner, cap)))
            .collect();
        if !kcaps.is_empty() {
            assignments(cx, kf, &kcaps, out, depth + 1);
        }
    }
}

/// Where a handler calls a function value that may be closure `c` (of type `ty`), as written
/// (`i.onChange`), with the call's span: in a request's closure first, else in another closure
/// the handler reaches. A call of a field `c` is stored in, on what `c` was stored into
/// (`i.onChange` with `i.onChange = …`), is preferred, then of that field on anything, or
/// through a variable `c` flows into, then a call through a function value of `c`'s type, then
/// of its shape.
fn call_site(
    cx: &Ctx,
    g: &Graph,
    reached: &[(DefId, TyId, Why)],
    requests: &HashSet<DefId>,
    bodies: &Bodies,
    c: DefId,
    ty: TyId,
) -> Option<(String, Span)> {
    let mut order: Vec<DefId> = reached.iter().map(|(d, ..)| *d).collect();
    order.sort_by_key(|d| !requests.contains(d));
    let lit = g.ids.get(&Node::Lit(c)).copied();
    let homes = bodies.homes.get(&c).map(Vec::as_slice).unwrap_or_default();
    let flows = |n: usize, to: &[usize]| {
        let mut seen = HashSet::new();
        let mut work = vec![n];
        while let Some(m) = work.pop() {
            if to.contains(&m) {
                return true;
            }
            if seen.insert(m) {
                work.extend(g.srcs[m].iter().map(|(s, _)| *s));
            }
        }
        false
    };
    let ret = |t: TyId| match cx.ty.kind(t) {
        TyKind::FnPtr { ret, .. } => Some(*ret),
        _ => None,
    };
    let fields = &bodies.stores;
    let mut best: Option<(u8, bool, u32, String, Span)> = None;
    for d in order {
        if d == c || in_std(cx, d) {
            continue;
        }
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        let request = requests.contains(&d);
        // An argument that may be `c`: `tick`, or a closure capturing it (`setTimeout(tick, 0)`
        // passes the adapter `async () => tick()`).
        let passes1 = |x: &Expr| match x.kind {
            E::Local(l, _) => g
                .ids
                .get(&Node::Local(d, l))
                .is_some_and(|&n| flows(n, lit.as_slice())),
            E::Closure(w) => {
                matches!(&cx.defs[w.0 as usize], Some(Def::Fn(wf)) if wf.captures.iter().any(|k| {
                    g.ids
                        .get(&Node::Local(d, k.outer))
                        .is_some_and(|&n| flows(n, lit.as_slice()))
                }))
            }
            _ => false,
        };
        let passes = |x: &Expr| match &x.kind {
            // The adapter is made in a block (`{ let f = tick; async () => f() }`).
            E::Block(b) => b.value.as_ref().is_some_and(|v| passes1(v)),
            _ => passes1(x),
        };
        each_expr(&f.body.block, &mut |e: &Expr| {
            // `c` passed to the standard library, which calls it.
            if let E::Call {
                callee: Callee::Def(s, _),
                args,
            } = &e.kind
            {
                if in_std(cx, *s) && args.iter().any(passes) {
                    let key = (1, !request, e.span.lo);
                    if let Some(text) = shown(cx, f, e) {
                        if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
                            best = Some((1, !request, e.span.lo, text, e.span));
                        }
                    }
                }
                return;
            }
            let E::Call {
                callee: Callee::Indirect(callee),
                ..
            } = &e.kind
            else {
                return;
            };
            let field = match &callee.kind {
                E::Field { base, index, .. } => match cx.ty.kind(base.ty) {
                    TyKind::Adt(a, _) => fields.get(&c).map(|fs| fs.contains(&(*a, *index))),
                    _ => None,
                },
                _ => None,
            };
            let node = |x: &Expr| match root(x) {
                Some(l) => g.ids.get(&Node::Local(d, l)).copied(),
                None => None,
            };
            let rank = match &callee.kind {
                E::Field { base, .. } if field == Some(true) => {
                    if node(base).is_some_and(|n| flows(n, homes)) {
                        0
                    } else {
                        1
                    }
                }
                _ if field == Some(false) => return,
                E::Local(..) if node(callee).is_some_and(|n| flows(n, lit.as_slice())) => 1,
                _ if callee.ty == ty => 2,
                _ if types::may_be(cx, ty, callee.ty) && ret(ty) == ret(callee.ty) => 3,
                _ => return,
            };
            let Some(text) = shown(cx, f, callee) else {
                return;
            };
            let key = (rank, !request, e.span.lo);
            if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
                best = Some((rank, !request, e.span.lo, text, e.span));
            }
        });
    }
    best.map(|b| (b.3, b.4))
}

/// What the bodies of the program's functions do with fields, read once.
#[derive(Default)]
struct Bodies {
    /// The fields each closure literal is assigned to (`i.onChange = (v) => …`,
    /// `{ f: () => … }`), as `(class or object type, field)`.
    stores: HashMap<DefId, Vec<(DefId, u32)>>,
    /// Per function: the names of the fields it reads (or writes), or `None` when it uses one
    /// whose name is not known.
    reads: HashMap<DefId, Option<HashSet<String>>>,
    /// Per function: the functions of the program it calls directly.
    calls: HashMap<DefId, Vec<DefId>>,
    /// The program's methods and the functions it uses as values (`const f = poke`): any of
    /// them may run in a request.
    entries: Vec<DefId>,
    /// Per closure: the nodes of the locals it is stored into ([`held_closures`]).
    homes: HashMap<DefId, Vec<usize>>,
}

impl Bodies {
    fn new(cx: &Ctx) -> Bodies {
        let mut out = Bodies::default();
        for (i, def) in cx.defs.iter().enumerate() {
            let d = DefId(i as u32);
            let Some(Def::Fn(f)) = def else { continue };
            if in_std(cx, d) {
                continue;
            }
            if f.self_ty.is_some() {
                out.entries.push(d);
            }
            let mut reads = Some(HashSet::new());
            walk::block(
                &f.body.block,
                &mut FieldUses {
                    cx,
                    reads: &mut reads,
                },
            );
            let mut calls = vec![];
            each_expr(&f.body.block, &mut |e: &Expr| match &e.kind {
                E::Assign { place, value } => {
                    if let (E::Field { base, index, .. }, E::Closure(c)) =
                        (&place.kind, &value.kind)
                    {
                        if let TyKind::Adt(a, _) = cx.ty.kind(base.ty) {
                            out.stores.entry(*c).or_default().push((*a, *index));
                        }
                    }
                }
                E::AdtLit { def, fields, .. } => {
                    for (k, x) in fields.iter().enumerate() {
                        if let E::Closure(c) = x.kind {
                            out.stores.entry(c).or_default().push((*def, k as u32));
                        }
                    }
                }
                E::Call {
                    callee: Callee::Def(g, _),
                    ..
                } if !in_std(cx, *g) => calls.push(*g),
                E::FnRef(g, _) if !in_std(cx, *g) => out.entries.push(*g),
                _ => {}
            });
            out.reads.insert(d, reads);
            out.calls.insert(d, calls);
        }
        out
    }

    /// The names of the fields that code a request may run uses: the closures `reached`, the
    /// program's methods and functions used as values, and what they call. `None`: unknown.
    fn request_reads(&self, reached: impl Iterator<Item = DefId>) -> Option<HashSet<String>> {
        let mut work: Vec<DefId> = reached.chain(self.entries.iter().copied()).collect();
        let mut seen = HashSet::new();
        let mut out = HashSet::new();
        while let Some(d) = work.pop() {
            if !seen.insert(d) {
                continue;
            }
            match self.reads.get(&d) {
                Some(Some(names)) => out.extend(names.iter().cloned()),
                Some(None) => return None,
                None => {}
            }
            work.extend(self.calls.get(&d).into_iter().flatten().copied());
        }
        Some(out)
    }
}

/// Collects the names of the fields a body uses, in expressions and patterns.
struct FieldUses<'a, 'm> {
    cx: &'a Ctx<'m>,
    reads: &'a mut Option<HashSet<String>>,
}

impl FieldUses<'_, '_> {
    fn field(&mut self, ty: TyId, index: u32) {
        let name = match self.cx.ty.kind(ty) {
            TyKind::Adt(a, _) => self
                .cx
                .adt(*a)
                .and_then(|i| i.fields.get(index as usize))
                .map(|f| f.name.clone()),
            _ => None,
        };
        match (name, self.reads.as_mut()) {
            (Some(n), Some(reads)) => {
                reads.insert(n);
            }
            _ => *self.reads = None,
        }
    }

    fn pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Adt { fields } => {
                for (k, sub) in fields {
                    self.field(p.ty, *k);
                    self.pat(sub);
                }
            }
            PatKind::Variant { args: xs, .. }
            | PatKind::Tuple(xs)
            | PatKind::Or(xs)
            | PatKind::Array { elems: xs, .. } => xs.iter().for_each(|x| self.pat(x)),
            PatKind::Some(x) => self.pat(x),
            _ => {}
        }
    }
}

impl Visit for FieldUses<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        if let S::LetPat { pat, .. } = &s.kind {
            self.pat(pat);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Field { base, index, .. } => self.field(base.ty, *index),
            E::Match { arms, .. } => arms.iter().for_each(|a| self.pat(&a.pat)),
            _ => {}
        }
    }
}

/// `e` as written, for a callee: a variable, a field, an element or a call of one.
fn shown(cx: &Ctx, f: &FnDef, e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
        E::Field { base, index, .. } => {
            let b = shown(cx, f, base)?;
            let TyKind::Adt(d, _) = cx.ty.kind(base.ty) else {
                return None;
            };
            let field = cx.adt(*d)?.fields.get(*index as usize)?.name.clone();
            format!("{b}.{field}")
        }
        E::Index { base, index, .. } => {
            let i = match &index.kind {
                E::Lit(Lit::Int(n)) => n.to_string(),
                E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
                _ => "…".into(),
            };
            format!("{}[{i}]", shown(cx, f, base)?)
        }
        E::Call {
            callee: Callee::Def(g, _),
            args,
        } => {
            let name = match &cx.defs[g.0 as usize] {
                Some(Def::Fn(gf)) => gf.name.rsplit("::").next()?.to_string(),
                _ => return None,
            };
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{name}({dots})")
        }
        E::Call {
            callee: Callee::Indirect(inner),
            args,
        } => {
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{}({dots})", shown(cx, f, inner)?)
        }
        E::Cast(x) | E::Upcast(x) | E::UnwrapSome(x, _) => shown(cx, f, x)?,
        _ => return None,
    })
}

/// The initializer of the variable closure `c` captured as `cap`, as written (`""`, `0`):
/// found on its declaration in the function making `c` (or the one making that, for a variable
/// captured on the way). `None` when it is not a literal.
fn declaration(cx: &Ctx, g: &Graph, c: DefId, cap: LocalId) -> Option<String> {
    let (mut d, mut l) = (c, cap);
    for _ in 0..64 {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            return None;
        };
        let outer = f.captures.iter().find(|k| k.inner == l)?.outer;
        let p = *g.parent.get(&d)?;
        let Some(Def::Fn(pf)) = &cx.defs[p.0 as usize] else {
            return None;
        };
        if pf.captures.iter().any(|k| k.inner == outer) {
            (d, l) = (p, outer);
            continue;
        }
        let mut v = Decl(outer, None);
        walk::block(&pf.body.block, &mut v);
        return v.1.flatten();
    }
    None
}

/// Finds the initializer of a local's `let`, as written when it is a literal.
struct Decl(LocalId, Option<Option<String>>);

impl Visit for Decl {
    fn stmt(&mut self, s: &Stmt) {
        if let S::Let { local, init } = &s.kind {
            if *local == self.0 && self.1.is_none() {
                self.1 = Some(init.as_ref().and_then(literal));
            }
        }
    }
}

fn literal(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Lit(Lit::Str(s)) => {
            let mut out = String::from("\"");
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        E::Lit(Lit::Int(n)) => n.to_string(),
        E::Lit(Lit::Float(x)) => {
            let s = x.to_string();
            s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
        }
        E::Lit(Lit::Bool(b)) => b.to_string(),
        E::Lit(Lit::Null) => "null".into(),
        E::Unary {
            op: UnOp::Neg,
            expr,
        } => format!("-{}", literal(expr)?),
        E::ArrayLit(xs) if xs.is_empty() => "[]".into(),
        E::Cast(x) | E::WrapSome(x) | E::Upcast(x) => literal(x)?,
        _ => return None,
    })
}

/// How a variable is shared: as `shared(…)` itself (a 64-bit integer), in a `Mutex` (a value
/// that is copied) or in an object in one.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Int,
    Copy,
    Object,
}

/// The declaration sharing a variable of type `t` initialized with `init` (as written, when a
/// literal): the type is written out where the initializer alone may not give it (`null`, `[]`,
/// an object's field, an initializer that is not a literal).
fn fix(kind: Kind, t: &str, init: Option<&str>) -> String {
    let typed = matches!(init, None | Some("null" | "[]"));
    let init = init.unwrap_or("…");
    match kind {
        Kind::Int => format!("shared({init})"),
        Kind::Copy if typed => format!("shared(new Mutex<{t}>({init}))"),
        Kind::Copy => format!("shared(new Mutex({init}))"),
        Kind::Object => format!("shared(new Mutex<{{ value: {t} }}>({{ value: {init} }}))"),
    }
}

fn report(
    cx: &mut Ctx,
    name: &str,
    ty: TyId,
    at: Span,
    call: Option<(String, Span)>,
    init: Option<String>,
    why: Why,
) {
    let mut d = match &call {
        Some((callee, span)) => Diagnostic::error(
            format!("this handler calls `{callee}`, which changes `{name}`; requests run at the same time"),
            *span,
        )
        .with_label(at, format!("`{name}` is changed here")),
        None => Diagnostic::error(
            format!("an HTTP handler can call this function, which changes `{name}`; requests run at the same time"),
            at,
        )
        .with_label(why.span, "the handler is passed to `serve` here"),
    };
    // `shared` alone holds a 64-bit integer (`add`, `get`, `set`); any other value goes in a
    // `Mutex`, and one that is not copied (a string, an object) in an object there, since a
    // `with` callback can replace the fields of what it gets but not the value itself.
    let kind = if matches!(
        cx.ty.kind(ty),
        TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize)
    ) {
        Kind::Int
    } else if cx.is_copy(ty) {
        Kind::Copy
    } else {
        Kind::Object
    };
    let decl = fix(kind, &cx.display(ty), init.as_deref());
    let how = match kind {
        Kind::Int => format!("then read it with `{name}.get()` and change it with `{name}.set(v)` or `{name}.add(1)`"),
        Kind::Copy => format!("then read it with `{name}.with((v) => v)` and change it with `{name}.with((v) => {{ v = … }})`"),
        Kind::Object => format!("then read it with `{name}.with((v) => v.value)` and change it with `{name}.with((v) => {{ v.value = … }})`"),
    };
    d = d
        .with_note(format!(
            "fix: const {name} = {decl}  // one value shared by every request, as in Node"
        ))
        .with_note(how);
    cx.error(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    fn e(kind: E) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::new(FileId(0), 0, 0),
        }
    }

    #[test]
    fn literal_initializers_are_shown_as_written() {
        let s = |x: &str| literal(&e(E::Lit(Lit::Str(x.into()))));
        assert_eq!(s("").as_deref(), Some("\"\""));
        assert_eq!(s("a\"b\\").as_deref(), Some("\"a\\\"b\\\\\""));
        assert_eq!(literal(&e(E::Lit(Lit::Int(7)))).as_deref(), Some("7"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(1.5)))).as_deref(), Some("1.5"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(2.0)))).as_deref(), Some("2"));
        let neg = E::Unary {
            op: UnOp::Neg,
            expr: Box::new(e(E::Lit(Lit::Int(3)))),
        };
        assert_eq!(literal(&e(neg)).as_deref(), Some("-3"));
        assert_eq!(literal(&e(E::ArrayLit(vec![]))).as_deref(), Some("[]"));
        let call = E::ArrayLit(vec![e(E::Lit(Lit::Int(1)))]);
        assert_eq!(literal(&e(call)), None);
        assert_eq!(literal(&e(E::Lit(Lit::Null))).as_deref(), Some("null"));
    }

    #[test]
    fn fix_writes_the_type_where_the_initializer_does_not_give_it() {
        assert_eq!(fix(Kind::Int, "i64", Some("0")), "shared(0)");
        assert_eq!(fix(Kind::Copy, "number", Some("0")), "shared(new Mutex(0))");
        assert_eq!(
            fix(Kind::Copy, "number", None),
            "shared(new Mutex<number>(…))"
        );
        assert_eq!(
            fix(Kind::Object, "string", Some("\"\"")),
            "shared(new Mutex<{ value: string }>({ value: \"\" }))"
        );
        assert_eq!(
            fix(Kind::Object, "string | null", Some("null")),
            "shared(new Mutex<{ value: string | null }>({ value: null }))"
        );
        assert_eq!(
            fix(Kind::Object, "string[]", Some("[]")),
            "shared(new Mutex<{ value: string[] }>({ value: [] }))"
        );
    }
}
