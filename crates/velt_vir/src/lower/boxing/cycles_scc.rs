//! Tarjan's strongly connected components without recursion, over a graph whose nodes are
//! numbered as they are found (the cycle analysis, cycles.rs): `succ(v)` is asked once per node
//! reached, and may number new nodes.

/// The components reachable from `starts` that contain a cycle: two nodes or more, or one node
/// with an edge to itself.
pub(super) fn cyclic_components(
    starts: &[usize],
    succ: &mut dyn FnMut(usize) -> Vec<usize>,
) -> Vec<Vec<usize>> {
    struct Frame {
        node: usize,
        next: usize,
    }
    let mut order: Vec<Option<usize>> = vec![];
    let mut low: Vec<usize> = vec![];
    let mut on_stack: Vec<bool> = vec![];
    let mut edges: Vec<Vec<usize>> = vec![];
    let mut stack: Vec<usize> = vec![];
    let mut out = vec![];
    let mut counter = 0;
    let grow = |n: usize, o: &mut Vec<Option<usize>>, l: &mut Vec<usize>, s: &mut Vec<bool>| {
        if o.len() <= n {
            o.resize(n + 1, None);
            l.resize(n + 1, 0);
            s.resize(n + 1, false);
        }
    };
    for &root in starts {
        grow(root, &mut order, &mut low, &mut on_stack);
        if order[root].is_some() {
            continue;
        }
        let mut frames = vec![Frame {
            node: root,
            next: 0,
        }];
        while let Some(f) = frames.last() {
            let v = f.node;
            grow(v, &mut order, &mut low, &mut on_stack);
            if order[v].is_none() {
                order[v] = Some(counter);
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
                if edges.len() <= v {
                    edges.resize(v + 1, vec![]);
                }
                edges[v] = succ(v);
            }
            let f = frames.last_mut().expect("ICE: a Tarjan frame vanished");
            if let Some(&w) = edges[v].get(f.next) {
                f.next += 1;
                grow(w, &mut order, &mut low, &mut on_stack);
                match order[w] {
                    None => frames.push(Frame { node: w, next: 0 }),
                    Some(ow) if on_stack[w] => low[v] = low[v].min(ow),
                    Some(_) => {}
                }
                continue;
            }
            frames.pop();
            if let Some(parent) = frames.last() {
                let p = parent.node;
                low[p] = low[p].min(low[v]);
            }
            if Some(low[v]) == order[v] {
                let mut comp = vec![];
                loop {
                    let w = stack.pop().expect("ICE: Tarjan's stack ran out");
                    on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                if comp.len() > 1 || edges[v].contains(&v) {
                    out.push(comp);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::cyclic_components;

    fn comps(edges: &[&[usize]], starts: &[usize]) -> Vec<Vec<usize>> {
        let mut out = cyclic_components(starts, &mut |v| edges[v].to_vec());
        for c in &mut out {
            c.sort();
        }
        out.sort();
        out
    }

    #[test]
    fn a_chain_has_no_cycle() {
        assert!(comps(&[&[1], &[2], &[]], &[0]).is_empty());
    }

    #[test]
    fn a_self_edge_is_a_cycle_of_one() {
        assert_eq!(comps(&[&[0, 1], &[]], &[0]), vec![vec![0]]);
    }

    #[test]
    fn two_cycles_joined_by_an_edge_are_two_components() {
        // 0 <-> 1 -> 2 <-> 3, and 4 reaches both without being on either.
        let g: &[&[usize]] = &[&[1], &[0, 2], &[3], &[2], &[0]];
        assert_eq!(comps(g, &[4]), vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn a_node_reached_again_from_a_later_start_is_not_reported_twice() {
        let g: &[&[usize]] = &[&[1], &[0], &[0]];
        assert_eq!(comps(g, &[0, 2, 1]), vec![vec![0, 1]]);
    }
}
