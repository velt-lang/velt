//! The exports src/native.vlt declares (`velt_redis__*`, docs/native_abi.md): thin wrappers
//! over [`crate::client`] and [`crate::pubsub`]. A client or subscriber handle is a registry
//! key (`u64`): a closed one fails with `EBADF`.

use crate::client::{self, ClientHandle};
use crate::error::RedisErr;
use crate::pubsub::{self, Message, SubHandle};
use crate::reply::{Nodes, RawReply};
use crate::resp::Value;
use std::borrow::Cow;
use std::future::Future;
use velt_native::handles::Key;
use velt_native::{export, Error, Str};

fn io<T>(r: Result<T, RedisErr>) -> Result<T, Error> {
    r.map_err(|e| e.to_error())
}

fn reply(r: Result<Value, RedisErr>) -> Result<RawReply, Error> {
    io(r).map(|v| RawReply::of(&v))
}

/// The elements of a Velt `string[]` as UTF-8, borrowed while the call lasts (a string with
/// lone surrogates is converted: one U+FFFD each, #377).
fn args(a: &[Str]) -> Vec<Cow<'_, [u8]>> {
    a.iter()
        .map(|s| match s.to_str() {
            Cow::Borrowed(t) => Cow::Borrowed(t.as_bytes()),
            Cow::Owned(t) => Cow::Owned(t.into_bytes()),
        })
        .collect()
}

fn owned_names(a: &[Str]) -> Vec<Vec<u8>> {
    args(a).into_iter().map(Cow::into_owned).collect()
}

/// `connect(url, { ca })` → `IoResult<u64>`.
#[export(async)]
async fn velt_redis__connect(url: String, ca: String) -> Result<u64, Error> {
    io(client::connect(&url, ca.into_bytes()).await.map(Key::bits))
}

/// `client.duplicate()` → `IoResult<u64>`.
#[export(async)]
async fn velt_redis__duplicate(c: u64) -> Result<u64, Error> {
    io(client::duplicate(Key::from_bits(c)).await.map(Key::bits))
}

/// `client.close()`.
#[export]
fn velt_redis__close(c: u64) {
    client::close(Key::from_bits(c))
}

/// `call(args)` → `IoResult<RawReply>`: encoded from the arguments now, sent by the future.
#[export(async)]
fn velt_redis__command(
    c: u64,
    a: &[Str],
) -> impl Future<Output = Result<RawReply, Error>> + Send + 'static {
    let c: ClientHandle = Key::from_bits(c);
    let f = client::command(c, client::encode(&args(a)));
    async move { reply(f.await) }
}

/// `pipeline.exec()` → `IoResult<RawReply>`.
#[export(async)]
fn velt_redis__pipeline(
    c: u64,
    a: &[Str],
    counts: &[u64],
    atomic: bool,
) -> impl Future<Output = Result<RawReply, Error>> + Send + 'static {
    let payload = client::encode_batch(&args(a), counts, atomic);
    let f = client::pipeline(Key::from_bits(c), payload, counts.len(), atomic);
    async move { reply(f.await) }
}

/// The package's `RawMessage`: kind 0 = closed (no more messages), 1 = message, 2 = pattern message
/// (`pattern` set).
#[derive(velt_native::Value)]
#[repr(C)]
pub struct RawMessage {
    /// 0 closed, 1 message, 2 pattern message.
    pub kind: u32,
    /// The channel it was published to.
    pub channel: Str,
    /// The payload.
    pub message: Str,
    /// The matching pattern (kind 2), else `""`.
    pub pattern: Str,
}

fn message(m: Option<Message>) -> RawMessage {
    let Some(m) = m else {
        return RawMessage {
            kind: 0,
            channel: Str::new(""),
            message: Str::new(""),
            pattern: Str::new(""),
        };
    };
    RawMessage {
        kind: if m.pattern.is_empty() { 1 } else { 2 },
        channel: Str::from(m.channel),
        message: Str::from(m.message),
        pattern: Str::from(m.pattern),
    }
}

/// `subscribe(urlOrClient, names)` → `IoResult<u64>` (see [`pubsub::subscribe`]).
#[export(async)]
fn velt_redis__subscribe(
    c: u64,
    url: &str,
    ca: &str,
    names: &[Str],
    patterns: bool,
) -> impl Future<Output = Result<u64, Error>> + Send + 'static {
    let f = pubsub::subscribe(
        Key::from_bits(c),
        url,
        ca.as_bytes(),
        owned_names(names),
        patterns,
    );
    async move { io(f.await.map(Key::bits)) }
}

/// `next()` → `IoResult<RawMessage>`: kind 0 after `close()`.
#[export(async)]
async fn velt_redis__sub_next(s: u64) -> Result<RawMessage, Error> {
    let s: SubHandle = Key::from_bits(s);
    io(pubsub::next(s).await.map(message))
}

/// `subscribe(names)` / `unsubscribe(names)` on a subscriber → `IoStatus` once confirmed.
#[export(async)]
fn velt_redis__sub_change(
    s: u64,
    names: &[Str],
    patterns: bool,
    subscribe: bool,
) -> impl Future<Output = Result<(), Error>> + Send + 'static {
    let f = pubsub::change_names(Key::from_bits(s), owned_names(names), patterns, subscribe);
    async move { io(f.await) }
}

/// `close()` a subscriber.
#[export]
fn velt_redis__sub_close(s: u64) {
    pubsub::close(Key::from_bits(s))
}
