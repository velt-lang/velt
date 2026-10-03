//! Union parse (see parse_union.vlt): an internally tagged serde enum (the tag may come last).
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Shape {
    Circle { r: f64 },
    Rect { w: f64, h: f64 },
}

fn main() {
    let parts: Vec<String> = (0..200_000i64)
        .map(|i| {
            if i % 2 == 0 {
                format!(r#"{{"kind":"circle","r":{}.5}}"#, i % 100)
            } else {
                format!(r#"{{"w":{},"h":{},"kind":"rect"}}"#, i % 100, i % 7)
            }
        })
        .collect();
    let text = format!("[{}]", parts.join(","));
    let mut sum: i64 = 0;
    for _ in 0..8 {
        let shapes: Vec<Shape> = serde_json::from_str(&text).unwrap();
        for s in &shapes {
            sum += match s {
                Shape::Circle { r } => (r * 2.0) as i64,
                Shape::Rect { w, h } => (w * h) as i64,
            };
        }
    }
    println!("{} {}", text.len(), sum);
}
