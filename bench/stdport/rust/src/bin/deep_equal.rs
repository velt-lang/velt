// Deep equality's cycle guard: the same program as bench/stdport/deep_equal.velt, with the graphs
// in an arena and the pairs being compared on a stack, as the runtime's guard (velt_rt
// `deep_eq.rs`, before std/lang/hooks/deep_eq.velt) kept them: a pair met again is equal, an
// object met again with another partner differs.
use std::hint::black_box;

struct Node {
    v: f64,
    next: Option<usize>,
    head: Option<usize>,
}

struct Guard {
    pairs: Vec<(usize, usize)>,
}

impl Guard {
    /// 0: a new pair (compare it), 1: this pair again (equal), 2: one side again (different).
    fn enter(&mut self, a: usize, b: usize) -> u8 {
        for &(x, y) in self.pairs.iter().rev() {
            if x == a {
                return if y == b { 1 } else { 2 };
            }
            if y == b {
                return 2;
            }
        }
        self.pairs.push((a, b));
        0
    }

    fn leave(&mut self) {
        self.pairs.pop();
    }
}

fn same(nodes: &[Node], g: &mut Guard, a: Option<usize>, b: Option<usize>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => equal(nodes, g, a, b),
        _ => false,
    }
}

fn equal(nodes: &[Node], g: &mut Guard, a: usize, b: usize) -> bool {
    if a == b {
        return true;
    }
    match g.enter(a, b) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let (x, y) = (&nodes[a], &nodes[b]);
    let eq = x.v == y.v && same(nodes, g, x.next, y.next) && same(nodes, g, x.head, y.head);
    g.leave();
    eq
}

/// A list of `n` nodes valued `i % 7`, the last one `last`, each pointing back at the first.
fn list(nodes: &mut Vec<Node>, n: usize, last: f64) -> usize {
    let first = nodes.len();
    for i in 0..n {
        let v = if i == n - 1 { last } else { (i % 7) as f64 };
        let next = (i + 1 < n).then_some(first + i + 1);
        nodes.push(Node {
            v,
            next,
            head: Some(first),
        });
    }
    first
}

/// A ring of `n` nodes valued 1.
fn ring(nodes: &mut Vec<Node>, n: usize) -> usize {
    let first = nodes.len();
    for i in 0..n {
        let next = if i + 1 < n { first + i + 1 } else { first };
        nodes.push(Node {
            v: 1.0,
            next: Some(next),
            head: None,
        });
    }
    first
}

fn main() {
    let mut nodes = Vec::new();
    let a = list(&mut nodes, 64, (63 % 7) as f64);
    let b = list(&mut nodes, 64, (63 % 7) as f64);
    let c = list(&mut nodes, 64, 99.0);
    let rings: Vec<usize> = (1..=8).map(|n| ring(&mut nodes, n)).collect();
    let others: Vec<usize> = (1..=8).map(|n| ring(&mut nodes, n)).collect();
    let mut g = Guard { pairs: Vec::new() };
    let mut total: u64 = 0;
    for round in 0..3000usize {
        if equal(&nodes, &mut g, black_box(a), black_box(b)) {
            total += 1;
        }
        if equal(&nodes, &mut g, black_box(a), black_box(c)) {
            total += 1_000_000;
        }
        if equal(&nodes, &mut g, rings[round % 8], others[round % 8]) {
            total += 1000;
        }
    }
    println!("{total}");
}
