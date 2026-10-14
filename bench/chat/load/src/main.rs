//! Load generator for chatbench (../SPEC.md). Prints one JSON object per run.
//!
//!   chat-load join   <base> <conns> <rooms>                connect as fast as possible
//!   chat-load steady <base> <conns> <rooms> <rate> <secs>  `say` at <rate>/s in total
//!   chat-load idle   <base> <conns> <rooms> <secs>         connected clients at rest
//!
//! <base> is `ws://host:port`, or `wss://host:port` for TLS (the server certificate is verified
//! against the CA certificates in the PEM file named by env CA). Env THREADS: worker threads (default 4). `steady` puts the sender's
//! clock (microseconds since the run started) in the text; every receiver of the broadcast
//! measures the latency to its own receipt. `READY` is printed once every client is connected.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn token(user: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"chatbench-secret").unwrap();
    mac.update(user.as_bytes());
    format!("{user}.{}", hex::encode(mac.finalize().into_bytes()))
}

/// The TLS client configuration for `wss://` bases, set once in `main`.
static TLS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();

/// A rustls client trusting only the CA certificates in the PEM file `ca`.
fn tls_config(ca: &str) -> Result<Arc<rustls::ClientConfig>, String> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::CertificateDer;
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(ca).map_err(|e| format!("CA {ca}: {e}"))? {
        roots.add(cert.map_err(|e| format!("CA {ca}: {e}"))?).map_err(|e| format!("CA {ca}: {e}"))?;
    }
    if roots.is_empty() {
        return Err(format!("CA {ca}: no certificates"));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Opens a WebSocket: plain for `ws://`, rustls with the CA from env CA for `wss://`.
async fn connect(url: &str) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let r = match TLS.get() {
        Some(config) if url.starts_with("wss://") => {
            let connector = Connector::Rustls(config.clone());
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector)).await
        }
        _ => tokio_tungstenite::connect_async(url).await,
    };
    r.map(|(ws, _)| ws)
}

/// Connects client `i` and waits for its welcome; the time from start to welcome.
async fn join(base: &str, i: usize, rooms: usize) -> Result<(Ws, Duration), String> {
    let user = format!("u{i}");
    let url = format!("{base}/ws?token={}&room=r{}", token(&user), i % rooms);
    let t = Instant::now();
    let mut last = String::new();
    for attempt in 0..6u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(25 * attempt)).await;
        }
        match connect(url.as_str()).await {
            Ok(mut ws) => match ws.next().await {
                Some(Ok(Message::Text(m))) if m.as_str().starts_with("{\"type\":\"welcome\"") => {
                    return Ok((ws, t.elapsed()))
                }
                other => return Err(format!("no welcome: {other:?}")),
            },
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Connects `n` clients, at most 64 handshakes at a time.
async fn join_all(base: &str, n: usize, rooms: usize) -> (Vec<Ws>, Vec<u32>, usize, String) {
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut tasks = Vec::with_capacity(n);
    for i in 0..n {
        let (base, sem) = (base.to_string(), sem.clone());
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            join(&base, i, rooms).await
        }));
    }
    let (mut socks, mut lat, mut failed, mut first) = (Vec::new(), Vec::new(), 0, String::new());
    for t in tasks {
        match t.await.unwrap() {
            Ok((ws, d)) => {
                socks.push(ws);
                lat.push(d.as_micros() as u32);
            }
            Err(e) => {
                failed += 1;
                if first.is_empty() {
                    first = e;
                }
            }
        }
    }
    (socks, lat, failed, first)
}

fn pct(sorted: &[u32], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize] as f64 / 1000.0
}

/// The sender clock (µs) in a broadcast's text: `"text":"<micros> ...`.
fn sent_micros(m: &str) -> Option<u64> {
    let at = m.find("\"text\":\"")? + 8;
    let rest = &m[at..];
    let end = rest.find(' ')?;
    rest[..end].parse().ok()
}

async fn reader(
    mut rx: SplitStream<Ws>,
    start: Instant,
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
) -> Vec<u32> {
    let mut lat = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_millis(200), rx.next()).await;
        let m = match next {
            Ok(Some(Ok(Message::Text(m)))) => m,
            Ok(Some(Ok(_))) => continue,
            Ok(_) => {
                if !stop.load(Ordering::Relaxed) {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
            Err(_) if stop.load(Ordering::Relaxed) => break,
            Err(_) => continue,
        };
        if m.as_str().starts_with("{\"type\":\"message\"") {
            delivered.fetch_add(1, Ordering::Relaxed);
            if let Some(sent) = sent_micros(m.as_str()) {
                let now = start.elapsed().as_micros() as u64;
                lat.push(now.saturating_sub(sent) as u32);
            }
        } else if m.as_str().starts_with("{\"type\":\"error\"") {
            errors.fetch_add(1, Ordering::Relaxed);
        }
    }
    lat
}

async fn sender(
    mut tx: SplitSink<Ws, Message>,
    i: usize,
    period: Duration,
    offset: Duration,
    start: Instant,
    end: Instant,
    sent: Arc<AtomicU64>,
) -> SplitSink<Ws, Message> {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + offset, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // About 80 characters of chat text after the clock.
    let filler = " hello from the load generator, this is a typical short chat line ok";
    loop {
        tick.tick().await;
        if Instant::now() >= end {
            break;
        }
        let micros = start.elapsed().as_micros();
        let msg = format!("{{\"type\":\"say\",\"text\":\"{micros} u{i}{filler}\"}}");
        if tx.send(Message::text(msg)).await.is_err() {
            break;
        }
        sent.fetch_add(1, Ordering::Relaxed);
    }
    tx
}

async fn steady(base: &str, conns: usize, rooms: usize, rate: f64, secs: f64) {
    let (socks, _, failed, first) = join_all(base, conns, rooms).await;
    println!("READY {}", socks.len());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n = socks.len();
    let start = Instant::now();
    let end = start + Duration::from_secs_f64(secs);
    let period = Duration::from_secs_f64(n as f64 / rate);
    let stop = Arc::new(AtomicBool::new(false));
    let (delivered, errors, sent) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let (mut readers, mut senders) = (Vec::new(), Vec::new());
    for (i, ws) in socks.into_iter().enumerate() {
        let (tx, rx) = ws.split();
        readers.push(tokio::spawn(reader(rx, start, stop.clone(), delivered.clone(), errors.clone())));
        let offset = period.mul_f64(i as f64 / n as f64);
        senders.push(tokio::spawn(sender(tx, i, period, offset, start, end, sent.clone())));
    }
    tokio::time::sleep_until((end).into()).await;
    let in_window = delivered.load(Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    // Let in-flight broadcasts arrive, then stop the readers.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    stop.store(true, Ordering::Relaxed);
    let mut txs = Vec::new();
    for s in senders {
        txs.push(s.await.unwrap());
    }
    let mut all = Vec::new();
    for r in readers {
        all.extend(r.await.unwrap());
    }
    for mut tx in txs {
        let _ = tx.close().await;
    }
    all.sort_unstable();
    let sent = sent.load(Ordering::Relaxed);
    let expected = sent as f64 * (n as f64 / rooms as f64);
    println!(
        "{{\"mode\":\"steady\",\"conns\":{n},\"rooms\":{rooms},\"rate\":{rate},\"failed\":{failed},\"sent\":{sent},\"sent_per_s\":{:.0},\"deliveries_per_s\":{:.0},\"delivered_ratio\":{:.3},\"p50_ms\":{:.2},\"p99_ms\":{:.2},\"p999_ms\":{:.2},\"max_ms\":{:.2},\"errors\":{},\"first_error\":{:?}}}",
        sent as f64 / elapsed,
        in_window as f64 / elapsed,
        all.len() as f64 / expected.max(1.0),
        pct(&all, 0.50),
        pct(&all, 0.99),
        pct(&all, 0.999),
        pct(&all, 1.0),
        errors.load(Ordering::Relaxed),
        first
    );
}

async fn join_mode(base: &str, conns: usize, rooms: usize) {
    let t = Instant::now();
    let (socks, mut lat, failed, first) = join_all(base, conns, rooms).await;
    let secs = t.elapsed().as_secs_f64();
    lat.sort_unstable();
    println!(
        "{{\"mode\":\"join\",\"conns\":{conns},\"rooms\":{rooms},\"joined\":{},\"failed\":{failed},\"secs\":{secs:.2},\"joins_per_s\":{:.0},\"p50_ms\":{:.2},\"p99_ms\":{:.2},\"first_error\":{:?}}}",
        socks.len(),
        socks.len() as f64 / secs,
        pct(&lat, 0.5),
        pct(&lat, 0.99),
        first
    );
    for mut ws in socks {
        let _ = ws.close(None).await;
    }
}

async fn idle(base: &str, conns: usize, rooms: usize, secs: f64) {
    let (socks, _, failed, first) = join_all(base, conns, rooms).await;
    println!("READY {}", socks.len());
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    let n = socks.len();
    for mut ws in socks {
        let _ = ws.close(None).await;
    }
    println!("{{\"mode\":\"idle\",\"conns\":{conns},\"rooms\":{rooms},\"joined\":{n},\"failed\":{failed},\"first_error\":{first:?}}}");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let threads = std::env::var("THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    if a.get(2).is_some_and(|b| b.starts_with("wss://")) {
        let ca = std::env::var("CA").unwrap_or_else(|_| {
            eprintln!("a wss:// base needs env CA (a PEM file with the CA certificate)");
            std::process::exit(2)
        });
        match tls_config(&ca) {
            Ok(config) => TLS.set(config).unwrap(),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2)
            }
        }
    }
    let num = |i: usize| -> f64 { a.get(i).and_then(|s| s.parse().ok()).expect("bad arguments; see src/main.rs") };
    rt.block_on(async {
        match a.get(1).map(|s| s.as_str()) {
            Some("join") => join_mode(&a[2], num(3) as usize, num(4) as usize).await,
            Some("steady") => steady(&a[2], num(3) as usize, num(4) as usize, num(5), num(6)).await,
            Some("idle") => idle(&a[2], num(3) as usize, num(4) as usize, num(5)).await,
            _ => eprintln!("usage: see src/main.rs"),
        }
    });
}
