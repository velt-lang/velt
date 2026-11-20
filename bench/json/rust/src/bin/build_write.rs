//! Building and writing (see build_write.velt): 3 rounds of 20k `serde_json::Value` records built,
//! read back, cloned with every fourth record changed, and both arrays written.
use serde_json::{json, Value};

fn main() {
    let n = 20_000;
    let mut total: i64 = 0;
    for _ in 0..3 {
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            rows.push(json!({
                "id": i,
                "name": format!("user {i}"),
                "score": (i % 1000) as f64 + 0.5,
                "active": i % 2 == 0,
                "tags": [format!("t{}", i % 7), "x"],
                "note": null,
            }));
        }
        let rows = Value::Array(rows);
        let mut sum = 0.0;
        for r in rows.as_array().unwrap() {
            let tag = r["tags"][0].as_str().unwrap();
            sum += r["id"].as_f64().unwrap() + tag.encode_utf16().count() as f64;
        }
        let mut copy = rows.clone();
        for (i, r) in copy.as_array_mut().unwrap().iter_mut().enumerate() {
            if i % 4 == 0 {
                r["score"] = json!(0);
            }
        }
        let a = serde_json::to_string(&rows).unwrap();
        let b = serde_json::to_string(&copy).unwrap();
        total += a.len() as i64 + b.len() as i64 + sum as i64;
    }
    println!("{total}");
}
