//! Strongly connected components of a directed graph (Tarjan), shared by the call graph
//! (bottom-up inlining order) and the per-function CFG analyses (which blocks are in loops).

/// Components of the graph with nodes `0..edges.len()` and adjacency lists `edges`, in
/// Tarjan's order: a component comes before every component that has an edge into it. Each
/// component is sorted. Computed iteratively, so deep graphs cannot overflow the stack.
pub(crate) fn sccs(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    Tarjan::new(edges).run()
}

/// Per node: whether it lies on a cycle (a component with several nodes, or a self-edge).
pub(crate) fn on_cycle(edges: &[Vec<usize>]) -> Vec<bool> {
    let mut out = vec![false; edges.len()];
    for scc in sccs(edges) {
        let cyclic = scc.len() > 1 || edges[scc[0]].contains(&scc[0]);
        for v in scc {
            out[v] = cyclic;
        }
    }
    out
}

struct Tarjan<'a> {
    edges: &'a [Vec<usize>],
    index: Vec<Option<u32>>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    next: u32,
    out: Vec<Vec<usize>>,
}

impl<'a> Tarjan<'a> {
    fn new(edges: &'a [Vec<usize>]) -> Self {
        let n = edges.len();
        Tarjan {
            edges,
            index: vec![None; n],
            low: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            next: 0,
            out: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Vec<usize>> {
        for root in 0..self.edges.len() {
            if self.index[root].is_none() {
                self.visit(root);
            }
        }
        self.out
    }

    fn open(&mut self, v: usize) {
        self.index[v] = Some(self.next);
        self.low[v] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v] = true;
    }

    /// Iterative DFS: each frame is (node, index of the next edge to explore).
    fn visit(&mut self, root: usize) {
        self.open(root);
        let mut frames = vec![(root, 0usize)];
        while let Some(&mut (v, ref mut edge)) = frames.last_mut() {
            if let Some(&w) = self.edges[v].get(*edge) {
                *edge += 1;
                match self.index[w] {
                    None => {
                        self.open(w);
                        frames.push((w, 0));
                    }
                    Some(iw) if self.on_stack[w] => self.low[v] = self.low[v].min(iw),
                    Some(_) => {}
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                self.low[parent] = self.low[parent].min(self.low[v]);
            }
            if Some(self.low[v]) == self.index[v] {
                self.close(v);
            }
        }
    }

    fn close(&mut self, v: usize) {
        let mut scc = Vec::new();
        while let Some(w) = self.stack.pop() {
            self.on_stack[w] = false;
            scc.push(w);
            if w == v {
                break;
            }
        }
        scc.sort();
        self.out.push(scc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_and_self_edges() {
        // 0 → 1 ⇄ 2 → 3, 3 → 3, 4 alone.
        let edges = vec![vec![1], vec![2], vec![1, 3], vec![3], vec![]];
        assert_eq!(on_cycle(&edges), vec![false, true, true, true, false]);
        let sccs = sccs(&edges);
        let pos = |v: usize| sccs.iter().position(|s| s.contains(&v)).unwrap();
        assert!(pos(3) < pos(1) && pos(1) < pos(0));
    }
}
