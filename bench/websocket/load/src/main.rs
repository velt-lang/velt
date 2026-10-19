//! Load generator for bench/websocket (README.md). Prints one JSON object per run.
//!
//!   ws-load echo  <url> <conns> <secs> [size]   closed loop: each connection sends a message and
//!                                               waits for it to come back; round trips/s, latency
//!   ws-load bcast <url> <conns> <senders> <secs> each of <senders> connections sends a message
//!                                               and waits until it comes back through the room;
//!                                               every connection counts what it receives
//!   ws-load rate  <url> <conns> <msgs/s> <secs> fixed load: each connection sends on a timer
//!                                               (msgs/s spread evenly), waits for the echo;
//!                                               achieved rate and latency (compare server CPU)
//!   ws-load conns <url> <target> <hold secs>    opens up to <target> idle connections (stops at
//!                                               the first failures), holds them, then checks each
//!                                               still echoes
//!
//! Env THREADS: worker threads (default 4), so the load generator leaves the server most cores.
//! Env INFLIGHT (echo): messages each connection sends per write before reading the echoes
//! (default 1), so one client syscall covers many messages and the server becomes the limit.

use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(url: &str) -> Result<Ws, String> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| e.to_string())?;
    Ok(ws)
}

/// Opens `n` connections, at most 64 handshakes at a time; a refused or reset handshake (a full
/// listen backlog: macOS' somaxconn is 128) is retried up to 5 times.
async fn connect_all(url: &str, n: usize) -> Vec<Result<Ws, String>> {
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut tasks = Vec::with_capacity(n);
    for _ in 0..n {
        let url = url.to_string();
        let sem = sem.clone();
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            let mut r = connect(&url).await;
            for attempt in 1..=5 {
                if r.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
                r = connect(&url).await;
            }
            r
        }));
    }
    let mut out = Vec::with_capacity(n);
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

fn percentile(sorted: &[u32], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i] as f64 / 1000.0
}

async fn echo(url: &str, conns: usize, secs: f64, size: usize) {
    let socks: Vec<Ws> = connect_all(url, conns)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("connect failed: {e}"));
    let payload = "x".repeat(size);
    let inflight: usize = std::env::var("INFLIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let stop = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let mut tasks = Vec::new();
    for mut ws in socks {
        let payload = payload.clone();
        let stop = stop.clone();
        let errors = errors.clone();
        tasks.push(tokio::spawn(async move {
            let mut lat: Vec<u32> = Vec::with_capacity(1 << 16);
            'run: while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                // INFLIGHT messages in one write, then their echoes (one latency sample each).
                for _ in 0..inflight {
                    if ws.feed(Message::text(payload.clone())).await.is_err() {
                        errors.fetch_add(1, Ordering::Relaxed);
                        break 'run;
                    }
                }
                if ws.flush().await.is_err() {
                    errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                for _ in 0..inflight {
                    match ws.next().await {
                        Some(Ok(Message::Text(t2))) if t2.len() == payload.len() => {
                            lat.push(t.elapsed().as_micros() as u32)
                        }
                        _ => {
                            errors.fetch_add(1, Ordering::Relaxed);
                            break 'run;
                        }
                    }
                }
            }
            let _ = ws.close(None).await;
            lat
        }));
    }
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    stop.store(true, Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    all.sort_unstable();
    println!(
        "{{\"mode\":\"echo\",\"conns\":{conns},\"size\":{size},\"msgs\":{},\"rate\":{:.0},\"p50_ms\":{:.3},\"p99_ms\":{:.3},\"max_ms\":{:.3},\"errors\":{}}}",
        all.len(),
        all.len() as f64 / elapsed,
        percentile(&all, 0.50),
        percentile(&all, 0.99),
        percentile(&all, 1.0),
        errors.load(Ordering::Relaxed)
    );
}

async fn rate(url: &str, conns: usize, per_s: f64, secs: f64) {
    let socks: Vec<Ws> = connect_all(url, conns)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("connect failed: {e}"));
    let period = Duration::from_secs_f64(conns as f64 / per_s);
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    let start = Instant::now();
    let mut tasks = Vec::new();
    for (i, mut ws) in socks.into_iter().enumerate() {
        tasks.push(tokio::spawn(async move {
            // Stagger the connections over one period so the load is smooth.
            let offset = period.mul_f64(i as f64 / conns as f64);
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + offset, period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut lat: Vec<u32> = Vec::new();
            while Instant::now() < deadline {
                tick.tick().await;
                let t = Instant::now();
                if ws.send(Message::text("x".repeat(32))).await.is_err() {
                    break;
                }
                match ws.next().await {
                    Some(Ok(Message::Text(_))) => lat.push(t.elapsed().as_micros() as u32),
                    _ => break,
                }
            }
            let _ = ws.close(None).await;
            lat
        }));
    }
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    let elapsed = start.elapsed().as_secs_f64();
    all.sort_unstable();
    println!(
        "{{\"mode\":\"rate\",\"conns\":{conns},\"target\":{per_s},\"msgs\":{},\"rate\":{:.0},\"p50_ms\":{:.3},\"p99_ms\":{:.3},\"max_ms\":{:.3}}}",
        all.len(),
        all.len() as f64 / elapsed,
        percentile(&all, 0.50),
        percentile(&all, 0.99),
        percentile(&all, 1.0)
    );
}

async fn bcast(url: &str, conns: usize, senders: usize, secs: f64) {
    let socks: Vec<Ws> = connect_all(url, conns)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("connect failed: {e}"));
    // Let the server register every connection in the room before the first message.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stop = Arc::new(AtomicBool::new(false));
    let received = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let mut tasks = Vec::new();
    for (i, ws) in socks.into_iter().enumerate() {
        let stop = stop.clone();
        let received = received.clone();
        let sender = i < senders;
        tasks.push(tokio::spawn(async move {
            let (mut tx, mut rx) = ws.split();
            let me = format!("m{i:06}");
            let mut lat: Vec<u32> = Vec::new();
            let mut t = Instant::now();
            if sender {
                tx.send(Message::text(me.clone())).await.ok();
            }
            loop {
                let next = tokio::time::timeout(Duration::from_millis(200), rx.next()).await;
                let m = match next {
                    Ok(Some(Ok(Message::Text(m)))) => m,
                    Ok(Some(Ok(_))) => continue,
                    Ok(_) => break,
                    Err(_) if stop.load(Ordering::Relaxed) => break,
                    Err(_) => continue,
                };
                received.fetch_add(1, Ordering::Relaxed);
                if sender && m.as_str() == me {
                    lat.push(t.elapsed().as_micros() as u32);
                    if stop.load(Ordering::Relaxed) {
                        continue;
                    }
                    t = Instant::now();
                    if tx.send(Message::text(me.clone())).await.is_err() {
                        break;
                    }
                }
            }
            let _ = tx.close().await;
            lat
        }));
    }
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    stop.store(true, Ordering::Relaxed);
    let delivered_in_window = received.load(Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    all.sort_unstable();
    println!(
        "{{\"mode\":\"bcast\",\"conns\":{conns},\"senders\":{senders},\"broadcasts\":{},\"deliveries_per_s\":{:.0},\"p50_ms\":{:.3},\"p99_ms\":{:.3},\"max_ms\":{:.3}}}",
        all.len(),
        delivered_in_window as f64 / elapsed,
        percentile(&all, 0.50),
        percentile(&all, 0.99),
        percentile(&all, 1.0)
    );
}

async fn conns(url: &str, target: usize, hold: f64) {
    let start = Instant::now();
    let mut open: Vec<Ws> = Vec::new();
    let mut failed = 0usize;
    let mut first_error = String::new();
    // Batches of 1000, so a server that stops accepting is noticed without hammering it.
    while open.len() < target && failed == 0 {
        let n = (target - open.len()).min(1000);
        for r in connect_all(url, n).await {
            match r {
                Ok(ws) => open.push(ws),
                Err(e) => {
                    failed += 1;
                    if first_error.is_empty() {
                        first_error = e;
                    }
                }
            }
        }
        eprintln!("  {} connections open", open.len());
    }
    let connect_s = start.elapsed().as_secs_f64();
    println!("READY {}", open.len());
    tokio::time::sleep(Duration::from_secs_f64(hold)).await;
    // Every connection must still answer.
    let mut tasks = Vec::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(512));
    for mut ws in open {
        let sem = sem.clone();
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            if ws.send(Message::text("ping")).await.is_err() {
                return false;
            }
            let ok = matches!(
                tokio::time::timeout(Duration::from_secs(10), ws.next()).await,
                Ok(Some(Ok(Message::Text(_))))
            );
            drop(_p);
            // Keep the connection until every check is done.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = ws.close(None).await;
            ok
        }));
    }
    let n = tasks.len();
    let mut alive = 0;
    for t in tasks {
        if t.await.unwrap() {
            alive += 1;
        }
    }
    println!(
        "{{\"mode\":\"conns\",\"target\":{target},\"opened\":{n},\"alive\":{alive},\"failed\":{failed},\"connect_s\":{connect_s:.2},\"first_error\":{:?}}}",
        first_error
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let threads = std::env::var("THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();
    let num = |i: usize| -> f64 { a.get(i).and_then(|s| s.parse().ok()).expect("bad arguments; see the header of src/main.rs") };
    rt.block_on(async {
        match a.get(1).map(|s| s.as_str()) {
            Some("echo") => echo(&a[2], num(3) as usize, num(4), a.get(5).map_or(32, |_| num(5) as usize)).await,
            Some("bcast") => bcast(&a[2], num(3) as usize, num(4) as usize, num(5)).await,
            Some("rate") => rate(&a[2], num(3) as usize, num(4), num(5)).await,
            Some("conns") => conns(&a[2], num(3) as usize, num(4)).await,
            _ => eprintln!("usage: see the header of src/main.rs"),
        }
    });
}
