// Closures capturing the object in its constructor and methods (same workload as
// closures_this.vlt): one heap object per iteration, closures borrowing it.
struct Particle {
    x: f64,
    v: f64,
    hits: f64,
}

impl Particle {
    fn new(x: f64, v: f64) -> Box<Particle> {
        let mut p = Box::new(Particle { x, v, hits: 0.0 });
        let mut wrap = || {
            if p.x > 1000.0 {
                p.x -= 1000.0;
                p.hits += 1.0;
            }
        };
        wrap();
        p
    }

    fn step(&mut self, k: f64) -> f64 {
        let mut mv = |d: f64| {
            self.x += self.v * d;
            if self.x > 1000.0 {
                self.x -= 1000.0;
                self.hits += 1.0;
            }
        };
        mv(k);
        mv(k + 1.0);
        let me = &*self;
        me.x + me.hits
    }
}

fn main() {
    let mut total = 0.0f64;
    let mut keep: Vec<Box<Particle>> = Vec::new();
    for i in 0..5_000_000i64 {
        let fi = i as f64;
        let mut p = Particle::new(fi % 1500.0, (fi % 7.0) + 1.0);
        total = (total + p.step(fi % 3.0)) % 1000003.0;
        if i % 100000 == 0 {
            keep.push(p);
        }
    }
    let hits: f64 = keep.iter().map(|p| p.hits).sum();
    println!("{} {} {}", total, keep.len(), hits);
}
