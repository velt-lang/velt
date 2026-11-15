//! Calls through function values whose callee holds only values the graph traces (#2952): the
//! closures such a call may reach are exactly the literals whose values flow into its callee, so
//! each argument flows into the matching parameter of each of them, as a direct call's do. A
//! closure every call of which is such a call then has parameters the graph traces too, which
//! may make more callees traced: repeated until nothing changes.
//!
//! The settler `new Promise` hands its executor is the motivating case: std calls
//! `executor(resolve, reject)` through the constructor's parameter, which only the program's
//! executor literals feed, so `resolve` is followed into the program instead of being taken as
//! any closure of its type at every call whose callee the graph can't trace.

use rustc_hash::FxHashSet;

use crate::ctx::Ctx;
use crate::hir::Def;

use super::{Graph, Node, STORED};

/// Wires the calls through function values whose callee is traced (`Graph::wired`), and leaves
/// `Graph::untraced` computed for the wired graph.
pub(super) fn wire(cx: &Ctx, g: &mut Graph) {
    g.wired = vec![false; g.indirect.len()];
    // A closure's parameters are of unknown origin until every call that may reach it is wired:
    // having sources isn't enough, a capture assigned in a closure flows back into the variable
    // it captures (a parameter of the enclosing closure, `super::closure`).
    g.forced = vec![false; g.nodes.len()];
    for (i, node) in g.nodes.iter().enumerate() {
        let Node::Local(c, l) = *node else { continue };
        let closure = cx
            .try_fn(c)
            .is_some_and(|fi| fi.kind == crate::defs::FnKind::Closure);
        if closure && declared_param(cx, c, l) {
            g.forced[i] = true;
        }
    }
    // A capture local's forward edge runs back to the variable it captures (a closure assigning
    // it changes the enclosing function's): that variable's values may come from the capture.
    let mut caps_into: Vec<Vec<usize>> = vec![vec![]; g.nodes.len()];
    for caps in g.captures.values() {
        for &i in caps {
            for &(o, _) in &g.srcs[i] {
                caps_into[o].push(i);
            }
        }
    }
    loop {
        g.untraced = super::dynamic::untraced_nodes(cx, g);
        let new: Vec<usize> = (0..g.indirect.len())
            .filter(|&ci| !g.wired[ci] && !g.untraced[g.indirect[ci].callee])
            .collect();
        if new.is_empty() {
            break;
        }
        for ci in new {
            connect(cx, g, &mut caps_into, ci);
        }
        mark_forced(cx, g);
    }
    // A closure taken as wired in one round may reach a call no round wired once more arguments
    // are followed, which leaves a call wired whose callee is no longer traced: such a call goes
    // back to what any call through a function value is (its arguments stored, its closures
    // matched by type), keeping the edges, until none is left.
    loop {
        let bad: Vec<usize> = (0..g.indirect.len())
            .filter(|&ci| g.wired[ci] && g.untraced[g.indirect[ci].callee])
            .collect();
        if bad.is_empty() {
            break;
        }
        for ci in bad {
            g.wired[ci] = false;
            for k in 0..g.indirect[ci].args.len() {
                let a = g.indirect[ci].args[k];
                if g.indirect[ci].only.is_none() {
                    g.seeds.push((a, STORED, None));
                }
            }
        }
        mark_forced(cx, g);
        g.untraced = super::dynamic::untraced_nodes(cx, g);
    }
}

/// Wires call `ci`: each argument flows into the matching parameter of each closure literal
/// whose value flows into its callee, and isn't taken as stored any more.
fn connect(cx: &Ctx, g: &mut Graph, caps_into: &mut Vec<Vec<usize>>, ci: usize) {
    g.wired[ci] = true;
    let lits = lits_into(g, caps_into, g.indirect[ci].callee);
    let (args, span, library) = {
        let call = &g.indirect[ci];
        (call.args.clone(), call.span, call.library)
    };
    for c in lits {
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        let declared: Vec<_> = (f.params.iter().skip(f.captures.len()))
            .map(|p| (p.local, p.ty))
            .collect();
        for (k, &a) in args.iter().enumerate() {
            let Some(&(local, ty)) = declared.get(k) else {
                break;
            };
            let p = g.node(Node::Local(c, local), ty);
            if g.forced.len() < g.nodes.len() {
                g.forced.resize(g.nodes.len(), false);
                caps_into.resize(g.nodes.len(), vec![]);
            }
            g.srcs[p].push((a, Some((span, library))));
        }
    }
    // What the callee keeps, the graph now sees it keep: no longer taken as stored.
    let args: FxHashSet<usize> = args.into_iter().collect();
    g.seeds
        .retain(|&(n, f, _)| f != STORED || !args.contains(&n));
}

/// A closure that a call the graph doesn't wire may reach has parameters of unknown origin,
/// whatever the wired calls pass it (`Graph::forced`).
fn mark_forced(cx: &Ctx, g: &mut Graph) {
    let fwd = super::super::call_sites::forward(cx, g);
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        let wired = super::super::call_sites::held_in_locals(g, &fwd, n)
            .is_some_and(|seen| seen.iter().all(|&m| !unwired_callee(g, m)));
        for p in f.params.iter().skip(f.captures.len()) {
            if let Some(&i) = g.ids.get(&Node::Local(c, p.local)) {
                g.forced[i] = !wired;
            }
        }
    }
}

/// Is local `l` one of closure `c`'s declared parameters (after its captures)?
fn declared_param(cx: &Ctx, c: crate::hir::DefId, l: crate::hir::LocalId) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return false;
    };
    f.params.iter().skip(f.captures.len()).any(|p| p.local == l)
}

/// Is node `m` the callee of a call through a function value the graph doesn't wire?
fn unwired_callee(g: &Graph, m: usize) -> bool {
    matches!(g.nodes[m], Node::Callee(ci) if !g.wired[ci as usize])
}

/// The closure literals whose values flow into node `n`: along the edges backward, and from a
/// variable to the capture locals that assign it.
fn lits_into(g: &Graph, caps_into: &[Vec<usize>], n: usize) -> Vec<crate::hir::DefId> {
    let mut seen = FxHashSet::default();
    let mut work = vec![n];
    let mut out = vec![];
    while let Some(m) = work.pop() {
        if !seen.insert(m) {
            continue;
        }
        if let Node::Lit(c) = g.nodes[m] {
            out.push(c);
        }
        work.extend(g.srcs[m].iter().map(|&(s, _)| s));
        work.extend(caps_into[m].iter().copied());
    }
    out.sort_by_key(|c| c.0);
    out
}
