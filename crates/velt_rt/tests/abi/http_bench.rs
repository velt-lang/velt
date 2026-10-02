//! HTTP server throughput benchmark (ignored by default):
//! `cargo test -p velt_rt --release --lib http_bench -- --ignored --nocapture`.
//!
//! The server is `velt_rt_http_serve` with the handler generated code would emit for
//! `(req) => Response.text("Hello, World!")`, written against the C ABI. The load generator is a
//! raw HTTP/1.1 keep-alive client on its own tokio runtime (`BENCH_CLIENT_THREADS`, default 4):
//! `BENCH_CONNS` connections (default 64), each sending one request at a time for `BENCH_SECS`
//! (default 5). `BENCH_TARGET=host:port` measures another server instead (e.g. a plain hyper or
//! axum hello-world, see bench/http/). Set `VELT_THREADS` to size the server's workers.
//! `BENCH_OHA=1` additionally runs `oha` (on PATH) with the same connections and duration.

use super::fake::{arg, block_on_fut, ok};
use crate::http::request::*;
use crate::http::response::*;
use crate::http::server::*;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::READY;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[repr(C)]
struct Hello {
    result: RespHandle,
    req: ReqHandle,
}

unsafe extern "C" fn hello_init(_env: *mut c_void, req: *mut ReqObj, state: *mut u8) {
    (state as *mut Hello).write(Hello {
        result: RespHandle::NULL,
        req: ReqHandle::from_bits(req as usize as u64),
    });
}

unsafe extern "C" fn hello_poll(s: *mut u8, _cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Hello);
    velt_rt_http_req_drop(st.req);
    let resp = velt_rt_http_resp_new(200);
    // A string literal is static (cap == 0), exactly as generated code passes it.
    let mut body = VeltStr::from_static(b"Hello, World!");
    velt_rt_http_resp_body_text(resp, &mut body);
    st.result = resp;
    READY
}

unsafe extern "C" fn hello_drop(s: *mut u8) {
    velt_rt_http_req_drop((*(s as *mut Hello)).req);
}

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn start_rt_server() -> String {
    let handler = VeltHandler {
        init: hello_init,
        poll: hello_poll,
        drop: hello_drop,
        state_size: size_of::<Hello>() as u64,
        state_align: 8,
        env: std::ptr::null_mut(),
    };
    let server = ok(block_on_fut::<IoResult<ServerHandle>>(unsafe {
        velt_rt_http_serve(&arg("127.0.0.1:0"), &handler)
    }));
    format!("127.0.0.1:{}", unsafe { velt_rt_http_server_port(server) })
}

/// Read one response (headers + `content-length` body) from `buf`/`s`; returns false on EOF.
async fn read_response(s: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(&buf[..end]).expect("response head is UTF-8");
            assert!(head.starts_with("HTTP/1.1 200"), "bad response: {head}");
            let len: usize = head
                .lines()
                .find_map(|l| {
                    let (n, v) = l.split_once(':')?;
                    n.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .expect("content-length");
            let total = end + 4 + len;
            while buf.len() < total {
                if !fill(s, buf).await {
                    return false;
                }
            }
            buf.drain(..total);
            return true;
        }
        if !fill(s, buf).await {
            return false;
        }
    }
}

async fn fill(s: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 4096];
    match s.read(&mut chunk).await {
        Ok(0) | Err(_) => false,
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            true
        }
    }
}

/// One keep-alive connection sending requests back to back until `stop`; returns its count.
async fn connection(addr: String, stop: Arc<AtomicBool>) -> u64 {
    let mut s = TcpStream::connect(&addr).await.expect("connect");
    s.set_nodelay(true).unwrap();
    let req = b"GET /plaintext HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let mut buf = Vec::with_capacity(8192);
    let mut n = 0;
    while !stop.load(Ordering::Relaxed) {
        s.write_all(req).await.expect("write");
        if !read_response(&mut s, &mut buf).await {
            break;
        }
        n += 1;
    }
    n
}

fn load(addr: &str, conns: usize, secs: u64, client_threads: usize) -> f64 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(client_threads)
        .enable_all()
        .build()
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicU64::new(0));
    let t = Instant::now();
    rt.block_on(async {
        let tasks: Vec<_> = (0..conns)
            .map(|_| {
                let (addr, stop, total) = (addr.to_string(), stop.clone(), total.clone());
                tokio::spawn(async move {
                    total.fetch_add(connection(addr, stop).await, Ordering::Relaxed);
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_secs(secs)).await;
        stop.store(true, Ordering::Relaxed);
        for task in tasks {
            task.await.unwrap();
        }
    });
    total.load(Ordering::Relaxed) as f64 / t.elapsed().as_secs_f64()
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn http_bench_hello_world() {
    let addr = std::env::var("BENCH_TARGET").unwrap_or_else(|_| start_rt_server());
    let (conns, secs) = (env_or("BENCH_CONNS", 64), env_or("BENCH_SECS", 5) as u64);
    let client_threads = env_or("BENCH_CLIENT_THREADS", 4);
    load(&addr, conns.min(4), 1, client_threads); // warm-up
    let rps = load(&addr, conns, secs, client_threads);
    eprintln!(
        "http bench {addr}: {conns} connections, {client_threads} client threads, {secs}s: {rps:.0} req/s"
    );
    assert!(rps > 1000.0, "only {rps:.0} req/s");
    if std::env::var_os("BENCH_OHA").is_some() {
        run_oha(&addr, conns, secs);
    }
}

/// Same load with `oha` (must be on PATH), printing its requests/sec line.
fn run_oha(addr: &str, conns: usize, secs: u64) {
    let out = std::process::Command::new("oha")
        .args([
            "--no-tui",
            "-z",
            &format!("{secs}s"),
            "-c",
            &conns.to_string(),
        ])
        .arg(format!("http://{addr}/plaintext"))
        .output()
        .expect("oha on PATH");
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text
        .lines()
        .filter(|l| l.contains("Requests/sec") || l.contains("[200]"))
    {
        eprintln!("oha {addr}: {}", line.trim());
    }
}

/// The cost of building and taking back one response through the ABI (what every request adds
/// on top of hyper), alone and with every worker thread doing it at once (registry contention):
/// `cargo test -p velt_rt --release --lib http_response_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn http_response_cost() {
    fn build(n: u32) {
        for _ in 0..n {
            let resp = velt_rt_http_resp_new(200);
            let mut body = VeltStr::from_static(b"Hello, World!");
            unsafe { velt_rt_http_resp_body_text(resp, &mut body) };
            std::hint::black_box(take(resp));
        }
    }
    // What the same work cost when a response was a raw `Box` pointer (before the registry).
    fn build_boxed(n: u32) {
        for _ in 0..n {
            let mut resp = Box::new(hyper::Response::new(crate::http::body::RespBody::full(
                bytes::Bytes::new(),
            )));
            *resp.status_mut() = hyper::StatusCode::OK;
            let mut body = VeltStr::from_static(b"Hello, World!");
            let text = unsafe { crate::http::take_text(&mut body) };
            *resp.body_mut() = crate::http::body::RespBody::full(text);
            resp.headers_mut()
                .entry(hyper::header::CONTENT_TYPE)
                .or_insert(hyper::header::HeaderValue::from_static("text/plain"));
            std::hint::black_box(*resp);
        }
    }
    let n = 1_000_000u32;
    build_boxed(n / 10);
    let t = Instant::now();
    build_boxed(n);
    let boxed = t.elapsed().as_nanos() as f64 / n as f64;
    eprintln!("boxed response build+take: {boxed:.0} ns (1 thread)");
    build(n / 10);
    let t = Instant::now();
    build(n);
    let one = t.elapsed().as_nanos() as f64 / n as f64;
    let threads = env_or("BENCH_THREADS", 16);
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| build(n / 10));
        }
    });
    let all = t.elapsed().as_nanos() as f64 / (threads as f64 * (n / 10) as f64);
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| build_boxed(n / 10));
        }
    });
    let boxed_all = t.elapsed().as_nanos() as f64 / (threads as f64 * (n / 10) as f64);
    eprintln!("boxed response build+take: {boxed_all:.0} ns/response ({threads} threads at once)");
    eprintln!("response build+take: {one:.0} ns (1 thread), {all:.0} ns/response ({threads} threads at once)");
}
