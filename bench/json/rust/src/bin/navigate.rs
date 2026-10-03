//! Navigation (see navigate.vlt): `Value::get` by index and key.
use serde_json::Value;

fn main() {
    let v: Value = serde_json::from_str(&json_bench::doc(100_000)).unwrap();
    let n = v.as_array().unwrap().len();
    let mut sum: i64 = 0;
    for _ in 0..20 {
        for i in 0..n {
            let Some(item) = v.get(i) else { continue };
            let id = item.get("id").and_then(Value::as_f64).unwrap_or(0.0);
            let score = item.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            let tag = item
                .get("tags")
                .and_then(|t| t.get(0))
                .and_then(Value::as_str)
                .unwrap_or("");
            sum += id as i64 + (score * 2.0) as i64 + tag.len() as i64;
        }
    }
    println!("{n} {sum}");
}
