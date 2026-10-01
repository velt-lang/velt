//! TechEmpower Framework Benchmarks server on axum 0.8 + tokio-postgres (deadpool-postgres pool):
//! /json, /plaintext, /db, /queries?queries=N, /fortunes and /updates?queries=N
//! (bench/web/README.md has the rules).
//! Usage: web-bench-axum [port]; env PORT, HOST (127.0.0.1), DATABASE_URL, DB_POOL (default
//! 2 × cores).

use std::fmt::Write;

use axum::extract::{RawQuery, State};
use axum::http::header::{CONTENT_TYPE, SERVER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::serve::ListenerExt;
use axum::{Json, Router};
use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod};
use futures::future::try_join_all;
use serde::Serialize;

const SELECT_WORLD: &str = "SELECT id, randomnumber FROM world WHERE id = $1";
const SELECT_FORTUNES: &str = "SELECT id, message FROM fortune";
/// One statement for any N: the sorted ids and new numbers travel as two int arrays.
const UPDATE_WORLDS: &str = "UPDATE world SET randomnumber = u.r FROM (SELECT unnest($1::int[]) \
     AS id, unnest($2::int[]) AS r) AS u WHERE world.id = u.id";
const FORTUNES_HEAD: &str = "<!DOCTYPE html><html><head><title>Fortunes</title></head><body>\
     <table><tr><th>id</th><th>message</th></tr>";

#[derive(Serialize, Clone, Copy)]
struct World {
    id: i32,
    #[serde(rename = "randomNumber")]
    random_number: i32,
}

#[derive(Serialize)]
struct Message {
    message: &'static str,
}

/// A failed database call, answered with a 500.
struct DbError(String);

impl<E: std::fmt::Display> From<E> for DbError {
    fn from(e: E) -> Self {
        DbError(e.to_string())
    }
}

impl IntoResponse for DbError {
    fn into_response(self) -> Response {
        eprintln!("request failed: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    }
}

fn random_id() -> i32 {
    fastrand::i32(1..=10000)
}

/// `?queries=N`, clamped to 1..500; missing or not a number is 1.
fn query_count(query: Option<String>) -> usize {
    let n = query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("queries="))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1);
    n.clamp(1, 500) as usize
}

/// `n` random rows, one query each, pipelined on one pooled connection.
async fn fetch_worlds(client: &Object, n: usize) -> Result<Vec<World>, DbError> {
    let statement = client.prepare_cached(SELECT_WORLD).await?;
    let queries = (0..n).map(|_| {
        let statement = &statement;
        async move {
            let row = client.query_one(statement, &[&random_id()]).await?;
            Ok::<_, tokio_postgres::Error>(World {
                id: row.get(0),
                random_number: row.get(1),
            })
        }
    });
    Ok(try_join_all(queries).await?)
}

async fn plaintext() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Hello, World!",
    )
}

async fn json() -> impl IntoResponse {
    Json(Message {
        message: "Hello, World!",
    })
}

async fn db(State(pool): State<Pool>) -> Result<Json<World>, DbError> {
    let client = pool.get().await?;
    Ok(Json(fetch_worlds(&client, 1).await?[0]))
}

async fn queries(
    State(pool): State<Pool>,
    RawQuery(q): RawQuery,
) -> Result<Json<Vec<World>>, DbError> {
    let client = pool.get().await?;
    Ok(Json(fetch_worlds(&client, query_count(q)).await?))
}

async fn updates(
    State(pool): State<Pool>,
    RawQuery(q): RawQuery,
) -> Result<Json<Vec<World>>, DbError> {
    let client = pool.get().await?;
    let mut worlds = fetch_worlds(&client, query_count(q)).await?;
    for w in &mut worlds {
        w.random_number = random_id();
    }
    let mut sorted = worlds.clone();
    sorted.sort_by_key(|w| w.id);
    let ids: Vec<i32> = sorted.iter().map(|w| w.id).collect();
    let values: Vec<i32> = sorted.iter().map(|w| w.random_number).collect();
    let statement = client.prepare_cached(UPDATE_WORLDS).await?;
    client.execute(&statement, &[&ids, &values]).await?;
    Ok(Json(worlds))
}

fn escape_html(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

async fn fortunes(State(pool): State<Pool>) -> Result<Response, DbError> {
    let client = pool.get().await?;
    let statement = client.prepare_cached(SELECT_FORTUNES).await?;
    let mut fortunes: Vec<(i32, String)> = client
        .query(&statement, &[])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    fortunes.push((0, "Additional fortune added at request time.".to_string()));
    fortunes.sort_by(|a, b| a.1.cmp(&b.1));
    let mut html = String::with_capacity(2048);
    html.push_str(FORTUNES_HEAD);
    for (id, message) in &fortunes {
        let _ = write!(html, "<tr><td>{id}</td><td>");
        escape_html(&mut html, message);
        html.push_str("</td></tr>");
    }
    html.push_str("</table></body></html>");
    Ok(([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn add_server(mut res: Response) -> Response {
    res.headers_mut()
        .insert(SERVER, HeaderValue::from_static("axum"));
    res
}

fn env_or(name: &str, fallback: String) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
}

fn make_pool() -> Result<Pool, String> {
    let url = env_or(
        "DATABASE_URL",
        "postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world".into(),
    );
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let size: usize = env_or("DB_POOL", (cores * 2).to_string())
        .parse()
        .map_err(|e| format!("DB_POOL: {e}"))?;
    let config: tokio_postgres::Config = url.parse().map_err(|e| format!("DATABASE_URL: {e}"))?;
    let manager = Manager::from_config(
        config,
        tokio_postgres::NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    Pool::builder(manager)
        .max_size(size)
        .build()
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| env_or("PORT", "8080".into()));
    let addr = format!("{}:{port}", env_or("HOST", "127.0.0.1".into()));
    let app = Router::new()
        .route("/plaintext", get(plaintext))
        .route("/json", get(json))
        .route("/db", get(db))
        .route("/queries", get(queries))
        .route("/updates", get(updates))
        .route("/fortunes", get(fortunes))
        .layer(axum::middleware::map_response(add_server))
        .with_state(make_pool()?);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    println!("listening on http://{addr}");
    // TCP_NODELAY like the other servers (axum leaves Nagle on, which stalls pipelined responses).
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}
