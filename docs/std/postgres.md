# velt:postgres

> This driver is moving from the standard library to a separately versioned package. The API
> stays the same.

`import { connect, createPool, Client, Pool, PgError } from "velt:postgres"`. A PostgreSQL
client (tokio-postgres in the runtime) with an API close to node-postgres. Every call that talks
to the server is async.

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
  - `batchQuery<T, P>(sql, paramSets: P[]): T[][]`, `batchQueryOne<T, P>(sql, paramSets): (T | null)[]`
    and `batchExecute<P>(sql, paramSets): i64[]` run one statement with many parameter sets in
    one round trip (one message group with a single Sync, like pgx's `Batch`): the server runs
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
  format), both null at the end; `r.close()` stops early. The connection is busy until the
  copy ends; a failed copy counts against an enclosing `transaction`.
- `createPool({ url, max? }): Pool` (default `max` 10; a bad connection string throws here,
  connections open on first use). A `Pool` has the same `query`/`queryOne`/`execute`/`select`/
  `selectOne`/`run`/`batch` methods, each running on a free connection (waiting when all `max`
  are busy), plus `connect(): Client` (a dedicated connection; its `close()` returns it to the
  pool), `transaction(fn)` on a dedicated connection, `idleCount` and `end()`. Pools are safe
  to use from many spawned tasks.
- `PgError { code, message, detail, constraint }`: `code` is the server's SQLSTATE (for
  example `"23505"` unique violation, `"42P01"` undefined table, `"42601"` syntax error,
  `"25P02"` statement in an aborted transaction). Failures outside the server use names:
  `"ECONNREFUSED"` (and the other velt:io names), `"ECONNRESET"` (connection lost),
  `"ECLOSED"` (closed client or ended pool), `"EINVAL"` (connection string or parameters),
  `"ETLS"`, `"ENOTSUP"` (a column type that can't be decoded) and `"EMISMATCH"` (a row that
  doesn't fit `T`).
- **TLS** is rustls. `sslmode=disable`; `prefer` (the default: TLS if the server offers it);
  `require` (encrypted, but the certificate isn't checked); `verify-ca` (the certificate must
  chain to a trusted root); `verify-full` (the host name must also match). Trusted roots are
  Mozilla's plus the PEM file named by `sslrootcert`, which also upgrades `require` to
  `verify-ca`, as in libpq. SCRAM channel binding isn't offered. The goldens only cover
  unencrypted local servers; the modes were checked by hand against a server with a private CA.

```ts
import { connect, createPool } from "velt:postgres";

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

Notes: a `Client` or `Pool` that was closed through one copy must not be used through another
(like velt:net sockets). A pooled connection that is still in a transaction when its client is
closed is dropped rather than reused. WebAssembly isn't supported.
