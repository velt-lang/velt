//! The multiplexed client connection: one socket shared by every task that uses a client.
//!
//! Callers hand a request (encoded commands + the number of replies they produce) to the
//! client's driver task, which queues the caller's reply slot and writes the bytes, batching
//! whatever requests are waiting into one write. A reader task parses replies and completes the
//! queued slots in order (Redis answers in request order). So concurrent commands from many tasks
//! are pipelined on one connection, like ioredis' single connection or the `redis` crate's
//! `MultiplexedConnection`.
//!
//! When the connection fails, the requests already written fail with that error (they may have
//! run; resending could run them twice). The driver then reconnects, lazily, when the next
//! request arrives: with backoff (`connect::reconnect`), carrying that request and every one
//! queued meanwhile over to the new connection. If reconnecting fails for its whole window, the
//! waiting requests fail with the connect error and the next request starts over. Dropping the
//! last `Conn` stops the driver and the reader.

use super::connect::{self, Endpoint, Stream};
use super::error::RedisErr;
use super::resp::{Parser, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

/// What one request resolves to: its replies, in order.
type Replies = Result<Vec<Value>, RedisErr>;

/// Requests waiting for the writer are batched up to about this many bytes per write.
const MAX_BATCH: usize = 256 * 1024;

struct Request {
    payload: Vec<u8>,
    replies: usize,
    done: oneshot::Sender<Replies>,
}

impl Request {
    fn fail(self, e: &RedisErr) {
        let _ = self.done.send(Err(e.clone()));
    }
}

struct Pending {
    want: usize,
    got: Vec<Value>,
    done: oneshot::Sender<Replies>,
}

/// One connection's reply slots in request order, and the error that ended it (if it has).
#[derive(Default)]
struct Queue {
    failed: Option<RedisErr>,
    pending: VecDeque<Pending>,
}

type SharedQueue = Arc<Mutex<Queue>>;

fn lock(q: &SharedQueue) -> MutexGuard<'_, Queue> {
    q.lock().unwrap_or_else(|e| e.into_inner())
}

/// An open multiplexed connection.
pub struct Conn {
    /// How it was opened (for `duplicate()`, `subscribe(client, …)` and reconnecting).
    pub endpoint: Endpoint,
    requests: mpsc::UnboundedSender<Request>,
    driver: AbortHandle,
    /// Connections the reader saw end (each failed its queue first).
    lost: Arc<AtomicU64>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        // In-flight commands hold their own `Arc<Conn>`, so nothing is waiting on the driver.
        self.driver.abort();
    }
}

impl Conn {
    /// Connect, run the handshake and start the driver.
    pub async fn open(endpoint: Endpoint) -> Result<Conn, RedisErr> {
        let stream = connect::open(&endpoint).await?;
        let (requests, rx) = mpsc::unbounded_channel();
        let lost = Arc::new(AtomicU64::new(0));
        let driver = crate::task::runtime::handle()
            .spawn(drive(endpoint.clone(), stream, rx, lost.clone()))
            .abort_handle();
        Ok(Conn {
            endpoint,
            requests,
            driver,
            lost,
        })
    }

    /// How many connections have ended so far: a request sent after that goes to a new one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn connections_lost(&self) -> u64 {
        self.lost.load(Ordering::Acquire)
    }

    /// Send already encoded commands producing `replies` replies and wait for them.
    pub async fn send(&self, payload: Vec<u8>, replies: usize) -> Replies {
        let (done, rx) = oneshot::channel();
        let req = Request {
            payload,
            replies,
            done,
        };
        self.requests.send(req).map_err(|_| RedisErr::closed())?;
        rx.await.map_err(|_| RedisErr::closed())?
    }
}

/// How a connection ended.
enum Ended {
    /// Every `Conn` was dropped.
    Released,
    /// The connection failed; the request (not yet written) goes to the next one.
    Failed(Option<Request>),
}

/// The client's lifetime: serve connections, reconnecting on demand (module docs).
async fn drive(
    endpoint: Endpoint,
    first: Stream,
    mut rx: mpsc::UnboundedReceiver<Request>,
    lost: Arc<AtomicU64>,
) {
    let mut stream = Some(first);
    let mut carried = None;
    loop {
        let s = match stream.take() {
            Some(s) => s,
            None => {
                let next = match carried.take() {
                    Some(req) => req,
                    None => match rx.recv().await {
                        Some(req) => req,
                        None => return,
                    },
                };
                match connect::reconnect(&endpoint).await {
                    Ok(s) => {
                        carried = Some(next);
                        s
                    }
                    Err(e) => {
                        next.fail(&e);
                        while let Ok(req) = rx.try_recv() {
                            req.fail(&e);
                        }
                        continue;
                    }
                }
            }
        };
        match serve(s, &mut rx, carried.take(), &lost).await {
            Ended::Released => return,
            Ended::Failed(unsent) => carried = unsent,
        }
    }
}

/// Run one connection: a reader task delivers replies while this task writes requests.
async fn serve(
    stream: Stream,
    rx: &mut mpsc::UnboundedReceiver<Request>,
    first: Option<Request>,
    lost: &Arc<AtomicU64>,
) -> Ended {
    let (rd, wr) = tokio::io::split(stream);
    let queue = SharedQueue::default();
    let reader = crate::task::runtime::handle().spawn(read_loop(rd, queue.clone(), lost.clone()));
    let ended = write_loop(wr, rx, &queue, first).await;
    reader.abort();
    ended
}

async fn write_loop(
    mut wr: WriteHalf<Stream>,
    rx: &mut mpsc::UnboundedReceiver<Request>,
    queue: &SharedQueue,
    mut next: Option<Request>,
) -> Ended {
    let mut batch = Vec::new();
    loop {
        let req = match next.take() {
            Some(req) => req,
            None => match rx.recv().await {
                Some(req) => req,
                None => {
                    let _ = wr.shutdown().await;
                    return Ended::Released;
                }
            },
        };
        batch.clear();
        if let Err(unsent) = enqueue(queue, req, &mut batch) {
            return Ended::Failed(Some(unsent));
        }
        while batch.len() < MAX_BATCH {
            let Ok(req) = rx.try_recv() else { break };
            if let Err(unsent) = enqueue(queue, req, &mut batch) {
                next = Some(unsent);
                break;
            }
        }
        if !batch.is_empty() {
            if let Err(e) = connect::write_all(&mut wr, &batch).await {
                fail(queue, e);
                return Ended::Failed(next);
            }
        }
        if next.is_some() {
            return Ended::Failed(next);
        }
    }
}

/// Queue the reply slot before the bytes go out, so the reply always finds it. A request for a
/// connection that already failed comes back unsent.
fn enqueue(queue: &SharedQueue, req: Request, batch: &mut Vec<u8>) -> Result<(), Request> {
    let mut q = lock(queue);
    if q.failed.is_some() {
        return Err(req);
    }
    if req.replies == 0 {
        let _ = req.done.send(Ok(vec![]));
        return Ok(());
    }
    q.pending.push_back(Pending {
        want: req.replies,
        got: Vec::with_capacity(req.replies),
        done: req.done,
    });
    batch.extend_from_slice(&req.payload);
    Ok(())
}

async fn read_loop(mut rd: ReadHalf<Stream>, queue: SharedQueue, lost: Arc<AtomicU64>) {
    let e = read_replies(&mut rd, &queue).await.err();
    fail(&queue, e.unwrap_or_else(RedisErr::closed));
    lost.fetch_add(1, Ordering::Release);
}

/// Deliver replies until the stream ends (`Ok`) or fails.
async fn read_replies(rd: &mut ReadHalf<Stream>, queue: &SharedQueue) -> Result<(), RedisErr> {
    let (mut parser, mut buf, mut values) = (Parser::default(), Vec::new(), Vec::new());
    while connect::fill(rd, &mut buf).await? {
        let used = parser.feed(&buf, &mut values).map_err(RedisErr::protocol)?;
        buf.drain(..used);
        deliver(queue, &mut values)?;
    }
    Ok(())
}

fn deliver(queue: &SharedQueue, values: &mut Vec<Value>) -> Result<(), RedisErr> {
    let mut q = lock(queue);
    for v in values.drain(..) {
        let Some(p) = q.pending.front_mut() else {
            return Err(RedisErr::protocol("unexpected reply".to_string()));
        };
        p.got.push(v);
        if p.got.len() == p.want {
            let p = q.pending.pop_front().expect("ICE: front slot exists");
            let _ = p.done.send(Ok(p.got));
        }
    }
    Ok(())
}

/// End the connection: fail every queued slot; later requests go to the next connection.
fn fail(queue: &SharedQueue, e: RedisErr) {
    let mut q = lock(queue);
    let e = q.failed.get_or_insert(e).clone();
    for p in q.pending.drain(..) {
        let _ = p.done.send(Err(e.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::super::url;
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A fake server that answers each `PING` with `:n` (its count so far on that connection),
    /// closes a connection after `limit` commands, and accepts `connections` connections (it
    /// stops listening when it accepts the last one).
    async fn fake_server(limit: usize, connections: usize) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut l = Some(l);
            for i in 0..connections {
                let (mut s, _) = l.as_ref().unwrap().accept().await.unwrap();
                if i + 1 == connections {
                    l = None;
                }
                let (mut seen, mut buf) = (0, vec![0u8; 4096]);
                while seen < limit {
                    let n = s.read(&mut buf).await.unwrap();
                    let pings = buf[..n].windows(4).filter(|w| w == b"PING").count();
                    let mut out = String::new();
                    for _ in 0..pings.min(limit - seen) {
                        seen += 1;
                        out.push_str(&format!(":{seen}\r\n"));
                    }
                    s.write_all(out.as_bytes()).await.unwrap();
                }
            }
        });
        port
    }

    fn ping(n: usize) -> Vec<u8> {
        let mut p = vec![];
        for _ in 0..n {
            super::super::resp::encode_command(&["PING"], &mut p);
        }
        p
    }

    /// Wait until `conn` saw `n` connections end (a hang guard of a minute, not a time limit).
    async fn lost(conn: &Conn, n: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while conn.connections_lost() < n {
            assert!(std::time::Instant::now() < deadline, "the connection never ended");
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    async fn open(port: u16) -> Conn {
        let target = url::parse(&format!("redis://127.0.0.1:{port}")).unwrap();
        Conn::open(Endpoint { target, ca: vec![] }).await.unwrap()
    }

    #[test]
    fn replies_arrive_in_order_and_a_lost_connection_is_reopened() {
        crate::task::runtime::runtime().block_on(async {
            let port = fake_server(3, 2).await;
            let conn = open(port).await;
            assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
            assert_eq!(
                conn.send(ping(2), 2).await,
                Ok(vec![Value::Int(2), Value::Int(3)])
            );
            // The server closed the first connection; the next command gets a new one.
            lost(&conn, 1).await;
            assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
        });
    }

    #[test]
    fn requests_fail_once_reconnecting_gives_up() {
        crate::task::runtime::runtime().block_on(async {
            let port = fake_server(1, 1).await;
            let conn = open(port).await;
            assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
            lost(&conn, 1).await;
            let e = conn.send(ping(1), 1).await.unwrap_err();
            assert_eq!(e.code, crate::result::code::CONNECTION_REFUSED);
        });
    }
}
