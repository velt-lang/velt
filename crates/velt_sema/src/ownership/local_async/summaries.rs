//! A finished unit's facts for the local async analysis (`crate::units`): what a later unit's
//! graph takes in place of the unit's bodies ([`AsyncSummaries`]).

use rustc_hash::FxHashMap;

use super::graph::{self, Node};
use super::{Propagation, Why, CROSSES};
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, LocalId};

/// A parameter's summary: the parameter, its flags, why it got them, and why it
/// is shared (`Propagation::why`, `Propagation::shared`).
type ParamFacts = (LocalId, u8, Option<Why>, Option<Why>);

/// A finished unit's facts for this analysis (`Ctx::async_summaries`, `crate::units`), what a
/// later unit's graph takes in place of the unit's bodies: per function, each parameter's flags
/// and reasons once the unit's flow settled (a value passed there crosses, is kept in the heap,
/// ...), and the parameters that flow to what it returns.
#[derive(Clone, Default)]
pub(crate) struct AsyncSummaries {
    params: FxHashMap<DefId, Vec<ParamFacts>>,
    rets: FxHashMap<DefId, Vec<LocalId>>,
}

/// The finished unit's functions the unit calls: their parameters flow to what they return.
pub(super) fn returns(cx: &Ctx, g: &mut graph::Graph) {
    for (f, ls) in &cx.async_summaries.rets {
        let Some(&r) = g.ids.get(&Node::Ret(*f)) else {
            continue;
        };
        for l in ls {
            if let Some(&n) = g.ids.get(&Node::Local(*f, *l)) {
                g.srcs[r].push((n, None));
            }
        }
    }
}

/// ... and their parameters cross, or are kept, as the finished unit found.
pub(super) fn seed(cx: &Ctx, g: &graph::Graph, p: &mut Propagation) {
    for (f, ps) in &cx.async_summaries.params {
        for (l, flags, why, shared) in ps {
            let Some(&n) = g.ids.get(&Node::Local(*f, *l)) else {
                continue;
            };
            if let Some(w) = shared {
                p.set(n, CROSSES, Some(*w));
            }
            for bit in (0..8).map(|b| 1u8 << b).filter(|b| flags & b != 0) {
                p.set(n, bit, *why);
            }
        }
    }
}

/// This unit's [`AsyncSummaries`], from its settled graph.
pub(super) fn of(cx: &Ctx, g: &graph::Graph, p: &Propagation) -> AsyncSummaries {
    let mut out = AsyncSummaries::default();
    let params = |d: DefId| match &cx.defs[d.0 as usize] {
        Some(Def::Fn(f)) => f.params.iter().map(|p| p.local).collect(),
        _ => vec![],
    };
    let mut of: FxHashMap<DefId, Vec<LocalId>> = FxHashMap::default();
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Local(d, l) = *node else { continue };
        let ps = of.entry(d).or_insert_with(|| params(d));
        if ps.contains(&l) && (p.flags[n] != 0 || p.shared[n].is_some()) {
            let at = (l, p.flags[n], p.why[n], p.shared[n]);
            out.params.entry(d).or_default().push(at);
        }
    }
    // What flows back to each function's return, without leaving the function.
    for (r, node) in g.nodes.iter().enumerate() {
        let Node::Ret(d) = *node else { continue };
        let ps = of.entry(d).or_insert_with(|| params(d)).clone();
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![r];
        let mut back = vec![];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            match g.nodes[n] {
                Node::Local(e, l) if e == d => {
                    if ps.contains(&l) && !back.contains(&l) {
                        back.push(l);
                    }
                }
                Node::Local(..) | Node::Ret(_) | Node::Lit(_) if n != r => continue,
                _ => {}
            }
            stack.extend(g.srcs[n].iter().map(|(m, _)| *m));
        }
        if !back.is_empty() {
            out.rets.insert(d, back);
        }
    }
    out
}
