//! A finished unit's facts for the local async analysis (`crate::units`): what a later unit's
//! graph takes in place of the unit's bodies ([`AsyncSummaries`]).

use rustc_hash::{FxHashMap, FxHashSet};

use super::graph::{self, Node};
use super::{Propagation, Why, CROSSES};
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, LocalId, TyId};

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
    /// The unit's crossing values of a generic type (`Propagation::hits`): what they carry
    /// depends on the instantiations, which a later unit adds to, so its check resolves them
    /// again with its own (`instances`), through the unit's edges below.
    hits: Vec<(DefId, TyId, Why)>,
    insts: FxHashMap<DefId, Vec<(DefId, Vec<TyId>)>>,
    parent: FxHashMap<DefId, DefId>,
    news: FxHashMap<DefId, Vec<(DefId, Vec<TyId>)>>,
    dispatched: Vec<DefId>,
    referenced: FxHashSet<DefId>,
    /// The unit's generic closures and methods called through an interface
    /// (`generic_closures::DynCalls::sites`): a later unit's check does not follow those calls,
    /// so their type parameters stay generic there (any closure).
    pub(super) dyn_owners: FxHashSet<DefId>,
    /// [`crossing_closure_params`], found once as the unit's stage ends.
    pub(super) crossing: Vec<(DefId, usize, Option<Why>)>,
}

/// The finished unit's functions the unit calls: their parameters flow to what they return.
pub(super) fn returns(cx: &Ctx, g: &mut graph::Graph) {
    for (f, ls) in &cx.unit_sums.async_summaries.rets {
        if cx.walks(*f) {
            continue;
        }
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
    for (f, ps) in &cx.unit_sums.async_summaries.params {
        // With the std code the program reaches walked ([`Ctx::walk_std`]), the rest of the
        // unit's functions are code the program doesn't have: their facts don't count.
        if cx.walks(*f) || cx.walk_std.is_some() {
            continue;
        }
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

/// The finished unit's edges for resolving its crossing values of a generic type, added to the
/// unit's graph.
pub(super) fn merge(cx: &Ctx, g: &mut graph::Graph) {
    let s = &cx.unit_sums.async_summaries;
    for (f, sites) in &s.insts {
        g.insts.entry(*f).or_default().extend(sites.iter().cloned());
    }
    for (f, sites) in &s.news {
        g.news.entry(*f).or_default().extend(sites.iter().cloned());
    }
    for (c, p) in &s.parent {
        g.parent.entry(*c).or_insert(*p);
    }
    g.dispatched.extend(s.dispatched.iter().copied());
    g.dispatched.sort_by_key(|d| d.0);
    g.dispatched.dedup();
    g.held.referenced.extend(s.referenced.iter().copied());
    // What a finished unit's function returns may come from its heap (`Map.get` gives what
    // `Map.set` stored), which its summary does not follow: a value of unknown origin. And a
    // value passed to one of its parameters goes where this graph does not follow it on: it
    // does not stay in locals (`call_sites::held_in_locals`).
    for n in 0..g.nodes.len() {
        match g.nodes[n] {
            Node::Ret(f) if !cx.walks(f) => {
                g.unknown[n] = true;
                let ty = g.tys[n];
                if !g.inflows[n].contains(&(ty, None)) {
                    g.inflows[n].push((ty, None));
                }
            }
            Node::Local(f, _) if !cx.walks(f) => g.held.escapes.push(n),
            _ => {}
        }
    }
}

/// The finished unit's closures with a parameter that crosses, by the parameter's position
/// among the declared ones, with why: their literals are in the finished unit's bodies, which
/// this unit's graph does not walk (`call_sites::crossing_params`).
pub(super) fn crossing_closure_params<'c>(cx: &'c Ctx<'_>) -> &'c [(DefId, usize, Option<Why>)] {
    // With the std code the program reaches walked ([`Ctx::walk_std`]), its closures' params
    // are found again; the rest of the unit's are code the program doesn't have.
    if cx.walk_std.is_some() {
        return &[];
    }
    &cx.unit_sums.async_summaries.crossing
}

/// [`crossing_closure_params`] of the unit whose summaries were just made.
pub(super) fn find_crossing_closure_params(cx: &Ctx) -> Vec<(DefId, usize, Option<Why>)> {
    let mut out = vec![];
    for (f, ps) in &cx.unit_sums.async_summaries.params {
        if cx
            .try_fn(*f)
            .is_none_or(|i| i.kind != crate::defs::FnKind::Closure)
        {
            continue;
        }
        let Some(Def::Fn(fd)) = &cx.defs[f.0 as usize] else {
            continue;
        };
        let declared = &fd.params[fd.captures.len().min(fd.params.len())..];
        for (l, flags, why, _) in ps {
            if flags & CROSSES == 0 {
                continue;
            }
            if let Some(k) = declared.iter().position(|p| p.local == *l) {
                out.push((*f, k, *why));
            }
        }
    }
    out.sort_by_key(|(f, k, _)| (f.0, *k));
    out
}

/// The finished unit's crossing values of a generic type, resolved again with this unit's
/// instantiations (`resolve_hits`).
pub(super) fn std_hits(cx: &Ctx, p: &mut Propagation) {
    // With the std code the program reaches walked ([`Ctx::walk_std`]), its hits are found
    // again; the rest of the unit's are code the program doesn't have.
    if cx.walk_std.is_some() {
        return;
    }
    p.hits
        .extend(cx.unit_sums.async_summaries.hits.iter().copied());
}

/// This unit's [`AsyncSummaries`], from its settled graph.
pub(super) fn of(cx: &Ctx, g: &graph::Graph, p: &Propagation) -> AsyncSummaries {
    let mut out = AsyncSummaries {
        hits: p.hits.clone(),
        insts: g.insts.clone(),
        parent: g.parent.clone(),
        news: g.news.clone(),
        dispatched: g.dispatched.clone(),
        referenced: g.held.referenced.clone(),
        dyn_owners: (cx.fn_defs.iter().copied())
            .filter(|&d| g.dyn_calls.sites(cx, g, d).is_some())
            .collect(),
        ..AsyncSummaries::default()
    };
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

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    AsyncSummaries {
        params, rets, hits, insts, parent, news, dispatched, referenced, dyn_owners, crossing,
    }
}
