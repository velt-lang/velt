# The sqlite package's native library

The package's native library (`native/`, crate `velt_sqlite_native`, library `velt_native_sqlite`)
over `rusqlite` (SQLite compiled in: `bundled`; also `column_decltype`), built on the
native-package SDK ([native_abi.md](../../../docs/internals/contracts/native_abi.md)): its
functions are `velt_sqlite__*` exports with the signatures below, declared in `src/native.vlt`
(the compiler checks each declaration against the library's signature records).
**Synchronous**: a typical statement takes microseconds, less than a blocking-pool round trip. A database is a registry key of `Mutex<Option<Connection>>` plus a
failure counter and the last error; a statement is a key of `{ Arc<db>, sql }` that re-finds
its compiled statement in the connection's cache (`prepare_cached`, 256 entries) on every call,
so closing the database while statements exist is safe (they fail with `SQLITE_MISUSE`). A
null or closed handle (a closed `Database` / `Statement`) fails the same way instead of
crashing.

Errors: `err.code` is SQLite's **extended result code** (never 0; e.g. 2067
`SQLITE_CONSTRAINT_UNIQUE`), `message` SQLite's message. Parameter problems are `SQLITE_RANGE`
(25), a closed handle or several statements in one `prepare` `SQLITE_MISUSE` (21).

Parameters (`params`) are JSON text (`velt_text::db_json`): `""` = none, an object binds
`:name` / `@name` / `$name` by name (missing member ⇒ `SQLITE_RANGE`, extra members ignored), an
array or a lone scalar binds by position (count must match). Integer literals in i64 range ⇒
INTEGER (exact), other numbers ⇒ REAL, strings ⇒ TEXT, `true`/`false` ⇒ 1/0, `null` ⇒ NULL,
arrays of integers 0–255 (`u8[]`) ⇒ BLOB, nested objects ⇒ `SQLITE_RANGE`. For the parameter
types it can, velt/db's `bindParams` hands the parameters to a sink instead (`bind_begin`, native_abi.md "Parameter
sinks"), which binds them as their JSON text would. Rows come through a row cursor
(native_abi.md "Row cursors"), which velt/db's `decodeRows<T>` decodes: they
read as the JSON `[{"col":value,...}]` keyed by column name, INTEGER exact, REAL a number
(non-finite ⇒ `null`), TEXT a string, BLOB a number array, NULL `null`; a 0/1 value in a column
declared `BOOL`/`BOOLEAN` is `false`/`true`. The cursor holds the connection's lock until the Velt side
gives it back with `close_rows`, as a sink does until it is handed on or given up.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_sqlite__open` | `(string path, bool readonly, bool create, u32 timeout_ms) -> IoResult<u64>` | `":memory:"`, a file path or a `file:` URI; `timeout_ms` = busy timeout |
| `velt_sqlite__close` | `(u64 db) -> IoStatus` | closes the connection (finalizing cached statements) and releases the handle; null or closed ⇒ no-op |
| `velt_sqlite__exec` | `(u64 db, string sql) -> IoStatus` | `;`-separated statements, no parameters |
| `velt_sqlite__pragma` | `(u64 db, string source) -> IoResult<string>` | `PRAGMA <source>`: first column of the first row as text, `""` if none/NULL/blob |
| `velt_sqlite__in_transaction` | `(u64 db) -> bool` | not in autocommit mode; false when closed |
| `velt_sqlite__error_count` | `(u64 db) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_sqlite__reset_error_count` | `(u64 db, u64 count)` | a nested `transaction` that rolled back hands its failure to its caller |
| `velt_sqlite__last_error` | `(u64 db) -> IoStatus` | the most recent failure as a failed status (success if none) |
| `velt_sqlite__prepare` | `(u64 db, string sql) -> IoResult<u64>` | compiles exactly one statement; the connection keeps it per SQL text (preparing the same text again compiles nothing) |
| `velt_sqlite__run` | `(u64 stmt, string params) -> IoResult<{changes:i64,lastInsertRowid:i64}>` | steps to completion (rows discarded) |
| `velt_sqlite__open_rows` | `(u64 stmt, string params, bool first_only) -> IoResult<u64>` | binds, steps to the first row: a row cursor's handle (`open_cursor`) for `decodeRows<T>`; `first_only` ⇒ the first row alone (`decodeRow<T>`), or 0 if there is none |
| `velt_sqlite__close_rows` | `(u64 cursor) -> IoStatus` | releases a cursor (and the connection's lock); fails with what SQLite met while stepping; 0 ⇒ no-op |
| `velt_sqlite__bind_begin` | `(u64 stmt) -> IoResult<u64>` | a parameter sink's handle (`open_sink`) for `bindParams<P>` |
| `velt_sqlite__open_rows_bound` | `(u64 sink, bool first_only) -> IoResult<u64>` | `open_rows` with the parameters the sink bound (missing or extra ones fail as with JSON text) |
| `velt_sqlite__run_bound` | `(u64 sink) -> IoResult<{changes:i64,lastInsertRowid:i64}>` | `run` with the parameters the sink bound |
| `velt_sqlite__bind_abort` | `(u64 sink)` | gives up a sink the Velt side did not use (it then binds JSON text) |
| `velt_sqlite__note_error` | `(u64 stmt, i32 code, string message)` | counts a failure the Velt side found (a row that does not decode) against the connection |
| `velt_sqlite__stmt_free` | `(u64 stmt)` | releases the handle; null ⇒ no-op |
| `velt_sqlite__error_name` | `(i32 code) -> string` | known extended names (`SQLITE_CONSTRAINT_UNIQUE`, `SQLITE_BUSY_SNAPSHOT` …), else the primary one |

Not available on WebAssembly: a package with native code is an error there.
