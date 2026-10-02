//! The document shared by the JSON benchmarks (the same text as bench/json/*.vlt build).

/// `n` objects in one array: `{"id":i,"name":"user i","score":(i%1000).5,...}`.
pub fn doc(n: i64) -> String {
    let parts: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"{{"id":{i},"name":"user {i}","score":{}.5,"active":{},"tags":["t{}","x"]}}"#,
                i % 1000,
                i % 2 == 0,
                i % 7
            )
        })
        .collect();
    format!("[{}]", parts.join(","))
}
