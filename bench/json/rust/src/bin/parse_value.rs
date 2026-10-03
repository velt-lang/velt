//! Dynamic parse (see parse_value.vlt): `serde_json::Value`.
use serde_json::Value;

fn main() {
    let text = json_bench::doc(100_000);
    let mut sum = 0;
    for round in 0..8 {
        let v: Value = serde_json::from_str(&text).unwrap();
        let items = v.as_array().unwrap();
        sum += items.len() + items[round].as_object().unwrap().len();
    }
    println!("{} {}", text.len(), sum);
}
