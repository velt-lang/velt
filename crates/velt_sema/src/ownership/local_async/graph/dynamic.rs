//! Conversions into a dynamic type in the flow graph ([`Node::Dyn`]): a value made `any`,
//! `unknown`, `object` or `Function`, or thrown, which a crossing value of a dynamic type may
//! hold (#2865).

use velt_common::Span;

use crate::ctx::Ctx;

use crate::hir::{Callee, DefId, Expr, ExprKind as E, TyId};

use super::{Graph, Node, To, Walk};

/// A conversion into a dynamic type (`Graph::dynamic`).
#[derive(Clone, Copy)]
pub(in crate::ownership::local_async) struct DynConv {
    /// Its [`Node::Dyn`].
    pub node: usize,
    /// The conversion, and the dynamic type it makes (none: a `throw`, caught as `unknown`).
    pub span: Span,
    pub ty: Option<TyId>,
    /// The function it is in, and the type of the value converted: what flows in where the graph
    /// has no source is resolved by that type, in the function's instantiations.
    pub owner: DefId,
    pub from: TyId,
    /// Something flows in that the graph can't trace (a value of unknown origin, a parameter only
    /// dispatched calls feed, a generic value): set once the graph is built.
    pub untraced: bool,
}

/// Does something flow into node `n` that the graph can't trace ([`untraced_nodes`])?
pub(in crate::ownership::local_async) fn untraced_input(g: &Graph, n: usize) -> bool {
    g.untraced[n]
}

/// Per node: does something flow into it that the graph can't trace? Once for the whole graph:
/// a value of unknown origin, a node nothing feeds but a closure literal (a parameter only calls
/// through function values feed, `(v) => v as unknown` that std calls), a parameter of a named
/// function used as a value (calls through it feed it, with no edge), and whatever these flow
/// into: along the edges, and from a closure's capture back to the variable it assigns through it.
pub(super) fn untraced_nodes(cx: &Ctx, g: &Graph) -> Vec<bool> {
    let n = g.nodes.len();
    let mut out = vec![false; n];
    let mut work = vec![];
    for (i, node) in g.nodes.iter().enumerate() {
        let seed = g.unknown[i]
            || (g.srcs[i].is_empty() && !matches!(node, Node::Lit(_)))
            || matches!(*node, Node::Local(d, l) if g.held.referenced.contains(&d) && is_param(cx, d, l));
        if seed {
            out[i] = true;
            work.push(i);
        }
    }
    // The forward edges, flat (each node's targets at `next[start[m]..start[m + 1]]`): along
    // `srcs`, and from a capture local back to the variable it holds.
    let back = |i: usize| g.srcs[i].iter().map(|&(o, _)| o);
    let mut start = vec![0usize; n + 1];
    for srcs in &g.srcs {
        for &(s, _) in srcs {
            start[s + 1] += 1;
        }
    }
    for caps in g.captures.values() {
        for &i in caps {
            start[i + 1] += g.srcs[i].len();
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut next = vec![0u32; start[n]];
    for (m, srcs) in g.srcs.iter().enumerate() {
        for &(s, _) in srcs {
            next[fill[s]] = m as u32;
            fill[s] += 1;
        }
    }
    for caps in g.captures.values() {
        for &i in caps {
            for o in back(i) {
                next[fill[i]] = o as u32;
                fill[i] += 1;
            }
        }
    }
    while let Some(m) = work.pop() {
        for &t in &next[start[m]..start[m + 1]] {
            let t = t as usize;
            if !out[t] {
                out[t] = true;
                work.push(t);
            }
        }
    }
    out
}

/// Is local `l` a declared parameter of function `d`?
fn is_param(cx: &Ctx, d: DefId, l: crate::hir::LocalId) -> bool {
    matches!(&cx.defs[d.0 as usize], Some(crate::hir::Def::Fn(f)) if f.params.iter().any(|p| p.local == l))
}

impl Walk<'_, '_> {
    /// `x` made a dynamic value at `span` (of type `ty`; none: thrown), going to `to`: through a
    /// [`Node::Dyn`], which a crossing value of a dynamic type makes cross (`Graph::dynamic`).
    pub(super) fn made_dynamic(&mut self, x: &Expr, span: Span, ty: Option<TyId>, to: To) {
        let k = self.g.dynamic.len() as u32;
        let n = self.g.node(Node::Dyn(k), x.ty);
        // The call through a function value that gives the value, if it does (awaited or not):
        // its callee is numbered next.
        let mut inner = x;
        while let E::Await(v) = &inner.kind {
            inner = v;
        }
        let call = matches!(
            &inner.kind,
            E::Call {
                callee: Callee::Indirect(_),
                ..
            }
        )
        .then_some(self.g.indirect.len() as u32);
        // Or a local such a call gave its value (`const v = await f(); v as unknown`).
        let local = match &inner.kind {
            E::Local(l, _) => Some(self.local(*l)),
            _ => None,
        };
        self.g.dynamic.push(DynConv {
            node: n,
            span,
            ty,
            owner: self.d,
            from: x.ty,
            untraced: false,
        });
        self.value(x, To::Node(n, None));
        let callee = (call.and_then(|c| self.g.ids.get(&Node::Callee(c))))
            .or_else(|| local.and_then(|l| self.g.call_results.get(&l)))
            .copied();
        if let Some(callee) = callee {
            self.g.dyn_callee.insert(n, callee);
        }
        self.connect(n, to);
    }
}
