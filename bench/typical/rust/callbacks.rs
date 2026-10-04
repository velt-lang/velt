struct Emitter { handlers: Vec<Box<dyn Fn(f64) -> f64>> }
impl Emitter { fn emit(&self, x: f64) -> f64 { let mut s = 0.0; for h in &self.handlers { s += h(x); } s } }
fn compose(fs: Vec<Box<dyn Fn(f64) -> f64>>) -> Box<dyn Fn(f64) -> f64> { Box::new(move |x| { let mut v = x; for f in &fs { v = f(v); } v }) }
fn sum<'a>(xs: impl IntoIterator<Item = &'a f64>) -> f64 { let mut s = 0.0; for x in xs { s += x; } s }
fn main() {
    let mut em = Emitter { handlers: Vec::new() };
    for k in 1..=8 { let kk = k as f64; em.handlers.push(Box::new(move |x| x * kk)); }
    let mut total = 0.0;
    for i in 0..2000000 { total += em.emit((i % 100) as f64); }
    let pipe = compose(vec![Box::new(|x| x + 1.0), Box::new(|x| x * 2.0), Box::new(|x| x - 3.0), Box::new(|x| x / 2.0)]);
    for i in 0..10000000 { total += pipe((i % 1000) as f64); }
    let xs: Vec<f64> = (0..1000000).map(|i| (i % 7) as f64).collect();
    for _ in 0..20 { total += sum(&xs); }
    println!("{}", total);
}
