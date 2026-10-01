// Tagged unions (same workload as shapes.vlt): a Rust enum and `match`.
#[derive(Clone, Copy)]
enum Shape {
    Circle { r: f64 },
    Rect { w: f64, h: f64 },
    Tri { b: f64, h: f64 },
    Empty,
}

fn area(s: &Shape) -> f64 {
    match *s {
        Shape::Circle { r } => 3.0 * r * r,
        Shape::Rect { w, h } => w * h,
        Shape::Tri { b, h } => 0.5 * b * h,
        Shape::Empty => 0.0,
    }
}

fn make(i: i64) -> Shape {
    let x = ((i * 7919) % 1000) as f64 * 0.001;
    match (i * 31) % 4 {
        0 => Shape::Circle { r: x },
        1 => Shape::Rect { w: x, h: 2.0 },
        2 => Shape::Tri { b: x, h: 4.0 },
        _ => Shape::Empty,
    }
}

fn main() {
    let mut shapes = Vec::new();
    for i in 0..1_000_000 {
        shapes.push(make(i));
    }
    let mut total = 0.0;
    let mut empties: i64 = 0;
    for _ in 0..40 {
        for s in &shapes {
            total += area(s);
            if matches!(s, Shape::Empty) {
                empties += 1;
            }
        }
    }
    println!("{} {}", total.floor() as i64, empties);
}
