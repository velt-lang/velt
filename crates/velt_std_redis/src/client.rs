//! Client operations: connect, duplicate, close, single commands and pipelines (plain or
//! `MULTI`/`EXEC`), all on the multiplexed connection of `multiplex.rs`.
//!
//! `RedisClient` is a Copy struct in Velt, so a client handle is a registry key: after `close()`
//! through one copy, every copy fails with `EBADF` instead of reaching a freed connection.

use super::connect::Endpoint;
use super::error::RedisErr;
use super::multiplex::Conn;
use super::reply::{self, Flat};
use super::resp::encode_command;
use super::url;
use std::future::Future;
use std::sync::Arc;
use velt_native::handles::{Key, Registry};

/// Opaque client handle; in-flight commands keep their own `Arc<Conn>`.
pub type ClientHandle = Key<Conn>;

/// Open clients.
pub(super) static CLIENTS: Registry<Conn> = Registry::new();

/// A closed (or null) handle.
pub(super) fn bad_handle() -> RedisErr {
    RedisErr {
        code: velt_native::code::BAD_HANDLE,
        message: "handle is closed".to_string(),
    }
}

fn conn(c: ClientHandle) -> Result<Arc<Conn>, RedisErr> {
    CLIENTS.get(c).ok_or_else(bad_handle)
}

/// `connect(url, { ca })`: `redis://` or `rediss://` (TLS, also trusting the PEM CAs in `ca`,
/// empty = none), with `AUTH` and `SELECT` from the URL.
pub async fn connect(url: &str, ca: Vec<u8>) -> Result<ClientHandle, RedisErr> {
    let target = url::parse(url).map_err(RedisErr::invalid)?;
    Ok(CLIENTS.insert(Conn::open(Endpoint::new(target, ca)).await?))
}

/// `client.duplicate()`: a new connection opened like `c`.
pub async fn duplicate(c: ClientHandle) -> Result<ClientHandle, RedisErr> {
    let endpoint = conn(c)?.endpoint.clone();
    Ok(CLIENTS.insert(Conn::open(endpoint).await?))
}

/// `client.close()`: release the handle (through any copy; closing twice is a no-op); the
/// connection closes once no command is in flight.
pub fn close(c: ClientHandle) {
    CLIENTS.remove(c);
}

/// One command (`args[0]` is its name) encoded for [`command`]; empty for no arguments.
pub fn encode<A: AsRef<[u8]>>(args: &[A]) -> Vec<u8> {
    let mut payload = vec![];
    if !args.is_empty() {
        encode_command(args, &mut payload);
    }
    payload
}

/// `call(args)`: one command, `payload` from [`encode`]. An error reply fails with code 100
/// (§14.12).
pub fn command(
    c: ClientHandle,
    payload: Vec<u8>,
) -> impl Future<Output = Result<Flat, RedisErr>> + Send + use<> {
    let conn = conn(c);
    async move {
        let conn = conn?;
        if payload.is_empty() {
            return Err(RedisErr::invalid("empty Redis command".to_string()));
        }
        let mut replies = conn.send(payload, 1).await?;
        reply::single(replies.pop().expect("ICE: one reply per command"))
    }
}

/// Encode `args` split into commands of `counts[i]` arguments each.
fn encode_pipeline<A: AsRef<[u8]>>(
    args: &[A],
    counts: &[u64],
    out: &mut Vec<u8>,
) -> Result<(), RedisErr> {
    let mut at = 0usize;
    for &n in counts {
        let n = n as usize;
        if n == 0 || at + n > args.len() {
            return Err(RedisErr::invalid("malformed Redis pipeline".to_string()));
        }
        encode_command(&args[at..at + n], out);
        at += n;
    }
    if at != args.len() {
        return Err(RedisErr::invalid("malformed Redis pipeline".to_string()));
    }
    Ok(())
}

/// A pipeline's commands (`args` split by `counts`) encoded for [`pipeline`], inside
/// `MULTI`/`EXEC` when `atomic`.
pub fn encode_batch<A: AsRef<[u8]>>(
    args: &[A],
    counts: &[u64],
    atomic: bool,
) -> Result<Vec<u8>, RedisErr> {
    let mut payload = vec![];
    if atomic {
        encode_command(&["MULTI"], &mut payload);
    }
    encode_pipeline(args, counts, &mut payload)?;
    if atomic {
        encode_command(&["EXEC"], &mut payload);
    }
    Ok(payload)
}

/// `pipeline.exec()`: `commands` commands (`payload` from [`encode_batch`]) sent together; the
/// reply is an array with one node per command (error replies as ERROR nodes). With `atomic`,
/// they run in `MULTI`/`EXEC` and the result is `EXEC`'s array (an aborted transaction fails
/// with code 100).
pub fn pipeline(
    c: ClientHandle,
    payload: Result<Vec<u8>, RedisErr>,
    commands: usize,
    atomic: bool,
) -> impl Future<Output = Result<Flat, RedisErr>> + Send + use<> {
    let replies = commands + if atomic { 2 } else { 0 };
    let conn = conn(c);
    async move {
        let conn = conn?;
        let v = conn.send(payload?, replies).await?;
        if atomic {
            reply::transaction(v)
        } else {
            Ok(reply::pipeline(v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_pipelines() {
        let args: Vec<&[u8]> = vec![b"GET", b"a", b"PING"];
        let mut out = vec![];
        encode_pipeline(&args, &[2, 1], &mut out).unwrap();
        assert_eq!(out, b"*2\r\n$3\r\nGET\r\n$1\r\na\r\n*1\r\n$4\r\nPING\r\n");
        for bad in [&[2u64][..], &[2, 2], &[0, 3]] {
            assert!(encode_pipeline(&args, bad, &mut vec![]).is_err());
        }
    }
}
