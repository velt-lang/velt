use url::Url;

fn main() {
    let shapes = [
        "https://example.com/",
        "http://example.com:8080/a/b/c?x=1&y=2#frag",
        "https://user:pw@sub.example.co.uk/path/to/file.html",
        "HTTP://EXAMPLE.COM/A%20B/../c/./d?q=a b",
        "https://example.com/a/b/../../c",
        "http://127.0.0.1:3000/api/v1/items?limit=10",
        "https://example.com:443/default-port",
        "ws://chat.example.com/socket?room=42",
        "https://api.example.com/v2/users/12345/posts?sort=desc&page=3&per_page=50",
        "https://cdn.example.com/assets/app.3f9c2a.js",
    ];
    let mut urls: Vec<String> = Vec::new();
    for k in 0..100 {
        for shape in shapes {
            urls.push(format!("{shape}{k}"));
        }
    }
    let mut total: i64 = 0;
    for _ in 0..300 {
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
