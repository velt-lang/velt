//! Stringify (see stringify.vlt): serde derive.
use serde::Serialize;

#[derive(Serialize)]
struct Item {
    id: i64,
    name: String,
    score: f64,
    active: bool,
    tags: Vec<String>,
}

fn main() {
    let items: Vec<Item> = (0..100_000i64)
        .map(|i| Item {
            id: i,
            name: format!("user {i}"),
            score: (i % 1000) as f64 + 0.5,
            active: i % 2 == 0,
            tags: vec![format!("t{}", i % 7), "x".into()],
        })
        .collect();
    let mut total = 0;
    for _ in 0..8 {
        total += serde_json::to_string(&items).unwrap().len();
    }
    println!("{total}");
}
