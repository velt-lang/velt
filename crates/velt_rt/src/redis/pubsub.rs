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

use super::client::{ClientHandle, CLIENTS};
use super::connect::{self, Endpoint, Stream};
use super::error::RedisErr;
use super::resp::{encode_command, Parser, Value};
use super::{str_args, url};
use crate::net::tcp::text_arg;
use crate::registry::{Key, Registry};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use futures_util::future::{select, Either};
use std::borrow::Cow;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::AbortHandle;

/// A received message (`channel` / `message`, plus `pattern` for pattern subscriptions).
struct Message {
    channel: String,
    message: String,
    pattern: String,
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

/// `{ u32 kind; u32 pad; VeltStr channel; VeltStr message; VeltStr pattern; }` (80 bytes):
/// kind 0 = closed (no more messages), 1 = message, 2 = pattern message (`pattern` set).
#[repr(C)]
pub struct VeltRedisMessage {
    /// 0 closed, 1 message, 2 pattern message.
    pub kind: u32,
    /// Always 0.
    pub pad: u32,
    /// The channel it was published to.
    pub channel: VeltStr,
    /// The payload (invalid UTF-8 becomes U+FFFD).
    pub message: VeltStr,
    /// The matching pattern (kind 2), else `""`.
    pub pattern: VeltStr,
}

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
    let reader = crate::task::runtime::handle()
        .spawn(read_loop(rd, event_tx, ack_tx))
        .abort_handle();
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
async fn next(obj: &SubObj) -> Result<Option<Message>, RedisErr> {
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

fn message_result(r: Result<Option<Message>, RedisErr>) -> IoResult<VeltRedisMessage> {
    let text = |s: String| VeltStr::from_vec(s.into_bytes());
    match r {
        Err(e) => IoResult::err(e.to_velt()),
        Ok(m) => {
            let (kind, m) = match m {
                None => (0, None),
                Some(m) if m.pattern.is_empty() => (1, Some(m)),
                Some(m) => (2, Some(m)),
            };
            let m = m.unwrap_or(Message {
                channel: String::new(),
                message: String::new(),
                pattern: String::new(),
            });
            IoResult::ok(VeltRedisMessage {
                kind,
                pad: 0,
                channel: text(m.channel),
                message: text(m.message),
                pattern: text(m.pattern),
            })
        }
    }
}

unsafe fn owned_names(names: *const VeltStrArray) -> Vec<Vec<u8>> {
    str_args(names).into_iter().map(Cow::into_owned).collect()
}

/// `subscribe(urlOrClient, names)` → `IoResult<SubHandle>`: a new connection (to the same place
/// as client `c` when it is non-zero, else to `url` trusting `ca`) subscribed to channels, or to
/// glob patterns when `patterns`; resolves once the server confirmed every name.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_subscribe(
    c: ClientHandle,
    url: *const VeltStr,
    ca: *const VeltStr,
    names: *const VeltStrArray,
    patterns: u8,
) -> *mut VeltFut {
    let endpoint = match CLIENTS.get(c) {
        Some(conn) => Ok(conn.endpoint.clone()),
        None if c.bits() != 0 => Err(RedisErr {
            code: crate::result::code::BAD_HANDLE,
            message: "handle is closed".to_string(),
        }),
        None => url::parse(&text_arg(url))
            .map(|target| Endpoint {
                target,
                ca: (*ca).as_bytes().to_vec(),
            })
            .map_err(RedisErr::invalid),
    };
    let names = owned_names(names);
    let command = command_name(patterns != 0, true);
    new_leaf(async move {
        let r = match endpoint {
            Ok(ep) => open(ep, command, names).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(obj) => IoResult::ok(SUBSCRIBERS.insert(obj)),
            Err(e) => IoResult::err(e.to_velt()),
        }
    })
}

/// `next()` → `IoResult<VeltRedisMessage>`: waits for the next message; kind 0 after `close()`.
/// A connection lost to the server fails with `ECONNRESET`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_sub_next(s: SubHandle) -> *mut VeltFut {
    let obj = SUBSCRIBERS.get(s);
    new_leaf(async move {
        match obj {
            Some(obj) => message_result(next(&obj).await),
            None => message_result(Ok(None)),
        }
    })
}

/// `subscribe(names)` / `unsubscribe(names)` on a subscriber → `IoResult<()>` once confirmed
/// (`subscribe` 1 = add, 0 = remove; `patterns` 1 = glob patterns). No names = no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_sub_change(
    s: SubHandle,
    names: *const VeltStrArray,
    patterns: u8,
    subscribe: u8,
) -> *mut VeltFut {
    let names = owned_names(names);
    let command = command_name(patterns != 0, subscribe != 0);
    SUBSCRIBERS.op::<()>(s, |obj| {
        new_leaf(async move {
            match change(&obj, command, names).await {
                Ok(()) => IoResult::ok(()),
                Err(e) => IoResult::err(e.to_velt()),
            }
        })
    })
}

/// `close()`: pending and later `next()` calls return kind 0; releases the handle (the
/// connection closes once no operation is in flight).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_sub_close(s: SubHandle) {
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
