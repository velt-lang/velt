//! Conversions into a dynamic type in the flow graph ([`Node::Dyn`]): a value made `any`,
//! `unknown`, `object` or `Function`, or thrown, which a crossing value of a dynamic type may
//! hold (#2865).

use velt_common::Span;

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

/// Does something flow into node `n` that the graph can't trace: does a walk back over its
/// sources reach a value of unknown origin, or a node nothing flows into but a closure literal (a
/// parameter only calls through function values feed, `(v) => v as unknown` that std calls)?
/// Bounded; a walk too long to finish counts as untraced.
pub(in crate::ownership::local_async) fn untraced_input(g: &Graph, n: usize) -> bool {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut work = vec![n];
    while let Some(m) = work.pop() {
        if !seen.insert(m) {
            continue;
        }
        if seen.len() > 64 || g.unknown[m] {
            return true;
        }
        if g.srcs[m].is_empty() && !matches!(g.nodes[m], Node::Lit(_)) {
            return true;
        }
        work.extend(g.srcs[m].iter().map(|&(s, _)| s));
    }
    false
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
