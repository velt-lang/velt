// Dynamic dispatch (same workload as classes.vlt): `Box<dyn Shape>` for the class hierarchy
// (heap object + virtual `area`), `Box<dyn Scorer>` for the interface values.
trait Shape {
    fn id(&self) -> i64;
    fn area(&self) -> f64;
    fn weight(&self) -> i64 {
        self.id() % 3
    }
}

struct Circle {
    id: i64,
    r: f64,
}
struct Rect {
    id: i64,
    w: f64,
    h: f64,
}
struct Square {
    id: i64,
    side: f64,
}

impl Shape for Circle {
    fn id(&self) -> i64 {
        self.id
    }
    fn area(&self) -> f64 {
        3.0 * self.r * self.r
    }
}
impl Shape for Rect {
    fn id(&self) -> i64 {
        self.id
    }
    fn area(&self) -> f64 {
        self.w * self.h
    }
}
impl Shape for Square {
    fn id(&self) -> i64 {
        self.id
    }
    fn area(&self) -> f64 {
        self.side * self.side
    }
}

trait Scorer {
    fn score(&self, x: i64) -> i64;
}
struct AddScorer {
    k: i64,
}
struct MulScorer {
    k: i64,
}
struct XorScorer {
    k: i64,
}
impl Scorer for AddScorer {
    fn score(&self, x: i64) -> i64 {
        x + self.k
    }
}
impl Scorer for MulScorer {
    fn score(&self, x: i64) -> i64 {
        (x * self.k) % 1000003
    }
}
impl Scorer for XorScorer {
    fn score(&self, x: i64) -> i64 {
        x ^ self.k
    }
}

fn main() {
    let mut shapes: Vec<Box<dyn Shape>> = Vec::new();
    for i in 0..1_000_000i64 {
        let size = (i % 17) as f64 + 0.5;
        if i % 3 == 0 {
            shapes.push(Box::new(Circle { id: i, r: size }));
        } else if i % 3 == 1 {
            shapes.push(Box::new(Rect { id: i, w: size, h: 2.0 }));
        } else {
            shapes.push(Box::new(Square { id: i, side: size }));
        }
    }
    let mut total = 0.0f64;
    let mut weights = 0i64;
    for _ in 0..20 {
        for s in &shapes {
            total += s.area();
            weights += s.weight();
        }
    }
    println!("{} {}", total, weights);

    let mut scorers: Vec<Box<dyn Scorer>> = Vec::new();
    for i in 0..1_000_000i64 {
        if i % 3 == 0 {
            scorers.push(Box::new(AddScorer { k: i % 11 }));
        } else if i % 3 == 1 {
            scorers.push(Box::new(MulScorer { k: i % 13 + 1 }));
        } else {
            scorers.push(Box::new(XorScorer { k: i % 7 }));
        }
    }
    let mut acc = 1i64;
    for _ in 0..20 {
        for sc in &scorers {
            acc = sc.score(acc) % 1000003;
        }
    }
    println!("{}", acc);
}
