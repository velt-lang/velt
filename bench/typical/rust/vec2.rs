#[derive(Clone, Copy)]
struct Vec2 { x: f64, y: f64 }
impl Vec2 {
    fn add(self, o: Vec2) -> Vec2 { Vec2 { x: self.x + o.x, y: self.y + o.y } }
    fn scale(self, k: f64) -> Vec2 { Vec2 { x: self.x * k, y: self.y * k } }
    fn len(self) -> f64 { (self.x * self.x + self.y * self.y).sqrt() }
}
fn main() {
    let mut p = Vec2 { x: 0.0, y: 0.0 };
    let v = Vec2 { x: 0.5, y: 0.25 };
    let mut total = 0.0;
    for _ in 0..20000000 {
        p = p.add(v.scale(0.001));
        if p.x > 10.0 { p = Vec2 { x: 0.0, y: p.y }; }
        if p.y > 10.0 { p = Vec2 { x: p.x, y: 0.0 }; }
        total += p.len();
    }
    println!("{:.3}", total);
}
