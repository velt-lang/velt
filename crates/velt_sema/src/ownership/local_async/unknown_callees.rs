//! After the walk (`super::graph::build`): the functions called where the callee is unknown, whose
//! parameters hold values of unknown origin, and the locals of a generic type that cross with
//! their function's instantiations (`Graph::wild`).

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Def, DefId};

use super::graph::{Graph, Node};

/// Mark what the walk can't see the origin of in `g` (module docs).
pub(super) fn mark(cx: &Ctx, g: &mut Graph) {
    // Closure literals, named functions used as values and dispatched methods (an interface's
    // implementations, overriding and overridden methods, and methods called through a generic
    // bound) are called where the callee is unknown, whose arguments the graph doesn't follow into
    // their parameters: those hold values of unknown origin, of their declared types. One that
    // crosses (a callback that spawns its argument) makes its type a crossing type, so the closures
    // passed to such calls cross by the heap rule. A function referenced in several places is
    // listed once.
    let mut called: Vec<DefId> = g.parent.keys().copied().collect();
    called.extend(g.fn_values.iter().copied());
    // The methods a virtual, interface or bounded call may reach: those in a class's vtable
    // (only methods overridden somewhere have a slot) and an interface's implementations. Any
    // other method is called directly, where the graph follows its arguments.
    for info in &cx.info {
        if let DefInfo::Adt(a) = info {
            g.dispatched.extend(a.vtable.iter().copied());
        }
    }
    for i in &cx.impls {
        g.dispatched.extend(i.methods.iter().copied());
    }
    g.dispatched.sort_by_key(|d| d.0);
    g.dispatched.dedup();
    called.extend(g.dispatched.iter().copied());
    called.sort_by_key(|d| d.0);
    called.dedup();
    for c in called {
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        // A method's first parameter is `this`, the receiver the graph doesn't track.
        let this = usize::from(f.self_ty.is_some());
        for p in f.params.iter().skip(f.captures.len() + this) {
            if let Some(&n) = g.ids.get(&Node::Local(c, p.local)) {
                g.unknown[n] = true;
            }
        }
    }
    // A local whose type mentions a type parameter (`T`, `T[]`, `Box<T>`, a `T` read out of one)
    // carries what that type is in its function's instantiations. std's generic callbacks (the
    // prelude's `resolve`) are matched by their instantiations instead (`super::crossing_params`).
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Local(f, _) = *node else { continue };
        let std = cx.try_fn(f).is_some_and(|i| cx.scopes[i.module].is_std);
        if !std && super::types::mentions_param(cx, g.tys[n]) {
            g.wild.push(n);
        }
    }
}
