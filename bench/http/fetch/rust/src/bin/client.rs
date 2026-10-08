// The reqwest baseline: the same four scenarios as client.vlt and client.mjs.
// Usage: client <base url> [seq|conc|big|json|gzip|all].
use std::time::Instant;

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct User { id: i64, name: String, email: String, active: bool, score: f64 }

#[tokio::main]
async fn main() {
    let base = std::env::args().nth(1).unwrap_or("http://127.0.0.1:18080".into());
    let which = std::env::args().nth(2).unwrap_or("all".into());
    let c = reqwest::Client::new();
    if which == "all" || which == "seq" {
        let t = Instant::now();
        let mut n = 0usize;
        for _ in 0..10_000 { n += c.get(format!("{base}/small")).send().await.unwrap().text().await.unwrap().len(); }
        let s = t.elapsed().as_secs_f64();
        println!("seq: {:.0} req/s ({n} bytes) {:.3}s", 10_000.0 / s, s);
    }
    if which == "all" || which == "conc" {
        let t = Instant::now();
        let tasks: Vec<_> = (0..100).map(|_| { let c = c.clone(); let base = base.clone(); tokio::spawn(async move {
            let mut n = 0usize;
            for _ in 0..1000 { n += c.get(format!("{base}/small")).send().await.unwrap().text().await.unwrap().len(); }
            n })}).collect();
        let mut n = 0; for t in tasks { n += t.await.unwrap(); }
        let s = t.elapsed().as_secs_f64();
        println!("conc: {:.0} req/s ({n} bytes) {:.3}s", 100_000.0 / s, s);
    }
    if which == "all" || which == "big" {
        let t = Instant::now();
        let b = c.get(format!("{base}/big")).send().await.unwrap().bytes().await.unwrap();
        let s = t.elapsed().as_secs_f64();
        println!("big: {} bytes {:.3}s {:.0} MB/s", b.len(), s, b.len() as f64 / 1048576.0 / s);
    }
    if which == "all" || which == "json" {
        let t = Instant::now();
        let mut n = 0;
        for _ in 0..20 { let v: Vec<User> = c.get(format!("{base}/json")).send().await.unwrap().json().await.unwrap(); n += v.len(); }
        let s = t.elapsed().as_secs_f64();
        println!("json: 20 x 1MB {:.3}s {:.1} ms each ({n} users)", s, s * 1000.0 / 20.0);
    }
    if which == "all" || which == "gzip" {
        let t = Instant::now();
        let mut n = 0;
        for _ in 0..20 { let v: Vec<User> = c.get(format!("{base}/json-gzip")).send().await.unwrap().json().await.unwrap(); n += v.len(); }
        let s = t.elapsed().as_secs_f64();
        println!("gzip: 20 x 1MB {:.3}s {:.1} ms each ({n} users)", s, s * 1000.0 / 20.0);
    }
}
