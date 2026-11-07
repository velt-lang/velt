#[derive(Clone, Copy)]
struct Vec2 { x: f64, y: f64 }
#[inline(never)]
fn walk(p: Vec2, d: Vec2, n: i64) -> Vec2 {
    if n == 0 { return p; }
    let q = Vec2 { x: p.x + d.x * 0.5, y: p.y - d.y * 0.25 };
    let e = Vec2 { x: d.y, y: -d.x };
    let r = walk(q, e, n - 1);
    Vec2 { x: r.x * 0.999, y: r.y + 1.0 }
}
fn main() {
    let mut acc = Vec2 { x: 0.0, y: 0.0 };
    for i in 0..20000i64 {
        let r = walk(Vec2 { x: i as f64, y: 1.0 }, Vec2 { x: 0.5, y: 0.25 }, 500);
        acc = Vec2 { x: acc.x + r.x, y: acc.y + r.y };
    }
    println!("{:.3} {:.3}", acc.x, acc.y);
}
