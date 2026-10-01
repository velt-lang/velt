//! axum 0.8 baseline for the TechEmpower-style benchmark (same routes as server.vlt).
//! Usage: techempower-axum <port>.

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

#[derive(Serialize)]
struct Message {
    message: &'static str,
}

struct Fortune {
    id: i64,
    message: String,
}

async fn plaintext() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Hello, World!")
}

async fn json() -> impl IntoResponse {
    Json(Message {
        message: "Hello, World!",
    })
}

/// Stands in for the database query: a fresh list per request.
fn load_fortunes() -> Vec<Fortune> {
    [
        (1, "fortune: No such file or directory"),
        (2, "A computer scientist is someone who fixes things that aren't broken."),
        (3, "After enough decimal places, nobody gives a damn."),
        (4, "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1"),
        (5, "A computer program does what you tell it to do, not what you want it to do."),
        (6, "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen"),
        (7, "Any program that runs right is obsolete."),
        (8, "A list is only as strong as its weakest link. — Donald Knuth"),
        (9, "Feature: A bug with seniority."),
        (10, "Computers make very fast, very accurate mistakes."),
        (11, "<script>alert(\"This should not be displayed in a browser alert box.\");</script>"),
        (12, "フレームワークのベンチマーク"),
    ]
    .into_iter()
    .map(|(id, m)| Fortune { id, message: m.to_string() })
    .collect()
}

fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

async fn fortunes() -> impl IntoResponse {
    let mut fortunes = load_fortunes();
    fortunes.push(Fortune {
        id: 0,
        message: "Additional fortune added at request time.".to_string(),
    });
    fortunes.sort_by(|a, b| a.message.cmp(&b.message));
    let mut html = String::with_capacity(2048);
    html.push_str("<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>");
    for f in &fortunes {
        html.push_str(&format!("<tr><td>{}</td><td>", f.id));
        escape_html(&f.message, &mut html);
        html.push_str("</td></tr>");
    }
    html.push_str("</table></body></html>");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let app = Router::new()
        .route("/plaintext", get(plaintext))
        .route("/json", get(json))
        .route("/fortunes", get(fortunes));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    println!("listening on http://127.0.0.1:{port}");
    axum::serve(listener, app).await.expect("serve");
}
