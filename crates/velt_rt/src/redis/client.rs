//! Client operations at the C ABI: connect, duplicate, close, single commands and pipelines
//! (plain or `MULTI`/`EXEC`), all on the multiplexed connection of `multiplex.rs`.
//!
//! `RedisClient` is a Copy struct in Velt, so a client handle is a registry key
//! (`crate::registry`): after `close()` through one copy, every copy fails with `EBADF` instead
//! of reaching a freed connection.

use super::connect::Endpoint;
use super::error::RedisErr;
use super::multiplex::Conn;
use super::reply::{self, Flat, VeltRedisReply};
use super::resp::encode_command;
use super::{str_args, url};
use crate::array::VeltArray;
use crate::net::tcp::text_arg;
use crate::registry::{Key, Registry};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;

/// Opaque client handle; in-flight commands keep their own `Arc<Conn>`.
pub type ClientHandle = Key<Conn>;

/// Open clients.
pub(super) static CLIENTS: Registry<Conn> = Registry::new();

fn client_result(r: Result<Conn, RedisErr>) -> IoResult<ClientHandle> {
    match r {
        Ok(conn) => IoResult::ok(CLIENTS.insert(conn)),
        Err(e) => IoResult::err(e.to_velt()),
    }
}

fn reply_result(r: Result<Flat, RedisErr>) -> IoResult<VeltRedisReply> {
    match r {
        Ok(flat) => IoResult::ok(flat.into_velt()),
        Err(e) => IoResult::err(e.to_velt()),
    }
}

/// `connect(url, { ca })` → `IoResult<ClientHandle>`: `redis://` or `rediss://` (TLS, also
/// trusting the PEM CAs in `ca`, `""` = none), with `AUTH` and `SELECT` from the URL.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_connect(
    url: *const VeltStr,
    ca: *const VeltStr,
) -> *mut VeltFut {
    let url = text_arg(url);
    let ca = (*ca).text_lossy().as_bytes().to_vec();
    new_leaf(async move {
        let r = match url::parse(&url) {
            Ok(target) => Conn::open(Endpoint { target, ca }).await,
            Err(e) => Err(RedisErr::invalid(e)),
        };
        client_result(r)
    })
}

/// `client.duplicate()` → `IoResult<ClientHandle>`: a new connection opened like `c`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_duplicate(c: ClientHandle) -> *mut VeltFut {
    CLIENTS.op::<ClientHandle>(c, |conn| {
        let endpoint = conn.endpoint.clone();
        new_leaf(async move { client_result(Conn::open(endpoint).await) })
    })
}

/// `client.close()`: release the handle (through any copy; closing twice is a no-op); the
/// connection closes once no command is in flight.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_close(c: ClientHandle) {
    CLIENTS.remove(c);
}

/// `call(args)` → `IoResult<VeltRedisReply>`: one command (`args[0]` is its name, all copied).
/// An error reply fails with code 100 (§14.12).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_command(
    c: ClientHandle,
    args: *const VeltStrArray,
) -> *mut VeltFut {
    let args = str_args(args);
    CLIENTS.op::<VeltRedisReply>(c, |conn| {
        let mut payload = vec![];
        if !args.is_empty() {
            encode_command(&args, &mut payload);
        }
        new_leaf(async move {
            if payload.is_empty() {
                return reply_result(Err(RedisErr::invalid("empty Redis command".to_string())));
            }
            let r = conn.send(payload, 1).await.and_then(|mut replies| {
                reply::single(replies.pop().expect("ICE: one reply per command"))
            });
            reply_result(r)
        })
    })
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

/// `pipeline.exec()` → `IoResult<VeltRedisReply>`: the commands (`args` split by `counts`) sent
/// together; the reply is an array with one node per command (error replies as ERROR nodes).
/// With `atomic`, they run in `MULTI`/`EXEC` and the result is `EXEC`'s array (an aborted
/// transaction fails with code 100).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_redis_pipeline(
    c: ClientHandle,
    args: *const VeltStrArray,
    counts: *const VeltArray<u64>,
    atomic: u8,
) -> *mut VeltFut {
    let Some(conn) = CLIENTS.get(c) else {
        return crate::registry::closed_leaf::<VeltRedisReply>();
    };
    let counts = (*counts).as_slice();
    let atomic = atomic != 0;
    let mut payload = vec![];
    if atomic {
        encode_command(&["MULTI"], &mut payload);
    }
    let encoded = encode_pipeline(&str_args(args), counts, &mut payload);
    if atomic {
        encode_command(&["EXEC"], &mut payload);
    }
    let replies = counts.len() + if atomic { 2 } else { 0 };
    new_leaf(async move {
        let r = match encoded {
            Err(e) => Err(e),
            Ok(()) => conn.send(payload, replies).await.and_then(|v| {
                if atomic {
                    reply::transaction(v)
                } else {
                    Ok(reply::pipeline(v))
                }
            }),
        };
        reply_result(r)
    })
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
