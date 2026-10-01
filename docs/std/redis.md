# velt:redis

> This driver is moving from the standard library to a separately versioned package. The API
> stays the same.

`import { connect, subscribe, psubscribe, RedisClient, RedisError, ZMember } from "velt:redis"`. A
Redis client close to ioredis: strings with expiry, counters, hashes, lists, sets, sorted sets,
pipelines, `MULTI`/`EXEC` transactions and pub/sub, over `redis://` or `rediss://` (TLS). One
`RedisClient` is one multiplexed connection: commands from any number of tasks are pipelined on
it in order, so it already works as a connection pool (there is no separate `createPool`).

- `connect(url, opts: RedisConnectOptions { ca? } = {}): Promise<RedisClient>`: URL
  `redis://[[user]:password@]host[:port][/db]` (`AUTH` and `SELECT` are sent for you);
  `rediss://` uses TLS, also trusting the PEM CAs in `ca`.
- `RedisClient` (Copy handle, many tasks may use it at once; `close()` once when done):
  - Commands that take several keys or values take a `string[]` (there are no rest
    parameters): `del(["a", "b"])`.
  - Keys and strings: `get(key): string | null`, `set(key, value, opts: SetOptions { ex?, px?, nx?,
    xx? } = {}): bool` (false when `nx`/`xx` prevented it), `del(keys): i64`,
    `exists(keys): i64`, `expire(key, seconds): bool`, `pexpire(key, ms): bool`, `ttl(key)`,
    `pttl(key)` (-1 no expiry, -2 missing), `persist(key): bool`, `incr`, `incrBy(key, n)`,
    `decr`, `decrBy` (`i64`), `mget(keys): (string | null)[]`, `mset(entries: Map<string,
    string>)`, `keys(pattern): string[]`, `ping(): string`.
  - Hashes: `hget(key, field): string | null`, `hset(key, field, value): i64`,
    `hmset(key, fields: Map<string, string>): i64`, `hgetall(key): Map<string, string>`,
    `hdel(key, fields): i64`, `hincrBy(key, field, n): i64`, `hlen(key): i64`.
  - Lists: `lpush(key, values)`, `rpush(key, values)` (new length), `lpop(key)`, `rpop(key)`
    (`string | null`), `lrange(key, start, stop): string[]` (negative indexes count from the
    end), `llen(key)`.
  - Sets: `sadd(key, members)`, `srem(key, members)` (`i64`), `smembers(key): string[]`,
    `sismember(key, member): bool`, `scard(key)`.
  - Sorted sets: `zadd(key, members: ZMember[]): i64` with `ZMember { member, score: f64 }`,
    `zrange(key, start, stop): string[]`, `zrangeWithScores(...): ZMember[]`,
    `zscore(key, member): f64 | null`, `zrem(key, members): i64`,
    `zincrby(key, increment, member): f64`, `zcard(key)`. Infinite scores map to `±inf`.
  - `call(args: string[]): RedisReply` runs any command (`args[0]` is its name).
  - `publish(channel, message): i64` (number of receivers).
  - `pipeline()` / `multi(): RedisPipeline`; `duplicate(): Promise<RedisClient>` (a second
    connection, e.g. for blocking commands like `BLPOP`); `close()`.
- `RedisReply { kind: "nil" | "status" | "error" | "int" | "string" | "array"; text; int;
  items }` with `isNull()`, `isError()`, `asString(): string | null`, `asInt(): i64 | null`,
  `asStrings(): string[]`.
- `RedisPipeline`: queue with `get set del incr incrBy expire hget hset lpush rpush sadd zadd
  publish` or `call(args)` (all return nothing), then `exec(): Promise<RedisReply[]>`, one
  reply per command. In a pipeline a rejected command is an `"error"` reply, not a throw; a
  `multi()` transaction the server aborts throws `RedisError` "EXECABORT". `length` counts the
  queued commands.
- `subscribe(target: string | RedisClient, channels, opts = {}): Promise<RedisSubscriber>` and
  `psubscribe(target, patterns, opts = {})` (glob patterns) open a dedicated connection (to the
  URL, or to the server and database a client uses) and resolve once the server confirmed.
- `RedisSubscriber` (Copy handle): `next(): Promise<RedisMessage | null>` (null after
  `close()`), `subscribe(channels)`, `unsubscribe(channels)`, `psubscribe(patterns)`,
  `punsubscribe(patterns)`, `close()` (call once, from any task).
- `RedisMessage { channel; message; pattern: string | null }`.
- `RedisError { code, message }`: `code` is the server's error code for error replies
  (`"WRONGTYPE"`, `"ERR"`, `"NOAUTH"`, `"WRONGPASS"`, `"EXECABORT"`, …) or an I/O code
  (`"ECONNREFUSED"`, `"ECONNRESET"`, `"ETIMEDOUT"`, `"EINVAL"` for a bad URL or argument).

```ts
import { connect, subscribe } from "velt:redis";

async function main() {
  const redis = await connect("redis://127.0.0.1:6379/0");
  await redis.set("greeting", "hello", { ex: 60 });
  console.log(await redis.get("greeting")); // hello
  console.log(await redis.incr("visits")); // 1, 2, …

  const p = redis.pipeline();
  p.hset("user:1", "name", "Ada");
  p.hget("user:1", "name");
  const replies = await p.exec();
  console.log(replies[1].asString()); // Ada

  const sub = await subscribe(redis, ["chat"]);
  await redis.publish("chat", "hi");
  let m = await sub.next();
  while (m != null) {
    console.log(`${m.channel}: ${m.message}`); // chat: hi
    sub.close(); // the next `next()` returns null
    m = await sub.next();
  }
  redis.close();
}
```

Notes: the protocol is RESP2, implemented in the runtime over tokio with the shared rustls
configuration (no extra dependencies). A lost connection is reopened automatically: commands
already sent on it throw `RedisError` "ECONNRESET" (they may have run, so they are not resent);
later commands wait while the client reconnects with exponential backoff (50 ms doubling to 2 s,
for up to 10 s; then they throw the connect error and the next command tries again).
Subscribers do not reconnect. Messages are pulled with
`next()` (there is no `for await`, and the runtime stores no callbacks). Values are UTF-8
strings (invalid bytes read back as U+FFFD). Not available on WebAssembly.

