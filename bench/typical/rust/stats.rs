// bench/typical/stats.vlt in idiomatic Rust.
#[derive(Clone, Copy, Default)]
struct Stats {
    n: f64,
    sum: f64,
    sq: f64,
}

impl Stats {
    fn add(&mut self, x: f64) {
        self.n += 1.0;
        self.sum += x;
        self.sq += x * x;
    }
    fn merge(&mut self, o: &Stats) {
        self.n += o.n;
        self.sum += o.sum;
        self.sq += o.sq;
    }
    fn mean(&self) -> f64 {
        self.sum / self.n
    }
}

fn summarize(rows: i64) -> f64 {
    let mut out = 0.0;
    for r in 0..rows {
        let mut even = Stats::default();
        let mut odd = Stats::default();
        for k in 0..16i64 {
            let x = ((r * 31 + k * 7) % 101) as f64;
            if k % 2 == 0 {
                even.add(x);
            } else {
                odd.add(x);
            }
        }
        let mut all = Stats::default();
        all.merge(&even);
        all.merge(&odd);
        if all.n > 0.0 {
            out += all.mean() + all.sq / all.n;
        }
        out -= even.mean();
    }
    out
}

fn main() {
    println!("{:.3}", summarize(2_000_000));
}
