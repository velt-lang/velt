use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Cell {
    x: i32,
    y: i32,
}

const N: i32 = 300;

fn wall(c: Cell) -> bool {
    (c.x * 7 + c.y * 13) % 11 == 0 && c != Cell { x: 0, y: 0 }
}

fn search(start: Cell, goal: Cell) -> i32 {
    let mut seen = HashSet::new();
    let mut queue = vec![start];
    seen.insert(start);
    let mut steps = 0;
    while !queue.is_empty() {
        let mut next = Vec::new();
        for &c in &queue {
            if c == goal {
                return steps;
            }
            let around = [
                Cell { x: c.x + 1, y: c.y },
                Cell { x: c.x - 1, y: c.y },
                Cell { x: c.x, y: c.y + 1 },
                Cell { x: c.x, y: c.y - 1 },
            ];
            for d in around {
                if d.x < 0 || d.y < 0 || d.x >= N || d.y >= N || wall(d) || seen.contains(&d) {
                    continue;
                }
                seen.insert(d);
                next.push(d);
            }
        }
        queue = next;
        steps += 1;
    }
    -1
}

fn main() {
    let mut total = 0;
    for i in 0..20 {
        total += search(Cell { x: 0, y: i }, Cell { x: N - 1, y: N - 1 - i });
    }
    println!("{}", total);
}
