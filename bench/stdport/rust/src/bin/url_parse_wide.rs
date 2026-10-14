use url::Url;

fn main() {
    let shapes = [
        "https://example.com/café/menü".to_string(),
        "https://example.com/search?q=café".to_string(),
        "https://example.com/page#über".to_string(),
        format!("https://example.com/{}é", "abcdefghij".repeat(100)),
        format!("https://example.com/s?{}", "q=abcdefg&".repeat(50)),
        format!("data:text/plain,{}", "abcdefghij".repeat(100)),
    ];
    let mut urls: Vec<String> = Vec::new();
    for k in 0..100 {
        for shape in &shapes {
            urls.push(format!("{shape}{k}"));
        }
    }
    let mut total: i64 = 0;
    for _ in 0..40 {
        for text in &urls {
            let u = Url::parse(text).unwrap();
            // `search` is the query with its `?`, and empty for an empty query.
            let search = match u.query() {
                Some(q) if !q.is_empty() => q.len() + 1,
                _ => 0,
            };
            let host = u.host_str().map_or(0, str::len);
            total += (u.as_str().len() + host + u.path().len() + search) as i64;
        }
    }
    println!("{total}");
}
