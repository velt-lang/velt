//! Editing (see edit.vlt): an order-preserving `serde_json::Map` (`shift_remove` keeps the order,
//! like `delete` in JavaScript and Velt).
use serde_json::{Map, Value};

fn main() {
    let n = 10_000;
    let keys: Vec<String> = (0..n).map(|i| format!("key{i}")).collect();
    let mut sum = 0;
    for _ in 0..4 {
        let mut obj = Map::new();
        for (i, k) in keys.iter().enumerate() {
            obj.insert(k.clone(), Value::from(i as f64));
        }
        sum += obj.len();
        for k in &keys {
            if obj.shift_remove(k).is_some() {
                sum += 1;
            }
        }
        sum += obj.len();
    }
    println!("{sum}");
}
