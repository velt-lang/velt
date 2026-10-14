//! The Rust baselines for bench/websocket (README.md), on the stack Velt's runtime uses.
//! Usage: ws-bench-rust [port]; env MODE, READ_BUF, HOST (127.0.0.1).
//!
//!   MODE=raw    idiomatic tokio-tungstenite: TcpListener + accept_async, one task per
//!               connection, /bcast through a writer task per connection (mpsc)
//!   MODE=shard  MODE=raw on SHARDS (default: all cores) single-threaded runtimes, one thread
//!               each: a blocking accept loop hands each connection to the next shard, which
//!               registers it with its own I/O driver; nothing moves between threads after that
//!               (/bcast crosses shards through the room's channels)
//!   MODE=hyper  Velt's runtime path (velt_rt/src/http/server.rs, ws/): hyper-util's auto builder
//!               with upgrades, the socket split into halves behind tokio Mutexes, /bcast with
//!               join_all over sends to every member
//!
//! READ_BUF: tungstenite's read_buffer_size in bytes (default 131072, tungstenite's default).

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, UPGRADE};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

fn config() -> WebSocketConfig {
    let read = std::env::var("READ_BUF").ok().and_then(|s| s.parse().ok()).unwrap_or(128 * 1024);
    WebSocketConfig::default().read_buffer_size(read)
}

static IDS: AtomicU64 = AtomicU64::new(0);

// ---- MODE=raw ---------------------------------------------------------------------------------

type RawRoom = Arc<std::sync::Mutex<HashMap<u64, mpsc::UnboundedSender<Message>>>>;

async fn raw_conn(tcp: TcpStream, room: RawRoom) {
    let _ = tcp.set_nodelay(true);
    let mut path = String::new();
    let cb = |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
        path = req.uri().path().to_string();
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async_with_config(tcp, cb, Some(config())).await else {
        return;
    };
    if path == "/bcast" {
        let (mut tx, mut rx) = ws.split();
        let (q, mut queue) = mpsc::unbounded_channel::<Message>();
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        room.lock().unwrap().insert(id, q);
        let writer = tokio::spawn(async move {
            while let Some(m) = queue.recv().await {
                if tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(m)) = rx.next().await {
            if m.is_text() || m.is_binary() {
                for q in room.lock().unwrap().values() {
                    let _ = q.send(m.clone());
                }
            }
        }
        room.lock().unwrap().remove(&id);
        writer.abort();
    } else {
        let (mut tx, mut rx) = ws.split();
        while let Some(Ok(m)) = rx.next().await {
            if (m.is_text() || m.is_binary()) && tx.send(m).await.is_err() {
                break;
            }
        }
    }
}

// ---- MODE=hyper -------------------------------------------------------------------------------

type Ws = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

struct Halves {
    tx: Mutex<SplitSink<Ws, Message>>,
    rx: Mutex<SplitStream<Ws>>,
}

type HyperRoom = Arc<std::sync::Mutex<HashMap<u64, Arc<Halves>>>>;

async fn send(h: &Halves, m: Message) -> bool {
    h.tx.lock().await.send(m).await.is_ok()
}

async fn receive(h: &Halves) -> Option<Message> {
    loop {
        match h.rx.lock().await.next().await {
            Some(Ok(m)) if m.is_text() || m.is_binary() => return Some(m),
            Some(Ok(_)) => continue,
            _ => return None,
        }
    }
}

async fn hyper_session(on: hyper::upgrade::OnUpgrade, bcast: bool, room: HyperRoom) {
    let Ok(up) = on.await else { return };
    let ws = WebSocketStream::from_raw_socket(TokioIo::new(up), Role::Server, Some(config())).await;
    let (tx, rx) = ws.split();
    let h = Arc::new(Halves { tx: Mutex::new(tx), rx: Mutex::new(rx) });
    if bcast {
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        room.lock().unwrap().insert(id, h.clone());
        while let Some(m) = receive(&h).await {
            let members: Vec<Arc<Halves>> = room.lock().unwrap().values().cloned().collect();
            futures_util::future::join_all(members.iter().map(|s| send(s, m.clone()))).await;
        }
        room.lock().unwrap().remove(&id);
    } else {
        while let Some(m) = receive(&h).await {
            if !send(&h, m).await {
                break;
            }
        }
    }
    let _ = h.tx.lock().await.close().await;
}

async fn hyper_handle(mut req: Request<Incoming>, room: HyperRoom) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let Some(key) = req.headers().get(SEC_WEBSOCKET_KEY).cloned() else {
        return Ok(Response::new(Full::new(Bytes::from_static(b"websocket bench server: /echo or /bcast\n"))));
    };
    let bcast = req.uri().path() == "/bcast";
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(hyper_session(on, bcast, room));
    let mut r = Response::new(Full::new(Bytes::new()));
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = r.headers_mut();
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(SEC_WEBSOCKET_ACCEPT, HeaderValue::from_str(&derive_accept_key(key.as_bytes())).unwrap());
    Ok(r)
}

fn shard_main(port: u16, host: &str) {
    let n: usize = std::env::var("SHARDS").ok().and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let room: RawRoom = Default::default();
    let mut queues = Vec::new();
    for _ in 0..n {
        let (tx, mut rx) = mpsc::unbounded_channel::<std::net::TcpStream>();
        queues.push(tx);
        let room = room.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                while let Some(s) = rx.recv().await {
                    if let Ok(tcp) = TcpStream::from_std(s) {
                        tokio::spawn(raw_conn(tcp, room.clone()));
                    }
                }
            });
        });
    }
    let listener = std::net::TcpListener::bind((host, port)).unwrap();
    println!("listening on ws://{host}:{port} (shard x{n})");
    for (i, s) in listener.incoming().enumerate() {
        if let Ok(s) = s {
            let _ = s.set_nonblocking(true);
            let _ = queues[i % n].send(s);
        }
    }
}

fn main() {
    if std::env::var("MODE").is_ok_and(|m| m == "shard") {
        let port: u16 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8080);
        shard_main(port, &std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into()));
        return;
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(async_main());
}

async fn async_main() {
    let port: u16 = std::env::args().nth(1).or_else(|| std::env::var("PORT").ok()).and_then(|s| s.parse().ok()).unwrap_or(8080);
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let hyper_mode = std::env::var("MODE").map_or(false, |m| m == "hyper");
    let listener = TcpListener::bind((host.as_str(), port)).await.unwrap();
    println!("listening on ws://{host}:{port} ({})", if hyper_mode { "hyper" } else { "raw" });
    let raw_room: RawRoom = Default::default();
    let hyper_room: HyperRoom = Default::default();
    loop {
        let Ok((tcp, _)) = listener.accept().await else { continue };
        if hyper_mode {
            let room = hyper_room.clone();
            tokio::spawn(async move {
                let _ = tcp.set_nodelay(true);
                let svc = hyper::service::service_fn(move |req| hyper_handle(req, room.clone()));
                let mut b = auto::Builder::new(TokioExecutor::new());
                b.http1().pipeline_flush(true).half_close(true);
                let _ = b.serve_connection_with_upgrades(TokioIo::new(tcp), svc).await;
            });
        } else {
            tokio::spawn(raw_conn(tcp, raw_room.clone()));
        }
    }
}
