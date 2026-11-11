//! After the walk (`super::graph::build`): the methods a dispatched call may reach, and the locals
//! of a generic type that cross with their function's instantiations (`Graph::wild`). A function
//! called where the callee is unknown (a closure literal, a function value, a dispatched method)
//! is followed per call site instead (`super::crossing_params`, #2756): its parameters don't
//! make their types crossing for the whole program.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, DefId, Expr};

use super::graph::{Graph, Node};

/// Mark what the walk can't see the origin of in `g` (module docs).
pub(super) fn mark(cx: &Ctx, g: &mut Graph) {
    // The methods a virtual, interface or bounded call may reach: those in a class's vtable
    // (only methods overridden somewhere have a slot) and an interface's implementations. Their
    // instantiations aren't all shown by their direct calls (`super::resolve_hits`).
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
    // A local whose type mentions a type parameter (`T`, `T[]`, `Box<T>`, a `T` read out of one)
    // carries what that type is in its function's instantiations. std's generic callbacks (the
    // prelude's `resolve`) are matched by their instantiations instead (`super::crossing_params`).
    // A generic function's result (`pick(gs)` in `spawn(pick(gs)())`) likewise.
    for (n, node) in g.nodes.iter().enumerate() {
        let (Node::Local(f, _) | Node::Ret(f)) = *node else {
            continue;
        };
        let std = cx.try_fn(f).is_some_and(|i| cx.scopes[i.module].is_std);
        if !std && super::types::mentions_param(cx, g.tys[n]) {
            g.wild.push(n);
            // Fed by std (`m.get(k)` on a `Map<string, T>`, a std iterator's values): the flow goes
            // into std's own nodes, where the program's type parameters aren't resolved, so this
            // local is resolved through its function's instantiations like one of unknown origin.
            if g.srcs[n].iter().any(|&(m, _)| std_node(cx, g, m)) {
                g.std_fed.push(n);
            }
        }
    }
}

/// The methods a virtual, interface or bounded call may reach: an interface method's
/// implementations (each `ImplDef` lists them in the interface's order), or the vtable slot of
/// the receiver's class and of every class extending it. Empty when the receiver isn't one of
/// those (`super::crossing_params` then takes every dispatched method).
pub(super) fn dispatch_targets(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Vec<DefId> {
    let impl_methods = |iface: DefId, slot: u32| -> Vec<DefId> {
        (cx.impls.iter())
            .filter(|i| i.iface == iface)
            .filter_map(|i| i.methods.get(slot as usize).copied())
            .collect()
    };
    let receiver = args.first().map(|a| cx.ty.kind(a.ty).clone());
    match (callee, receiver) {
        (Callee::Dyn { slot, .. }, Some(crate::hir::TyKind::Dyn(iface, _))) => {
            impl_methods(iface, *slot)
        }
        (Callee::ParamMethod { iface, slot, .. }, _) => impl_methods(*iface, *slot),
        (Callee::Virtual { slot }, Some(crate::hir::TyKind::Adt(class, _))) => {
            let mut classes = vec![class];
            classes.extend(super::types::subclasses(cx, class));
            (classes.iter())
                .filter_map(|c| match &cx.info[c.0 as usize] {
                    DefInfo::Adt(a) => a.vtable.get(*slot as usize).copied(),
                    _ => None,
                })
                .collect()
        }
        _ => vec![],
    }
}

/// Is node `m` a local or the result of a function in std?
pub(super) fn std_node(cx: &Ctx, g: &Graph, m: usize) -> bool {
    match g.nodes[m] {
        Node::Local(d, _) | Node::Ret(d) => {
            cx.try_fn(d).is_some_and(|i| cx.scopes[i.module].is_std)
        }
        _ => false,
    }
}
