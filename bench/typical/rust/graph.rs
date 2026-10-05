use std::rc::Rc; use std::cell::{Cell, RefCell};
struct GNode { dist: Cell<f64>, neighbors: RefCell<Vec<Rc<GNode>>> }
fn main() {
    let n = 100000usize;
    let nodes: Vec<Rc<GNode>> = (0..n).map(|_| Rc::new(GNode { dist: Cell::new(-1.0), neighbors: RefCell::new(Vec::new()) })).collect();
    for i in 0..n {
        let mut nb = nodes[i].neighbors.borrow_mut();
        nb.push(nodes[(i + 1) % n].clone()); nb.push(nodes[(i * 7 + 3) % n].clone()); nb.push(nodes[(i * 13 + 5) % n].clone());
    }
    let mut total = 0.0;
    for round in 0..20 {
        for nd in &nodes { nd.dist.set(-1.0); }
        let start = nodes[(round * 977) % n].clone();
        start.dist.set(0.0);
        let mut queue: Vec<Rc<GNode>> = vec![start];
        let mut head = 0;
        while head < queue.len() {
            let cur = queue[head].clone(); head += 1;
            for nb in cur.neighbors.borrow().iter() {
                if nb.dist.get() < 0.0 { nb.dist.set(cur.dist.get() + 1.0); queue.push(nb.clone()); }
            }
        }
        for nd in &nodes { total += nd.dist.get(); }
    }
    println!("{}", total);
}
