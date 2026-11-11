# postgres

`import { connect, createPool, Client, Pool, PgError } from "postgres"`. A PostgreSQL client
written in plain Velt on `velt/net`, with an API close to node-postgres. Every call that talks to
the server is async.

```ts ignore
// package.velt
dependencies: {
  // From the Velt repository (there is no public registry yet):
  postgres: {
    git: "https://github.com/velt-lang/velt",
    branch: "main",
    path: "packages/postgres",
  },
  // Or a checkout: postgres: { path: "../velt/packages/postgres" },
},
```

Nothing here runs native code, so using the package needs no toolchain beyond `velt`. The `native/`
folder is the Rust driver this client replaces, with its ABI in `docs/native_abi.md`. Nothing in
`src/` uses it. It stays until this client also matches it on `pool_select_64`
([#2370](https://github.com/velt-lang/velt/issues/2370)), and then goes.
The full guide: https://veltlang.com/docs/packages/postgres/.

- `connect(url): Promise<Client>`. `url` is `postgres://user:password@host:port/db?options` or
  a libpq string (`host=… user=… dbname=…`). Supported options include `connect_timeout`,
  `application_name` and `sslmode`.
- `Client` is a handle, so you can pass it around and capture it in tasks. Concurrent
  queries on one client are pipelined on its connection. Methods:
  - `query<T, P>(sql, params): T[]`, `queryOne<T, P>(sql, params): T | null`,
    `execute<P>(sql, params): i64` (rows affected or returned);
  - the same without parameters: `select<T>(sql)`, `selectOne<T>(sql)`, `run(sql)` (passing
    `{}` as `params` also works);
  - `batch(sql)` runs a script of `;`-separated statements (no parameters, not prepared);
  - `batchQuery<T, P>(sql, paramSets: P[]): T[][]`,
    `batchQueryOne<T, P>(sql, paramSets): (T | null)[]` and
    `batchExecute<P>(sql, paramSets): i64[]` run one statement with many parameter sets in one
    round trip (one message group with a single Sync, like pgx's `Batch`): the server runs
    them as one implicit transaction, so if one execution fails the whole batch throws that
    error and, outside a transaction, none of its writes are kept;
  - `begin(): Transaction`, `transaction(fn)`, `inTransaction`, and `close()`.
- **Prepared statements** are cached per connection (256 per connection, least recently used
  evicted), so repeating a query costs one round trip.
- **Parameters** are a value that std passes through `JSON.stringify`. An array binds
  `$1, $2, …` by position. An object binds `:name` or `$name` by field name (std rewrites them
  to `$n` once per statement; `::type` casts, strings, quoted identifiers and comments are
  left alone). Values are converted for the types the server inferred: integers are
  range-checked for `int2`/`int4`, numbers also go to `numeric` and text, bools go to `bool`,
  `u8[]` goes to `bytea`, and a string goes to text and `json`/`jsonb` (pass
  `JSON.stringify(doc)`). A string for any other type (`uuid`, `date`, `timestamptz`,
  `numeric`, arrays such as `"{1,2}"`, enums) is parsed by the server. Array elements share
  one type, so mixed positional values are either an object or all strings.
- **Rows** are decoded with `JSON.parse<T>`, so a column fills the field with the same name
  (rename with `AS`):

  | PostgreSQL | Velt field |
  |---|---|
  | `int2`, `int4`, `int8`, `oid` | integer types (exact for all of i64), `f64` |
  | `float4`, `float8` | `f64` (NaN and ±Infinity become `null`) |
  | `numeric` | `string` with its exact digits (`"12.50"`); cast `::float8` for a number |
  | `bool` | `bool` |
  | `text`, `varchar`, `char(n)`, `name`, `citext`, enums, `uuid` | `string` |
  | `date`, `time`, `timestamp` | ISO 8601 `string` (`"2024-02-29"`, `"13:45:00"`, `"2024-02-29T13:45:00.5"`) |
  | `timestamptz` | ISO 8601 in UTC (`"2024-02-29T11:45:00.500Z"`) |
  | `interval` | PostgreSQL's text form `string` (`"1 year 2 mons 3 days 04:05:06.5"`) |
  | `money` | exact amount `string` (`"-1234.50"`, no currency symbol or grouping) |
  | `inet`, `cidr`, `macaddr`, `macaddr8` | PostgreSQL's text form `string` (`"10.0.0.1"`, `"10.0.0.0/8"`) |
  | `json`, `jsonb` | any type matching the document (nested classes, arrays, …) |
  | `bytea` | `u8[]` |
  | arrays of the above | `T[]`, `T[][]` (NULL elements need `(T \| null)[]`) |
  | domains | as their base type |
  | NULL | `T \| null` |

  Other types (ranges, composites, geometric types, …) throw `ENOTSUP`; cast them, for
  example `d::text`. Fractions of a second use 3 digits for whole milliseconds, 6
  otherwise, and are left out when zero.
- `transaction(fn)` (`fn: async (tx: Client) => T`, which may throw) commits after `fn`
  returns. If `fn` throws, or any operation on the client failed in the meantime (even one `fn`
  caught), it rolls back and throws that error (the first failed operation's). Nested calls use
  savepoints: a failed inner transaction undoes only its own work. `begin()` returns a
  `Transaction` with async `commit()` and `rollback()` for explicit control.
- `COPY`, streamed: `copyFrom(sql): CopyWriter` starts `COPY … FROM STDIN` (for example
  `"COPY items (id, name) FROM STDIN (FORMAT csv)"`); `await w.write(text)` /
  `writeBytes(u8[])` send chunks split anywhere, `await w.end()` completes it and returns the
  rows copied, `w.abort()` cancels it (nothing is copied). `copyTo(sql): CopyReader` starts
  `COPY … TO STDOUT`; `await r.read()` returns the data received so far (whole rows in the
  text and CSV formats, up to ~64 KiB per call), `readBytes()` the same as bytes (binary
  format), both null at the end; `r.close()` stops early. A `CopyReader` is an
  `AsyncIterable<string, PgError>`: `for await (const chunk of r)` reads the text chunks to the
  end (leaving the loop early keeps the copy open: `close()` it). The connection is busy until the
  copy ends; a failed copy counts against an enclosing `transaction`.
- `createPool({ url, max? }): Pool` (default `max` 10; a bad connection string throws here,
  connections open on first use). A `Pool` has the same `query`/`queryOne`/`execute`/`select`/
  `selectOne`/`run`/`batch`/`batchQuery`/`batchQueryOne`/`batchExecute` methods, each running on
  a free connection (waiting when all `max` are busy), plus `connect(): Client` (a dedicated
  connection; its `close()` returns it to the pool), `transaction(fn)` on a dedicated
  connection, `idleCount` and `end()`. Pools are safe to use from many spawned tasks.
- `PgError { code, message, detail, constraint }`: `code` is the server's SQLSTATE (for
  example `"23505"` unique violation, `"42P01"` undefined table, `"42601"` syntax error,
  `"25P02"` statement in an aborted transaction). Failures outside the server use names:
  `"ECONNREFUSED"` (and the other velt/io names), `"ECONNRESET"` (connection lost),
  `"ECLOSED"` (closed client or ended pool), `"EINVAL"` (connection string or parameters),
  `"ETLS"`, `"ENOTSUP"` (a column type that can't be decoded) and `"EMISMATCH"` (a row that
  doesn't fit `T`).
- **TLS**, as libpq has it, through `velt/net`:
  - `disable` is plain text.
  - `prefer` (the default) and `require` send `SSLRequest`. A server that answers `S` gets TLS on
    that socket (velt/net's `startTls`), encrypted but **without a certificate check**, as libpq
    does: it keeps out passive listeners, not someone who can intercept the connection. A server
    that answers `N` has no TLS: `prefer` goes on in plain text, `require` fails with `"ETLS"`. Any
    other answer, and any failed handshake, is `"ETLS"`: plain text comes only from an `N`. This
    works with any server version, and with a self-signed certificate such as Debian and Ubuntu
    install.
  - `verify-ca` and `verify-full` use PostgreSQL 17's direct TLS (ALPN `postgresql`, which the
    server must agree to) with the certificate checked against Mozilla's roots and the PEM file
    named by `sslrootcert`, the name too under `verify-full`. They need PostgreSQL 17 or later.
    `verify-full` is the mode that authenticates the server.

  The answer to `SSLRequest` is read one byte at a time, so nothing sent in plain text after it is
  taken for part of the encrypted stream (libpq's CVE-2021-23222). The handshake ends by the
  connection's deadline: `connect_timeout`, else 30 seconds for the whole opening of each host.
  `sslnegotiation` is accepted and changes nothing: `prefer` and `require` always ask first, and the
  checked modes always go direct. A server that refuses a TLS connection (`hostnossl`) is not tried
  again in plain text. SCRAM channel binding isn't offered.
- **SCRAM** has no SASLprep, since std has no Unicode normalization: a password goes as its own UTF-8
  bytes. ASCII passwords and those already in Unicode's compatibility normal form (NFKC) work. One
  that SASLprep would change (`Ⅸ` for `IX`, a no-break space, a soft hyphen) is refused by the
  server with `"28P01"`; md5 and cleartext passwords aren't affected.
- Unix-domain sockets (`host=/dir`) throw `"ENOTSUP"`, and keepalive options are accepted and do
  nothing, so a connection gets no `SO_KEEPALIVE` (the Rust driver had it on by default, 2 hours
  idle): `velt/net` has no Unix-domain stream and no socket options, and a socket made in Velt with
  `declare c` can't become a `TcpStream`.

```ts
import { connect, createPool } from "postgres";

class User {
  id: i64;
  name: string;
  constructor(id: i64, name: string) {
    this.id = id;
    this.name = name;
  }
}

async function main() {
  const db = await connect("postgres://app@localhost/app");
  await db.batch("CREATE TEMP TABLE users (id BIGSERIAL PRIMARY KEY, name TEXT UNIQUE)");
  await db.execute("INSERT INTO users (name) VALUES (:name)", { name: "Ann" });
  const users: User[] = await db.query("SELECT id, name FROM users WHERE id > $1", [0]);
  const ann: User | null = await db.queryOne("SELECT * FROM users WHERE name = :n", { n: "Ann" });
  console.log(users.length, ann?.id); // 1 1
  try {
    await db.transaction(async (tx) => {
      await tx.execute("INSERT INTO users (name) VALUES ($1)", ["Bob"]);
      try {
        await tx.execute("INSERT INTO users (name) VALUES ($1)", ["Ann"]);
      } catch (e) {
        console.log(e.code, e.constraint); // 23505 users_name_key
      }
      return 0;
    });
  } catch (e) {
    console.log("rolled back:", e.code); // Bob was not inserted either
  }
  db.close();

  const pool = createPool({ url: "postgres://app@localhost/app", max: 4 });
  console.log(await pool.run("SELECT 1")); // 1
  pool.end();
}
```

Notes: after `close()` or `end()` through one copy of a `Client` or `Pool`, every copy throws
`"ECLOSED"` (closing again does nothing). A pooled connection goes back to the pool only when nothing
is in flight on it: one still in a transaction, with a statement still running (one given up on with
`timeout`), or with a `COPY` still open when its client is closed is dropped rather than reused, and
an open `COPY`'s reader or writer then fails with `"ECONNRESET"`.

A task that stops waiting (`timeout`, a lost `Promise.race`) is not dropped in Velt: its statement,
or `pool.transaction`'s body, runs on to its end, and the connection goes back then. A task that
panics under `catchPanic` (what `http-server` does with a handler that panics) is abandoned with
nothing it held dropped, so a connection it held, from `pool.connect()`, `pool.transaction` or a
one-statement call, never goes back: the pool has one connection less until `end()`. Should the
runtime drop a task (nothing does today), its values are dropped and neither its `catch` nor its
`finally` runs: a `pool.transaction` connection is then closed, so the server rolls back, and the
pool may open another; a one-statement call's connection goes back once its reply is read; but a
`Client` from `pool.connect()` is a Copy handle with nothing to drop, so its connection never goes
back.

A connection idle over 100 ms has its socket looked at before a pool hands it out, so one the server
closed is replaced. `pool.connect()` and `pool.transaction` look at every idle connection's socket.
A one-statement call (`query`, `run`, `batch` …) whose connection the server ended within those
100 ms, so that its reply is the server's FATAL `"57P01"`, `"57P02"` or `"57P03"` or the connection
closes before any reply, runs once more on a connection whose socket was looked at, or a new one:
the session ended before the statement ran, so nothing of it was committed. (A `batch` whose own
statement ends its session before sending anything, such as a `DO` block that calls
`pg_terminate_backend(pg_backend_pid())`, looks the same, and runs once more too.)

## How it works

A request takes no hop through another task, and a connection has no task of its own. Under a short
lock the caller queues its bytes on the socket and takes its place in the line for replies; its own
task sends them when it next waits. The reading end (`Input`) is held by the request whose replies come
next: it reads in its own task, decodes the rows right out of the read buffer, and hands the `Input` to
the next in line. Parameters are encoded in place for the types the server inferred. A task dropped
while it waits or reads drops what it held, and a task starts to read what the server still owes, so
the next request finds the connection in step; a task given up on with `timeout` is not dropped, and
reads its own reply.

## Layout and tests

- `src/lib.velt`: the API (re-exports); `client.velt`, `pool.velt`, `transaction.velt`, `copy_reader.velt`,
  `copy_writer.velt`, `errors.velt`: the public types.
- The connection: `line.velt` (the state behind its lock) and `sent.velt`, `failure.velt`, `failure_log.velt`,
  `connection.velt` (requests), `ops.velt` (what `Client` and `Pool` share), `builder.velt`,
  `encoder.velt` and `binary_values.velt` (a request's bytes), `input.velt`, `read_buffer.velt` and `reply.velt` (a reply, and
  rows through velt/db's `readRows`), `drain.velt` (after a task went away), `startup.velt`,
  `socket.velt`, `unchecked_tls.velt` (TLS without a certificate check), `transport.velt` and `tls_read.velt` (the stream
  under it), `config.velt`, `describe.velt`, `prepared.velt`
  and `statement_cache.velt` (statements), `copy_state.velt`.
- The pool: `pool_state.velt` (the state behind its lock), `grant.velt`, `taken.velt`,
  `waiting_for_pool.velt`, `waiter_line.velt` and `waiter.velt` (the tasks in line), `opening.velt`, `lent.velt`, `pool_options.velt`.
- The protocol: `frontend.velt`, `backend.velt`, `scram.velt`, `md5.velt`, `placeholders.velt`,
  `pg_type.velt`, `values.velt`, `temporal.velt`, `network.velt`, `array_json.velt`, `json_params.velt`.
- `tests/`: `velt test` in this directory. `values`, `auth`, `config`, `placeholders`,
  `fake_server`, `negotiation`, `abandon`, `line`, `wire` and `encoder` need no server; the rest need one: set `VELT_TEST_PG_URL`
  (`postgres://user:password@host/db`), without which they do nothing. `VELT_TEST_PG_MD5_URL`,
  `VELT_TEST_PG_SCRAM_URL` and `VELT_TEST_PG_TLS_URL` add the password exchanges and TLS;
  `VELT_TEST_PG_TLS_UNTRUSTED_URL` (a server with TLS that nothing verifies) and
  `VELT_TEST_PG_PLAIN_URL` (one without TLS), both without `sslmode`, what each `sslmode` does. The
  repository's `scripts/linux-check.sh --services` starts a server, as the nightly CI does; it sets
  only `VELT_TEST_PG_URL`.

`src/` uses only public std APIs: velt/db (`readRows`, `writeParams`), velt/net, velt/crypto.
