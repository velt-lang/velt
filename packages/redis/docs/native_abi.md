# The redis package's native library

The package's native library (`native/`, crate `velt_redis_native`, library `velt_native_redis`),
built on the native-package SDK: a RESP2 client over the runtime's own sockets
(`velt_native::net::TcpStream`, on Velt's scheduler the worker's reactor) and timers, with TLS
from the shared client configuration (`velt_tls`: `ring`, webpki roots + extra PEM CAs). A
client is a registry key of one **multiplexed** connection: requests from any number of tasks
are queued in order and written in batches, and a reader task completes them in order;
in-flight operations keep a reference, so `close` is allowed any time, and a closed handle
fails with `EBADF`. A failed connection fails the commands already written to it
(`ECONNRESET`); later commands reopen it (exponential backoff for up to 10 s, stream
linux-servers) or fail with the connect error. A subscriber is a key of a dedicated subscribed
connection whose messages are pulled (no callbacks are stored, rt_abi_async.md §13.5).

URLs: `redis://[[user]:password@]host[:port][/db]` or `rediss://…` (TLS); percent-decoded
credentials, `[v6]` hosts, `?db=N`. The handshake sends `AUTH` and `SELECT` as needed; connecting
(TCP + TLS + handshake) times out after 10 s (`ETIMEDOUT`). A bad URL is `EINVAL`.

Errors: I/O failures use the rt_abi_async.md §3 codes. An **error reply** from the server uses code **100**
with the reply text as the message (`WRONGTYPE Operation against …`); the Velt side takes its first
word as `RedisError.code`. A reply that is not valid RESP is `EILSEQ`.

Commands are encoded from the borrowed `string[]` when called (an `#[export(async)]` `fn`
returning its future, the contract native_abi.md); replies are written straight into Velt's arrays.

```
RawReply   { tags: u8[]; nums: i64[]; strs: string[] }
  One entry per node in pre-order (a node, then its children). tags: 0 nil, 1 status,
  2 error, 3 int, 4 bulk string, 5 array; nums: the int value or the array's length (else 0);
  strs: the text of status/error/string nodes (else ""; invalid UTF-8 becomes U+FFFD).
RawMessage { kind: u32; channel: string; message: string; pattern: string }
  kind: 0 = closed (after close), 1 = message, 2 = pattern message (pattern set)
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_redis__connect` | `async (string url, string ca) -> IoResult<u64>` | `ca`: extra PEM CAs for `rediss://` (`""` = none) |
| `velt_redis__duplicate` | `async (u64 c) -> IoResult<u64>` | a second connection opened like `c` |
| `velt_redis__close` | `(u64 c)` | releases the handle (through any copy; again: no-op) |
| `velt_redis__command` | `async (u64 c, string[] args) -> IoResult<RawReply>` | one command (`args[0]` its name); an error reply fails with code 100 |
| `velt_redis__pipeline` | `async (u64 c, string[] args, u64[] counts, bool atomic) -> IoResult<RawReply>` | `args` split by `counts`, sent together; one array node per command (error replies as nodes); `atomic`: in `MULTI`/`EXEC`, the result is `EXEC`'s array |
| `velt_redis__subscribe` | `async (u64 c, string url, string ca, string[] names, bool patterns) -> IoResult<u64>` | a new subscribed connection (to client `c`'s server, or `url` when `c` is 0); resolves once every name is confirmed |
| `velt_redis__sub_next` | `async (u64 s) -> IoResult<RawMessage>` | the next message; kind 0 after `close` |
| `velt_redis__sub_change` | `async (u64 s, string[] names, bool patterns, bool subscribe) -> IoStatus` | (un)subscribe, once confirmed |
| `velt_redis__sub_close` | `(u64 s)` | pending and later `next` calls return kind 0 |

Not available on WebAssembly: a package with native code is an error there.
