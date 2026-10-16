//! Graph helpers for the uses between a block's nested functions (`super::Rw`).

use std::collections::{HashMap, HashSet};

/// A path from `a` back to itself through `edges` (the nodes after `a`, empty for a direct
/// self-use), if there is one.
pub(super) fn cycle(a: usize, edges: &HashMap<usize, Vec<usize>>) -> Option<Vec<usize>> {
    if edges[&a].contains(&a) {
        return Some(vec![]);
    }
    // Breadth-first, remembering where each node was reached from.
    let mut from: HashMap<usize, usize> = HashMap::new();
    let mut queue: std::collections::VecDeque<usize> = edges[&a].iter().copied().collect();
    for &b in &edges[&a] {
        from.insert(b, a);
    }
    while let Some(b) = queue.pop_front() {
        for &c in &edges[&b] {
            if c == a {
                let mut path = vec![b];
                let mut at = b;
                while from[&at] != a {
                    at = from[&at];
                    path.push(at);
                }
                path.reverse();
                return Some(path);
            }
            if let std::collections::hash_map::Entry::Vacant(v) = from.entry(c) {
                v.insert(b);
                queue.push_back(c);
            }
        }
    }
    None
}

/// The strongly connected components of `nodes` under `edges` (Tarjan), each in no particular
/// order, the components in reverse topological order.
pub(super) fn sccs(nodes: &[usize], edges: &HashMap<usize, Vec<usize>>) -> Vec<Vec<usize>> {
    struct T<'a> {
        edges: &'a HashMap<usize, Vec<usize>>,
        index: HashMap<usize, usize>,
        low: HashMap<usize, usize>,
        stack: Vec<usize>,
        on: HashSet<usize>,
        out: Vec<Vec<usize>>,
    }
    fn visit(t: &mut T, v: usize) {
        let i = t.index.len();
        t.index.insert(v, i);
        t.low.insert(v, i);
        t.stack.push(v);
        t.on.insert(v);
        for &w in &t.edges[&v] {
            if !t.index.contains_key(&w) {
                visit(t, w);
                let l = t.low[&v].min(t.low[&w]);
                t.low.insert(v, l);
            } else if t.on.contains(&w) {
                let l = t.low[&v].min(t.index[&w]);
                t.low.insert(v, l);
            }
        }
        if t.low[&v] == t.index[&v] {
            let mut c = vec![];
            while let Some(w) = t.stack.pop() {
                t.on.remove(&w);
                c.push(w);
                if w == v {
                    break;
                }
            }
            c.sort_unstable();
            t.out.push(c);
        }
    }
    let mut t = T {
        edges,
        index: HashMap::new(),
        low: HashMap::new(),
        stack: vec![],
        on: HashSet::new(),
        out: vec![],
    };
    for &v in nodes {
        if !t.index.contains_key(&v) {
            visit(&mut t, v);
        }
    }
    t.out
}

/// `nodes` ordered so that a node comes before every node it uses (`edges`, acyclic among
/// `nodes`), keeping source order otherwise.
pub(super) fn topo(nodes: &[usize], edges: &HashMap<usize, Vec<usize>>) -> Vec<usize> {
    let mut users: HashMap<usize, usize> = nodes.iter().map(|&a| (a, 0)).collect();
    for &a in nodes {
        for b in &edges[&a] {
            if let Some(k) = users.get_mut(b) {
                *k += 1;
            }
        }
    }
    let mut out = vec![];
    let mut done = HashSet::new();
    while out.len() < nodes.len() {
        let Some(&a) = nodes.iter().find(|a| !done.contains(*a) && users[*a] == 0) else {
            // Only cycles are left (cannot happen: they were kept as declarations).
            out.extend(nodes.iter().filter(|a| !done.contains(*a)));
            break;
        };
        done.insert(a);
        out.push(a);
        for b in &edges[&a] {
            if let Some(k) = users.get_mut(b) {
                *k -= 1;
            }
        }
    }
    out
}
