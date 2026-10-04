trait Shape { fn area(&self) -> f64; fn scale(&mut self, k: f64); }
struct Circle { r: f64 }
struct Rect { w: f64, h: f64 }
impl Shape for Circle { fn area(&self) -> f64 { 3.14159 * self.r * self.r } fn scale(&mut self, k: f64) { self.r *= k; } }
impl Shape for Rect { fn area(&self) -> f64 { self.w * self.h } fn scale(&mut self, k: f64) { self.w *= k; self.h *= k; } }
struct Particle { x: f64, y: f64, vx: f64, vy: f64 }
impl Particle {
    fn step(&mut self, dt: f64) {
        self.x += self.vx * dt; self.y += self.vy * dt;
        if self.x < 0.0 || self.x > 100.0 { self.vx = -self.vx; }
        if self.y < 0.0 || self.y > 100.0 { self.vy = -self.vy; }
    }
    fn energy(&self) -> f64 { 0.5 * (self.vx * self.vx + self.vy * self.vy) }
}
fn main() {
    // JS objects are heap references: Box each like Velt/Node do.
    let mut ps: Vec<Box<Particle>> = Vec::new();
    for i in 0..200000i64 {
        ps.push(Box::new(Particle { x: (i % 100) as f64, y: ((i * 7) % 100) as f64, vx: ((i % 13) - 6) as f64, vy: ((i % 7) - 3) as f64 }));
    }
    let mut e = 0.0;
    for _ in 0..100 {
        for p in ps.iter_mut() { p.step(0.01); }
        for p in ps.iter() { e += p.energy(); }
    }
    println!("{:.3}", e);
    let mut shapes: Vec<Box<dyn Shape>> = Vec::new();
    for i in 0..500000i64 {
        if i % 2 == 0 { shapes.push(Box::new(Circle { r: ((i % 10) + 1) as f64 })); }
        else { shapes.push(Box::new(Rect { w: ((i % 5) + 1) as f64, h: ((i % 3) + 1) as f64 })); }
    }
    let mut total = 0.0;
    for round in 0..40 {
        for s in shapes.iter() { total += s.area(); }
        if round % 10 == 0 { for s in shapes.iter_mut() { s.scale(1.0001); } }
    }
    println!("{:.1}", total);
}
