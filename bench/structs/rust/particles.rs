// A particle update over `Vec<Vec2>` with a `Copy` struct: the algorithm of particles_*.vlt.
#[derive(Clone, Copy)]
struct Vec2 {
    x: f64,
    y: f64,
}

impl Vec2 {
    fn add(self, o: Vec2) -> Vec2 {
        Vec2 { x: self.x + o.x, y: self.y + o.y }
    }
    fn scale(self, k: f64) -> Vec2 {
        Vec2 { x: self.x * k, y: self.y * k }
    }
}

fn main() {
    let n = 10000;
    let mut pos: Vec<Vec2> = Vec::new();
    let mut vel: Vec<Vec2> = Vec::new();
    for i in 0..n {
        pos.push(Vec2 { x: (i % 100) as f64 * 0.1, y: 1.0 + (i % 37) as f64 * 0.25 });
        vel.push(Vec2 { x: ((i % 7) - 3) as f64 * 0.5, y: (i % 11) as f64 * 0.2 });
    }
    let g = Vec2 { x: 0.0, y: -9.81 };
    let dt = 0.001;
    for _ in 0..500 {
        for i in 0..n as usize {
            let mut v = vel[i].add(g.scale(dt));
            let mut p = pos[i].add(v.scale(dt));
            if p.y < 0.0 {
                p = Vec2 { x: p.x, y: -p.y };
                v = Vec2 { x: v.x, y: -v.y * 0.9 };
            }
            pos[i] = p;
            vel[i] = v;
        }
    }
    let mut sx = 0.0;
    let mut sy = 0.0;
    for i in 0..n as usize {
        sx += pos[i].x;
        sy += pos[i].y + vel[i].y;
    }
    println!("{:.6} {:.6}", sx, sy);
}
