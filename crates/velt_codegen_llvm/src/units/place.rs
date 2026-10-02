//! Placement: which unit defines each function.
//!
//! Functions are first gathered into groups that should share a unit, then units are cut as
//! contiguous runs of groups of similar weight, never inside a group. Groups come from the
//! reference graph (`graph`): its strongly connected components (mutually recursive functions,
//! such as the drop glue of a recursive type: `drop TreeNode` → `objdrop TreeNode` →
//! `drop Option<TreeNode>`) are walked callers first, and a small component joins the group of
//! its first referrer in program order, so a hot call chain and the small helpers it calls stay
//! in one unit where LLVM can inline them. A group stops taking components at half a unit's
//! share of the program, so a huge `main` that calls everything does not swallow the program.
//! Large components, components no one refers to and exported functions start a group.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use velt_vir::vir::{self, Linkage};

use super::graph::{components, successors, Refs};

/// Components of at most this weight join the group of their first referrer; larger ones are
/// better spread over the units. 500 was too small: k-nucleotide's `frequencies` (525 statements
/// once `velt_opt` has inlined `Map.upsert` into it) then landed in another unit than the
/// `Map.lookup` its hot loop calls, 12 % slower.
const SMALL_COMPONENT_WEIGHT: usize = 2_000;

/// The unit of every function (`0..count`; every unit below the largest one defines at least
/// one function).
pub(super) fn owners(
    program: &vir::Program,
    weights: &[usize],
    refs: &[Refs],
    static_refs: &[Refs],
    count: usize,
) -> Vec<usize> {
    let (group, parent) = groups(program, weights, refs, static_refs, count);
    cut(weights, &group, &parent, count)
}

/// The group of every function, and the parent of every group: the group of the first referrer
/// of the component that started it. Groups are numbered callers first.
fn groups(
    program: &vir::Program,
    weights: &[usize],
    refs: &[Refs],
    static_refs: &[Refs],
    count: usize,
) -> (Vec<usize>, Vec<Option<usize>>) {
    let n = weights.len();
    let succ = successors(refs, static_refs);
    let (comp, n_comps) = components(&succ);
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); n_comps];
    for (v, &c) in comp.iter().enumerate() {
        members[c].push(v);
    }
    let preds = cross_component_predecessors(&succ, &comp);
    let total: usize = weights.iter().sum();
    let cap = total / (2 * count.max(1));
    // Program position of every node: a function's index; a static takes its first referrer's.
    let mut pos = vec![usize::MAX; succ.len()];
    let mut group = vec![usize::MAX; succ.len()];
    let mut group_weight: Vec<usize> = Vec::new();
    let mut parent: Vec<Option<usize>> = Vec::new();
    for c in callers_first(&succ, &comp, &members) {
        let nodes = &members[c];
        let weight: usize = nodes.iter().filter(|&&v| v < n).map(|&v| weights[v]).sum();
        let exported = nodes
            .iter()
            .any(|&v| v < n && program.funcs[v].linkage == Linkage::Export);
        let first = nodes
            .iter()
            .flat_map(|&v| preds[v].iter().copied())
            .min_by_key(|&p| (pos[p], p));
        let joined = first.map(|p| group[p]).filter(|&g| {
            !exported && weight <= SMALL_COMPONENT_WEIGHT && group_weight[g] + weight <= cap
        });
        let g = joined.unwrap_or_else(|| {
            group_weight.push(0);
            parent.push(first.map(|p| group[p]));
            group_weight.len() - 1
        });
        group_weight[g] += weight;
        let static_pos = nodes
            .iter()
            .copied()
            .filter(|&v| v < n)
            .min()
            .or(first.map(|p| pos[p]))
            .unwrap_or(usize::MAX);
        for &v in nodes {
            group[v] = g;
            pos[v] = if v < n { v } else { static_pos };
        }
    }
    group.truncate(n);
    (group, parent)
}

/// For every node, the nodes of other components that refer to it.
fn cross_component_predecessors(succ: &[Vec<usize>], comp: &[usize]) -> Vec<Vec<usize>> {
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); succ.len()];
    for (v, out) in succ.iter().enumerate() {
        for &w in out {
            if comp[w] != comp[v] {
                preds[w].push(v);
            }
        }
    }
    preds
}

/// The components in topological order, callers before callees (every referrer of a component
/// comes before it), and otherwise in program order (by their first function; statics last).
fn callers_first(succ: &[Vec<usize>], comp: &[usize], members: &[Vec<usize>]) -> Vec<usize> {
    let mut indegree = vec![0usize; members.len()];
    for (v, out) in succ.iter().enumerate() {
        for &w in out {
            if comp[w] != comp[v] {
                indegree[comp[w]] += 1;
            }
        }
    }
    // Members are ascending, so the first is the component's earliest node.
    let key = |c: usize| (members[c].first().copied().unwrap_or(usize::MAX), c);
    let mut ready: BinaryHeap<Reverse<(usize, usize)>> = (0..members.len())
        .filter(|&c| indegree[c] == 0)
        .map(|c| Reverse(key(c)))
        .collect();
    let mut order = Vec::with_capacity(members.len());
    while let Some(Reverse((_, c))) = ready.pop() {
        order.push(c);
        for &v in &members[c] {
            for &w in &succ[v] {
                if comp[w] != c {
                    indegree[comp[w]] -= 1;
                    if indegree[comp[w]] == 0 {
                        ready.push(Reverse(key(comp[w])));
                    }
                }
            }
        }
    }
    order
}

/// Units as contiguous runs of groups. Groups are ordered depth first: every group right after
/// its parent and the parent's earlier children (so a large callee that starts a group of its own
/// still lands next to its caller), roots and siblings callers first, in program order. A unit
/// ends once it holds its share of what is left (so one huge group does not leave the rest to a
/// single unit), or when there are only as many groups left as units still empty.
fn cut(weights: &[usize], group: &[usize], parent: &[Option<usize>], count: usize) -> Vec<usize> {
    let (mut weight, mut defines) = (vec![0usize; parent.len()], vec![false; parent.len()]);
    for (f, &g) in group.iter().enumerate() {
        weight[g] += weights[f];
        defines[g] = true;
    }
    let order: Vec<usize> = depth_first(parent)
        .into_iter()
        .filter(|&g| defines[g])
        .collect();
    let mut unit_of = vec![0usize; parent.len()];
    // What the current unit and the ones after it still have to share.
    let mut rest: usize = weights.iter().sum();
    let (mut acc, mut unit) = (0usize, 0usize);
    for (i, &g) in order.iter().enumerate() {
        unit_of[g] = unit;
        acc += weight[g];
        let groups_left = order.len() - i - 1;
        let units_left = count - unit - 1;
        if units_left > 0 && (acc * (units_left + 1) >= rest || groups_left <= units_left) {
            unit += 1;
            rest -= acc;
            acc = 0;
        }
    }
    group.iter().map(|&g| unit_of[g]).collect()
}

/// Pre-order of the forest given by `parent` (children after their parent, in id order),
/// without recursion.
fn depth_first(parent: &[Option<usize>]) -> Vec<usize> {
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); parent.len()];
    let mut stack = Vec::new();
    for (g, p) in parent.iter().enumerate().rev() {
        match p {
            Some(p) => children[*p].push(g),
            None => stack.push(g),
        }
    }
    // Children were pushed in descending id order: popping visits them ascending.
    let mut order = Vec::with_capacity(parent.len());
    while let Some(g) = stack.pop() {
        order.push(g);
        stack.extend(children[g].iter().copied());
    }
    order
}
