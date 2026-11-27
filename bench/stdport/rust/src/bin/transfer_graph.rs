//! `transfer_graph.velt` in Rust: the same graph (each node reached twice from the one before it)
//! deep-copied for each of 2,000 threads with a `HashMap` from node to copy, so a node reached
//! twice is copied once, as the transfer map does.
use std::collections::HashMap;
use std::sync::Arc;

struct Node {
    v: f64,
    a: Option<Arc<Node>>,
    b: Option<Arc<Node>>,
}

fn copy(n: &Option<Arc<Node>>, seen: &mut HashMap<usize, Arc<Node>>) -> Option<Arc<Node>> {
    let n = n.as_ref()?;
    let key = Arc::as_ptr(n) as usize;
    if let Some(c) = seen.get(&key) {
        return Some(c.clone());
    }
    let a = copy(&n.a, seen);
    let b = copy(&n.b, seen);
    let c = Arc::new(Node { v: n.v, a, b });
    seen.insert(key, c.clone());
    seen.insert(Arc::as_ptr(&c) as usize, c.clone());
    Some(c)
}

fn sum(head: Option<Arc<Node>>) -> f64 {
    let mut s = 0.0;
    let mut p = head;
    while let Some(n) = p {
        let same = match (&n.a, &n.b) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return -1.0;
        }
        s += n.v;
        p = n.a.clone();
    }
    s
}

fn main() {
    let mut nodes = Vec::new();
    let mut head: Option<Arc<Node>> = None;
    for i in 0..1000 {
        let n = Arc::new(Node {
            v: i as f64,
            a: head.clone(),
            b: head.clone(),
        });
        nodes.push(n.clone());
        head = Some(n);
    }
    let mut total = 0.0;
    for _ in 0..2000 {
        let mut seen = HashMap::new();
        let copied = copy(&head, &mut seen);
        drop(seen);
        total += std::thread::spawn(move || sum(copied)).join().unwrap();
    }
    println!("{} {}", total, nodes.len());
}
