//! Pub/sub subscribers: a dedicated connection in subscribed mode whose messages are *pulled*
//! with `next()` (no callbacks are stored, §13.5).
//!
//! A reader task parses pushes: `message` / `pmessage` go to an unbounded queue that `next()`
//! drains, (un)subscribe confirmations go to an acknowledgement queue that `subscribe()` and
//! `unsubscribe()` wait on, so those return once the server has applied them (a message
//! published afterwards is guaranteed to arrive). `close()` makes pending and later `next()`
//! calls return "closed" (kind 0) and drops the connection.
//!
//! `RedisSubscriber` is a Copy struct in Velt, so its handle is a registry key: after `close()`
//! (through any copy) `next()` keeps returning "closed" and other calls fail with `EBADF`,
//! instead of reaching a freed connection.

use super::client::{bad_handle, ClientHandle, CLIENTS};
use super::connect::{self, Endpoint, Stream};
use super::error::RedisErr;
use super::resp::{encode_command, Parser, Value};
use super::url;
use futures_util::future::{select, AbortHandle, Either};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{mpsc, Mutex, Notify};
use velt_native::handles::{Key, Registry};

/// A received message (`channel` / `message`, plus `pattern` for pattern subscriptions).
pub struct Message {
    /// The channel it was published to.
    pub channel: String,
    /// The payload (invalid UTF-8 becomes U+FFFD).
    pub message: String,
    /// The matching pattern (a pattern subscription), else `""`.
    pub pattern: String,
}

type Event = Result<Message, RedisErr>;
type Ack = Result<(), RedisErr>;

/// The write side and the acknowledgements, locked together: one (un)subscribe at a time.
struct Control {
    wr: WriteHalf<Stream>,
    acks: mpsc::UnboundedReceiver<Ack>,
}

/// A subscribed connection.
pub struct SubObj {
    control: Mutex<Control>,
    events: Mutex<mpsc::UnboundedReceiver<Event>>,
    closed: AtomicBool,
    close_notify: Notify,
    reader: AbortHandle,
}

impl Drop for SubObj {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Opaque subscriber handle; in-flight calls keep their own `Arc<SubObj>`.
pub type SubHandle = Key<SubObj>;

/// Open subscribers.
static SUBSCRIBERS: Registry<SubObj> = Registry::new();

fn command_name(patterns: bool, subscribe: bool) -> &'static str {
    match (patterns, subscribe) {
        (false, true) => "SUBSCRIBE",
        (true, true) => "PSUBSCRIBE",
        (false, false) => "UNSUBSCRIBE",
        (true, false) => "PUNSUBSCRIBE",
    }
}

async fn open(ep: Endpoint, command: &str, names: Vec<Vec<u8>>) -> Result<SubObj, RedisErr> {
    let (rd, wr) = tokio::io::split(connect::open(&ep).await?);
    let (event_tx, events) = mpsc::unbounded_channel();
    let (ack_tx, acks) = mpsc::unbounded_channel();
    let reader = crate::spawn_abortable(read_loop(rd, event_tx, ack_tx));
    let obj = SubObj {
        control: Mutex::new(Control { wr, acks }),
        events: Mutex::new(events),
        closed: AtomicBool::new(false),
        close_notify: Notify::new(),
        reader,
    };
    change(&obj, command, names).await?;
    Ok(obj)
}

/// Send one (un)subscribe command and wait until the server confirmed every name.
async fn change(obj: &SubObj, command: &str, names: Vec<Vec<u8>>) -> Result<(), RedisErr> {
    if names.is_empty() {
        return Ok(());
    }
    let mut payload = vec![];
    let mut args: Vec<&[u8]> = vec![command.as_bytes()];
    args.extend(names.iter().map(Vec::as_slice));
    encode_command(&args, &mut payload);
    let mut control = obj.control.lock().await;
    connect::write_all(&mut control.wr, &payload).await?;
    for _ in &names {
        control
            .acks
            .recv()
            .await
            .unwrap_or(Err(RedisErr::closed()))?;
    }
    Ok(())
}

async fn read_loop(
    mut rd: ReadHalf<Stream>,
    events: mpsc::UnboundedSender<Event>,
    acks: mpsc::UnboundedSender<Ack>,
) {
    let e = read_pushes(&mut rd, &events, &acks).await.err();
    let e = e.unwrap_or_else(RedisErr::closed);
    let _ = acks.send(Err(e.clone()));
    let _ = events.send(Err(e));
}

async fn read_pushes(
    rd: &mut ReadHalf<Stream>,
    events: &mpsc::UnboundedSender<Event>,
    acks: &mpsc::UnboundedSender<Ack>,
) -> Result<(), RedisErr> {
    let (mut parser, mut buf, mut values) = (Parser::default(), Vec::new(), Vec::new());
    while connect::fill(rd, &mut buf).await? {
        let used = parser.feed(&buf, &mut values).map_err(RedisErr::protocol)?;
        buf.drain(..used);
        for v in values.drain(..) {
            dispatch(v, events, acks);
        }
    }
    Ok(())
}

fn text(v: Value) -> String {
    match v {
        Value::Bulk(b) => String::from_utf8_lossy(&b).into_owned(),
        Value::Status(s) => s,
        _ => String::new(),
    }
}

/// Route one push; anything unexpected is ignored.
fn dispatch(v: Value, events: &mpsc::UnboundedSender<Event>, acks: &mpsc::UnboundedSender<Ack>) {
    let items = match v {
        Value::Array(items) if !items.is_empty() => items,
        Value::Error(e) => {
            let _ = acks.send(Err(RedisErr::server(e)));
            return;
        }
        _ => return,
    };
    let mut items = items.into_iter().map(text);
    let kind = items.next().unwrap_or_default().to_ascii_lowercase();
    let mut next = || items.next().unwrap_or_default();
    match kind.as_str() {
        "message" => {
            let (channel, message) = (next(), next());
            let pattern = String::new();
            let _ = events.send(Ok(Message {
                channel,
                message,
                pattern,
            }));
        }
        "pmessage" => {
            let (pattern, channel, message) = (next(), next(), next());
            let _ = events.send(Ok(Message {
                channel,
                message,
                pattern,
            }));
        }
        "subscribe" | "psubscribe" | "unsubscribe" | "punsubscribe" => {
            let _ = acks.send(Ok(()));
        }
        _ => {}
    }
}

/// The next message; `None` once closed by `close()`.
async fn next_on(obj: &SubObj) -> Result<Option<Message>, RedisErr> {
    if obj.closed.load(Ordering::Acquire) {
        return Ok(None);
    }
    let mut events = obj.events.lock().await;
    let mut notified = pin!(obj.close_notify.notified());
    notified.as_mut().enable();
    // Checked after `enable()`, so a `close()` in between still wakes us.
    if obj.closed.load(Ordering::Acquire) {
        return Ok(None);
    }
    let received = match select(pin!(events.recv()), notified).await {
        Either::Left((received, _)) => received,
        Either::Right(_) => return Ok(None),
    };
    match received {
        Some(event) => event.map(Some),
        None if obj.closed.load(Ordering::Acquire) => Ok(None),
        None => Err(RedisErr::closed()),
    }
}

/// `subscribe(urlOrClient, names)`: a new connection (to the same place as client `c` when it
/// is non-zero, else to `url` trusting `ca`) subscribed to channels, or to glob patterns when
/// `patterns`; resolves once the server confirmed every name.
pub fn subscribe(
    c: ClientHandle,
    url: &str,
    ca: &[u8],
    names: Vec<Vec<u8>>,
    patterns: bool,
) -> impl Future<Output = Result<SubHandle, RedisErr>> + Send + use<> {
    let endpoint = match CLIENTS.get(c) {
        Some(conn) => Ok(conn.endpoint.clone()),
        None if c.bits() != 0 => Err(bad_handle()),
        None => url::parse(url)
            .map(|target| Endpoint::new(target, ca.to_vec()))
            .map_err(RedisErr::invalid),
    };
    let command = command_name(patterns, true);
    async move { Ok(SUBSCRIBERS.insert(open(endpoint?, command, names).await?)) }
}

/// `next()`: waits for the next message; `None` after `close()` (or for a closed handle). A
/// connection lost to the server fails with `ECONNRESET`.
pub async fn next(s: SubHandle) -> Result<Option<Message>, RedisErr> {
    match SUBSCRIBERS.get(s) {
        Some(obj) => next_on(&obj).await,
        None => Ok(None),
    }
}

/// `subscribe(names)` / `unsubscribe(names)` on a subscriber, once confirmed (`patterns`: glob
/// patterns). No names = no-op.
pub async fn change_names(
    s: SubHandle,
    names: Vec<Vec<u8>>,
    patterns: bool,
    subscribe: bool,
) -> Result<(), RedisErr> {
    let obj = SUBSCRIBERS.get(s).ok_or_else(bad_handle)?;
    change(&obj, command_name(patterns, subscribe), names).await
}

/// `close()`: pending and later `next()` calls return `None`; releases the handle (the
/// connection closes once no operation is in flight).
pub fn close(s: SubHandle) {
    if let Some(obj) = SUBSCRIBERS.remove(s) {
        obj.closed.store(true, Ordering::Release);
        obj.close_notify.notify_waiters();
        obj.reader.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::Bulk(s.as_bytes().to_vec())
    }

    #[test]
    fn routes_pushes() {
        let (etx, mut erx) = mpsc::unbounded_channel();
        let (atx, mut arx) = mpsc::unbounded_channel();
        let push = |items: Vec<Value>| dispatch(Value::Array(items), &etx, &atx);
        push(vec![bulk("subscribe"), bulk("a"), Value::Int(1)]);
        push(vec![bulk("message"), bulk("a"), bulk("hi")]);
        push(vec![bulk("pmessage"), bulk("a*"), bulk("ab"), bulk("yo")]);
        dispatch(Value::Error("ERR x".into()), &etx, &atx);
        assert_eq!(arx.try_recv().unwrap(), Ok(()));
        assert!(arx.try_recv().unwrap().is_err());
        let m = erx.try_recv().unwrap().unwrap();
        assert_eq!(
            (m.channel.as_str(), m.message.as_str(), m.pattern.as_str()),
            ("a", "hi", "")
        );
        let m = erx.try_recv().unwrap().unwrap();
        assert_eq!(
            (m.channel.as_str(), m.message.as_str(), m.pattern.as_str()),
            ("ab", "yo", "a*")
        );
    }
}
