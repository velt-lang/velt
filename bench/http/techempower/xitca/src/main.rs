//! xitca-web baseline for the TechEmpower-style benchmark (same routes and bodies as
//! server.velt and axum/). Usage: techempower-xitca <port> [workers] (workers: XITCA_WORKERS or
//! the second argument, else the number of cores).

use serde::Serialize;
use xitca_web::body::ResponseBody;
use xitca_web::handler::handler_service;
use xitca_web::http::header::{HeaderValue, CONTENT_TYPE};
use xitca_web::http::WebResponse;
use xitca_web::route::get;
use xitca_web::App;

#[derive(Serialize)]
struct Message {
    message: &'static str,
}

struct Fortune {
    id: i64,
    message: String,
}

fn with_type(body: impl Into<ResponseBody>, content_type: &'static str) -> WebResponse {
    let mut res = WebResponse::new(body.into());
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

async fn plaintext() -> WebResponse {
    with_type("Hello, World!", "text/plain; charset=utf-8")
}

async fn json() -> WebResponse {
    let body = serde_json::to_vec(&Message {
        message: "Hello, World!",
    })
    .expect("json");
    with_type(body, "application/json")
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

async fn fortunes() -> WebResponse {
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
    with_type(html, "text/html; charset=utf-8")
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let workers: usize = args
        .next()
        .or_else(|| std::env::var("XITCA_WORKERS").ok())
        .and_then(|w| w.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    println!("listening on http://127.0.0.1:{port} with {workers} workers");
    App::new()
        .at("/plaintext", get(handler_service(plaintext)))
        .at("/json", get(handler_service(json)))
        .at("/fortunes", get(handler_service(fortunes)))
        .serve()
        .worker_threads(workers)
        .bind(("127.0.0.1", port))?
        .run()
        .wait()
}
