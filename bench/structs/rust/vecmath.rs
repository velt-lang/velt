// Vector math in a loop, n-body style, with a `Copy` struct: the algorithm of vecmath_*.vlt.
#[derive(Clone, Copy)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    fn add(self, o: Vec3) -> Vec3 {
        Vec3 { x: self.x + o.x, y: self.y + o.y, z: self.z + o.z }
    }
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3 { x: self.x - o.x, y: self.y - o.y, z: self.z - o.z }
    }
    fn scale(self, k: f64) -> Vec3 {
        Vec3 { x: self.x * k, y: self.y * k, z: self.z * k }
    }
    fn dot(self, o: Vec3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
}

#[derive(Clone, Copy)]
struct Body {
    pos: Vec3,
    vel: Vec3,
    mass: f64,
}

fn make_bodies(n: usize) -> Vec<Body> {
    (0..n)
        .map(|i| {
            let f = i as f64;
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            Body {
                pos: Vec3 { x: f * 1.5, y: f * 0.5 - 1.0, z: 0.1 * f },
                vel: Vec3 { x: 0.01 * f, y: 0.3 * sign, z: -0.02 * f },
                mass: 1.0 + f * 0.25,
            }
        })
        .collect()
}

fn step(bs: &mut [Body], dt: f64) {
    let n = bs.len();
    for i in 0..n {
        let b = bs[i];
        let mut acc = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
        for j in 0..n {
            if j == i {
                continue;
            }
            let d = bs[j].pos.sub(b.pos);
            let r2 = d.dot(d) + 0.01;
            acc = acc.add(d.scale(bs[j].mass / (r2 * r2.sqrt())));
        }
        bs[i] = Body { pos: b.pos, vel: b.vel.add(acc.scale(dt)), mass: b.mass };
    }
    for i in 0..n {
        let b = bs[i];
        bs[i] = Body { pos: b.pos.add(b.vel.scale(dt)), vel: b.vel, mass: b.mass };
    }
}

fn energy(bs: &[Body]) -> f64 {
    let mut e = 0.0;
    for i in 0..bs.len() {
        e += 0.5 * bs[i].mass * bs[i].vel.dot(bs[i].vel);
        for j in i + 1..bs.len() {
            let d = bs[i].pos.sub(bs[j].pos);
            e -= (bs[i].mass * bs[j].mass) / (d.dot(d) + 0.01).sqrt();
        }
    }
    e
}

fn main() {
    let mut bs = make_bodies(5);
    println!("{:.9}", energy(&bs));
    for _ in 0..1000000 {
        step(&mut bs, 0.001);
    }
    println!("{:.9}", energy(&bs));
}
