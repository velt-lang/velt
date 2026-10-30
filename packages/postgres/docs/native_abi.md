# The postgres package's native library

## PostgreSQL

The package's native library (`native/`, crate `velt_postgres_native`, library
`velt_native_postgres`), built on the native-package SDK (`velt_native` with `tokio`, `velt_text`,
`velt_tls`); its functions are `velt_postgres__*` exports with the signatures below, declared in
`src/native.vlt`. Over `tokio-postgres` (`default-features = false`), connected over the runtime's own sockets (`velt_native::net::TcpStream`) and wire stream ("Batches" below). Rows are written into the result string itself (`velt_native::StrBuf`); `sql` and `params` are kept by reference, not copied.
TLS is a hand-written `TlsConnect` over tokio-rustls with the runtime's `ring` provider and
roots (`velt_tls`); `sslmode` = `disable`, `prefer` (default), `require` (no certificate check),
`verify-ca`, `verify-full`, plus `sslrootcert=<PEM file>` (extra roots; turns `require` into
`verify-ca`). No channel binding (SCRAM falls back to SCRAM-SHA-256). Every server round trip is
an async leaf (rt_abi_async.md §1). `VeltPgClient` is an `Arc` of `{ connection (tokio-postgres client + LRU
statement cache of 256 + transaction depth), pool lease, failure counter, last 32 failures }`;
operations keep an `Arc` of the connection, so `close` is safe while they run. `VeltPgPool` is
an `Arc` of `{ config, semaphore(max), idle connections }`. The null handle (a closed `Client`,
an ended `Pool`) fails with `ECLOSED` instead of crashing. No code pointers are stored
(rt_abi_async.md §13.5): `transaction(fn)` is Velt over `begin` / `end` and the failure counter.

Errors: `err.code` is only the failure flag (the rt_abi_async.md §3 code when an I/O error caused it, else 99);
`message` is JSON `{"code":…,"message":…,"detail":…|null,"constraint":…|null}` where `code` is
the SQLSTATE (`"23505"`) or a name: rt_abi_async.md §3 names (`"ECONNREFUSED"`, `"ETIMEDOUT"` …),
`"ECONNRESET"` (connection lost), `"ECLOSED"`, `"EINVAL"` (connection string, parameters, bad
transaction nesting), `"ETLS"`, `"ENOTSUP"` (undecodable column type), `"UNKNOWN"`.

Parameters (`params`) are JSON text (`velt_text::db_json`): `""` = none, an array (or scalar)
binds `$1..$n` (count must match), an object binds `:name` / `$name` placeholders, which are
rewritten to `$n` once per SQL text and cached (`::` casts, literals, quoted identifiers, dollar
quotes and comments are skipped; mixing with `$1` ⇒ `EINVAL`; missing member ⇒ `EINVAL`). Values
are converted for the prepared parameter types: Int ⇒ int2/int4/int8/oid (range-checked),
float4/float8, numeric, text types; Float ⇒ float4/float8, numeric, text types; Bool ⇒ bool,
text types; Bytes ⇒ bytea; Text ⇒ text types and json/jsonb in binary, any other type in text
format (parsed by the server); Null ⇒ NULL; anything else ⇒ `EINVAL`. For the parameter
types it can, velt/db's `bindParams` hands them to a sink instead (`bind_begin`, the contract native_abi.md "Parameter sinks"),
which keeps them as the JSON text would have parsed.
Rows come through a row cursor (the contract native_abi.md "Row cursors", decoded by
velt/db's `decodeRows`), the values as JSON text would give them (batches still return
JSON text, `db_json::RowWriter`): int2/int4/int8/oid exact, float4/float8 (non-finite
⇒ `null`), numeric ⇒ string of its digits, bool, text types/enums ⇒ string, uuid ⇒ string,
date/time/timestamp ⇒ ISO 8601 string, timestamptz ⇒ ISO 8601 UTC with `Z`, json/jsonb ⇒ the
document, bytea ⇒ number array, arrays ⇒ nested arrays, domains ⇒ base type, NULL ⇒ `null`;
other column types ⇒ `ENOTSUP` (checked before any row).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_postgres__connect` | `(const VeltStr* url) -> VeltFut*` | `IoResult<VeltPgClient>`; URL or libpq `key=value` string |
| `velt_postgres__query_rows` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params, u8 first_only) -> VeltFut*` | `IoResult<u64>`: a row cursor's handle (`open_cursor`) over the rows, given back to `close_rows`; `first_only` ⇒ the first row alone, or 0 if there is none |
| `velt_postgres__query_bound` | `(VeltPgClient c, const VeltStr* sql, u64 sink, u8 first_only) -> VeltFut*` | `query_rows` with the parameters a sink took |
| `velt_postgres__execute_bound` | `(VeltPgClient c, const VeltStr* sql, u64 sink) -> VeltFut*` | `execute` with the parameters a sink took |
| `velt_postgres__bind_begin` | `() -> u64` | a parameter sink's handle (`open_sink`) for velt/db's `bindParams<P>`, handed on to a `…_bound` call |
| `velt_postgres__bind_abort` | `(u64 sink)` | gives up a sink the Velt side did not use |
| `velt_postgres__close_rows` | `(u64 cursor, VeltErr* out)` | releases a row cursor; fails if a value could not be read; 0 ⇒ no-op |
| `velt_postgres__execute` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | `IoResult<i64>`: rows affected/returned |
| `velt_postgres__batch` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `VeltErr`: `;`-separated statements, simple protocol, not cached |
| `velt_postgres__begin` | `(VeltPgClient c) -> VeltFut*` | `IoResult<u32>`: new level (1 = `BEGIN`, n > 1 = `SAVEPOINT velt_tx_n`) |
| `velt_postgres__end` | `(VeltPgClient c, u32 depth, u8 commit) -> VeltFut*` | `VeltErr`: commit / roll back level `depth`, which must be the innermost (`EINVAL` otherwise); the level ends whatever the outcome |
| `velt_postgres__depth` | `(VeltPgClient c) -> u32` | open levels; 0 when closed |
| `velt_postgres__error_count` | `(VeltPgClient c) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_postgres__reset_error_count` | `(VeltPgClient c, u64 count)` | forget failures after `count` |
| `velt_postgres__error_after` | `(VeltPgClient c, u64 count, VeltErr* out)` | the first failure after `count` failures (code 0 if none) |
| `velt_postgres__note_error` | `(VeltPgClient c, const VeltStr* code, const VeltStr* message)` | counts a failure the Velt side found (`EMISMATCH` row decoding) |
| `velt_postgres__close` | `(VeltPgClient c)` | closes (or returns a pooled connection to its pool unless broken or in a transaction) and releases the handle; null ⇒ no-op |
| `velt_postgres__pool_new` | `(const VeltStr* url, u32 max, IoResult<VeltPgPool>* out)` | parses the URL and reads `sslrootcert` now; connections open on demand; `max == 0` ⇒ 10 |
| `velt_postgres__pool_connect` | `(VeltPgPool p) -> VeltFut*` | `IoResult<VeltPgClient>`: a dedicated connection (holds a permit) until `velt_postgres__close` |
| `velt_postgres__pool_query_rows` / `_pool_query_bound` / `_pool_execute_bound` | as the client's, with `VeltPgPool p` | on a borrowed connection |
| `velt_postgres__pool_execute` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | as `velt_postgres__execute` |
| `velt_postgres__pool_batch` | `(VeltPgPool p, const VeltStr* sql) -> VeltFut*` | as `velt_postgres__batch` |
| `velt_postgres__pool_idle` | `(VeltPgPool p) -> u32` | idle connections |
| `velt_postgres__pool_end` | `(VeltPgPool p)` | closes idle connections, fails later acquires with `ECLOSED`, releases the handle; null ⇒ no-op |

Linking: tokio-postgres' `whoami` needs `-framework SystemConfiguration -framework
CoreFoundation` on macOS and `secur32.lib` on Windows (added to `velt_link`'s native library
lists). Not available on WebAssembly: a package with native code is an error there. In a release build
the library is a prelinked object that brings no system libraries of its own: `velt_link` adds
the ones above to every executable.

## COPY
Writers (`VeltPgCopyIn`) and readers (`VeltPgCopyOut`) are registry keys (`u64`, rt_abi_async.md §3.2).
Errors are the JSON `PgError` of "PostgreSQL" above (`ECLOSED` for a released handle); failures count
against the client.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_postgres__copy_from` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyIn>`: `COPY … FROM STDIN` started |
| `velt_postgres__copy_write` / `_write_bytes` | `(VeltPgCopyIn w, const VeltStr*/VeltBytes* data) -> VeltFut*` | `IoResult<()>`; copied, batched into ~4 KiB messages |
| `velt_postgres__copy_end` | `(VeltPgCopyIn w) -> VeltFut*` | `IoResult<i64>` rows copied; releases `w` |
| `velt_postgres__copy_abort` | `(VeltPgCopyIn w)` | releases `w` unfinished: the server aborts the copy |
| `velt_postgres__copy_to` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyOut>`: `COPY … TO STDOUT` started |
| `velt_postgres__copy_read` / `_read_bytes` | `(VeltPgCopyOut r) -> VeltFut*` | `IoResult<VeltStr / VeltBytes>`: every chunk received so far (≥ 1, up to ~64 KiB), empty at the end |
| `velt_postgres__copy_close` | `(VeltPgCopyOut r)` | releases `r` (unread data is discarded) |

## Batches
One prepared statement run with N parameter sets in one message group — `Bind` + `Execute` per
set and **one** `Sync` (pgx's `Batch`): one round trip and one implicit transaction on the server
instead of N. tokio-postgres has no API for it, so every connection now runs over a wire stream
(`postgres/wire`) between tokio-postgres and the socket/TLS stream that injects the group between
two driver requests and routes the server's replies up to its `ReadyForQuery` back to the batch.
The runtime opens the socket itself for that (`Config::connect_raw`): hosts/`hostaddr`s/ports in
order, `connect_timeout`, keepalives; `target_session_attrs` and `load_balance_hosts` are not
supported. The statement is prepared by tokio-postgres (types, cache) and a second time under a
batch name (`velt_b<n>`, `Parse` at the head of the first group; closed when evicted).

`sets` is JSON: an array with one element per execution, each a parameter set as "PostgreSQL" above
`params` (array ⇒ `$1..$n`, object ⇒ named placeholders, scalar ⇒ one parameter; the first
element decides named vs positional). `[]` ⇒ `"[]"` without a server round trip. Errors are the
"PostgreSQL" above JSON `PgError`; when an execution fails, the server skips the rest of the group, so the
whole batch fails with that error (its SQLSTATE) and, outside a transaction, the group's
earlier writes are rolled back with it (all or nothing). Failures count against the client.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_postgres__query_batch` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | `IoResult<VeltStr>`: a JSON array, per execution: `mode` 0 its rows (`[[{…},…],…]`), 1 its first row or `null` (`[{…},null]`), 2 its rows affected (`[1,0]`) |
| `velt_postgres__pool_query_batch` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | as `velt_postgres__query_batch`, on one pooled connection |
