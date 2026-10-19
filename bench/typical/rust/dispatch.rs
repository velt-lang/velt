// bench/typical/dispatch.vlt in idiomatic Rust: boxed trait objects.
trait Shape {
    fn area(&self) -> f64;
}

struct Sq {
    s: f64,
}

impl Shape for Sq {
    fn area(&self) -> f64 {
        self.s * self.s
    }
}

trait Item {
    fn weight(&self) -> f64 {
        1.0
    }
}

struct Heavy {
    k: f64,
}

impl Item for Heavy {
    fn weight(&self) -> f64 {
        self.k * 2.0 + 1.0
    }
}

fn run(n: i64) -> f64 {
    let mut t = 0.0;
    for i in 0..n {
        let s: Box<dyn Shape> = Box::new(Sq { s: (i % 100) as f64 });
        t += s.area();
        let it: Box<dyn Item> = Box::new(Heavy { k: (i % 7) as f64 });
        t += it.weight();
    }
    t
}

fn main() {
    println!("{}", run(20_000_000));
}
