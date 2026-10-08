# redis-clone

A Redis-compatible server: the RESP2 protocol over `velt:net`, with pipelining, 65 commands
over strings, lists, hashes, sets and sorted sets, and key expiry (on access, plus a background
sweep). `redis-cli` and `redis-benchmark` work against it unchanged, and `parity.vlt` checks
that its replies match a real Redis byte for byte (188 commands, including errors and edge
cases).

```sh
velt run                       # 127.0.0.1:6380 (velt run -- <port> for another)
redis-cli -p 6380 set greeting hello EX 60
redis-cli -p 6380 rpush queue a b c
velt test                      # tests/: protocol, commands, expiry with a fixed clock
velt run demo.vlt              # a client session against an in-process server (golden: demo.out)
./bench.sh                     # parity, then redis-benchmark against a real redis-server
```

**Commands:**
- connection and server: `PING ECHO QUIT SELECT CLIENT COMMAND CONFIG INFO DBSIZE FLUSHDB FLUSHALL`
- keys: `DEL UNLINK EXISTS TYPE KEYS EXPIRE PEXPIRE TTL PTTL PERSIST`
- strings: `GET SET` (with `EX PX EXAT PXAT NX XX GET KEEPTTL`) `SETNX SETEX PSETEX GETDEL MGET MSET APPEND STRLEN INCR DECR INCRBY DECRBY`
- lists: `LPUSH RPUSH LPOP RPOP LLEN LRANGE LINDEX`
- hashes: `HSET HMSET HGET HMGET HDEL HEXISTS HLEN HGETALL HKEYS HVALS HINCRBY`
- sets: `SADD SREM SISMEMBER SCARD SMEMBERS SPOP`
- sorted sets: `ZADD ZSCORE ZCARD ZREM ZRANGE` (`WITHSCORES`) `ZPOPMIN`

**Where it differs from Redis:**
- **Persistence and replication:** none.
- **Commands:** 65 commands, not about 240, and only one database.
- **Binary keys:** values are binary-safe, but keys and set/hash members are decoded as UTF-8
  (invalid bytes become U+FFFD).
- **Active expiry:** each sweep looks at the first 20 keys with a time to live, where Redis
  samples at random. Keys outside that window are only removed when they are accessed.
- **Atomicity:** each batch of pipelined commands runs under one lock, so other clients never
  see it half done, which is stronger than Redis guarantees.

| File | What |
|---|---|
| `src/resp.vlt` | `RespParser` (incremental: commands may arrive split or pipelined), `Writer` (replies into a growable `u8[]`) |
| `src/db.vlt` | `Db`: one map per value type plus an expiry map; `ZSet` (scores + an ordered array) |
| `src/commands.vlt` | `execute(db, args, out)`: each command with Redis's argument rules and error messages |
| `src/server.vlt` | a task per connection; each read's commands run as one batch under the keyspace lock |
| `parity.vlt` | 188 commands against this server and a real Redis, replies compared byte for byte |
| `bench.sh` | parity, then `redis-benchmark` (median of 3, interleaved), CPU per request, memory |

## Performance

`./bench.sh` on an Apple M-series laptop (4 performance + 6 efficiency cores), Redis 8.8.1,
50 connections, 200k requests per run, median of 3 runs interleaved between the servers. The
Velt server runs twice: on one worker thread (`VELT_THREADS=1`) and with the default (one per
core):

| requests/s | Redis | Velt, 1 thread | Velt, default |
|---|---|---|---|
| PING | 250,312 | 257,400 | 257,731 |
| SET | 250,312 | 258,397 | 254,129 |
| GET | 251,256 | 257,069 | 249,376 |
| INCR | 251,256 | 252,844 | 243,309 |
| LPUSH / LPOP | 251,889 / 250,000 | 250,000 / 249,066 | 242,424 / 238,663 |
| SADD / HSET / ZADD | 250,000 / 248,138 / 246,913 | 246,913 / 246,305 / 244,200 | 234,741 / 234,192 / 231,481 |
| LRANGE (100 elements) | 144,927 | 145,666 | 140,646 |
| MSET (10 keys) | 230,946 | 218,340 | 214,362 |
| SET, 16 pipelined | 2,352,941 | 2,352,941 | 2,531,645 |
| GET, 16 pipelined | 2,857,142 | 2,597,402 | 2,898,550 |

| Same million GETs | Redis | Velt, 1 thread | Velt, default |
|---|---|---|---|
| server CPU per request | 3.5 µs | 4.0 µs | 8.6 µs |

| Memory for the same 632k keys (16-byte values) | Redis | Velt, 1 thread | Velt, default |
|---|---|---|---|
| RSS | 70 MB | 89 MB | 119 MB |

Without pipelining, around 250k requests/s is the limit of `redis-benchmark`'s single client
thread, so all three are level there. The CPU and memory rows show the real differences:
- **On one thread,** Velt needs 14% more CPU per request than Redis and 27% more memory.
- **On the default runtime,** the same requests cost more than twice the CPU, with the work spread
  over threads that wake each other through one keyspace lock, and 30 MB more memory.

**What made it fast.** The first `Writer` built each `$<length>\r\n` header with a template
string and `utf8Encode`: three small allocations per bulk reply. That made LRANGE (100 bulk
replies per request) 31% slower than Redis. Writing the digits and CRLFs straight into the
buffer made LRANGE level with Redis and GET 20% faster in the same run. Replacing std's
`Deque` (whose `at()` returns a copy) with a list that hands out references made no measurable
difference, so lists use `Deque`.

Findings from building it (bugs, std gaps, the multi-threaded CPU cost) are tracked in #721.
