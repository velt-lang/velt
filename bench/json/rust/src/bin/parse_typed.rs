//! Typed parse (see parse_typed.vlt): serde derive into a struct.
use serde::Deserialize;

#[derive(Deserialize)]
struct Item {
    id: i64,
    name: String,
    score: f64,
    active: bool,
    tags: Vec<String>,
}

fn main() {
    let text = json_bench::doc(100_000);
    let mut sum: i64 = 0;
    for _ in 0..8 {
        let items: Vec<Item> = serde_json::from_str(&text).unwrap();
        for it in &items {
            sum += it.id + (it.score * 2.0) as i64 + it.active as i64;
            sum += it.name.len() as i64 + it.tags[0].len() as i64;
        }
    }
    println!("{} {}", text.len(), sum);
}
