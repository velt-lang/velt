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

mod bodies;
mod held;
mod message;

use bodies::Bodies;
use held::{held_closures, root};
use message::{call_site, declaration, report};

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
    // Which stored closures of the program are held by one local, and what its bodies do with
    // fields: only needed when it stores a closure at all.
    let user =
        g.nodes.iter().enumerate().any(
            |(n, node)| matches!(node, Node::Lit(c) if flags[n] & STORED != 0 && !in_std(cx, *c)),
        );
    let (held, homes) = if user {
        held_closures(cx, g, flags)
    } else {
        Default::default()
    };
    let mut bodies = if user {
        Bodies::new(cx)
    } else {
        Bodies::default()
    };
    bodies.homes = homes;
    // Nodes whose values flow somewhere the graph follows.
    let flowing: HashSet<usize> = if user {
        g.srcs.iter().flatten().map(|(n, _)| *n).collect()
    } else {
        HashSet::new()
    };
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
        let reads = if !user {
            None
        } else {
            bodies.request_reads(g.nodes.iter().enumerate().filter_map(|(n, node)| {
                match (node, why[n]) {
                    (Node::Lit(c), Some(_)) => Some(*c),
                    _ => None,
                }
            }))
        };
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
