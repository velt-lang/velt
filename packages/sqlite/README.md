# sqlite

`import { open, Database, Statement, SqliteError } from "sqlite"`. Embedded SQLite, compiled
into the package's native library (Rust over rusqlite, no system library), with an API close to
better-sqlite3. The package also holds a SQLite engine written in Velt, `sqlite/engine`
(read-only for now: [docs/engine.md](docs/engine.md)).

```ts ignore
// package.vlt
dependencies: {
  sqlite: "0.2",                                  // from the registry: a prebuilt library
  // sqlite: { path: "../velt/packages/sqlite" },  // or a checkout: cargo builds the library
},
```

From the registry, `velt` downloads the library prebuilt for your platform: no Rust toolchain is
needed. A path dependency (this repository's examples use one) builds the crate in `native/` with
cargo the first time, into `packages/sqlite/target/velt-native/`, and again when it changes
([Packages with native code](../../docs/book/native-packages.md)). It is
**synchronous**: a typical query takes microseconds, which is less than handing it to a thread
pool and back would cost. Run a long query inside `spawn(...)`.

- `open(path, opts = {}): Database`. `path` can be a file, `":memory:"` or a `file:` URI.
  Options: `readonly`, `fileMustExist` (throws `SQLITE_CANTOPEN` instead of creating the file),
  `timeout` (busy timeout in ms, default 5000) and `wal` (`journal_mode = WAL`).
- `Database` is a handle, so you can pass it around and capture it in handlers and tasks.
  Methods: `exec(sql)` (a script, no parameters), `prepare(sql): Statement`,
  `pragma(source): string` (first value, for example `"wal"`), `inTransaction`,
  `transaction(fn)`, `begin(): Transaction` and `close()`.
- `Statement`: `run(): RunResult { changes, lastInsertRowid }`, `get<T>(): T | null`,
  `all<T>(): T[]`, plus `runWith(params)`, `getWith<T>(params)` and `allWith<T>(params)`, and
  `close()`. `T` is usually inferred from the annotation (`const u: User | null = s.getWith(…)`).
  A statement releases itself when dropped, and preparing the same SQL again is a cache hit.
- **Parameters** bind as their `JSON.stringify` text would (velt/db's `bindParams` hands
  numbers, strings, bools, `null` and `u8[]` to SQLite directly, without making the text). An object binds
  `:name`/`@name`/`$name` by field name (extra fields are ignored; a missing one throws
  `SQLITE_RANGE`). An array, or a single number or string, binds `?` by position. Mapping:
  integer → INTEGER, float → REAL, string → TEXT, bool → 0/1, null → NULL, `u8[]` → BLOB.
- **Rows** are decoded by velt/db's `decodeRows` (as `JSON.parse<T>` would decode them), so a
  column fills the field with the same name
  (rename with `AS`). INTEGER goes into integer fields exactly (all of i64) and into `f64`
  fields, REAL into `f64`, TEXT into `string`, BLOB into `u8[]`, NULL into `T | null`. A row
  that doesn't fit `T` throws `SQLITE_MISMATCH`.
- `transaction(fn)` (`fn: () => T throws E`) commits after `fn` returns and returns its
  result. If `fn` throws, or any operation on the connection failed in the meantime (even one
  `fn` caught), it rolls back and throws that error: the transaction is all-or-nothing. Nested
  calls use savepoints: a failed inner transaction undoes only its own work.
  `begin()` returns a `Transaction` with `commit()` and `rollback()` for explicit control.
- `SqliteError { code, message }`: `code` is SQLite's name, for example `"SQLITE_ERROR"`,
  `"SQLITE_CONSTRAINT_UNIQUE"`, `"SQLITE_BUSY"`, `"SQLITE_RANGE"` (parameters) or
  `"SQLITE_MISUSE"` (closed handle, several statements passed to `prepare`).

```ts
import { open } from "sqlite";

class User {
  id: i64;
  name: string;
  constructor(id: i64, name: string) {
    this.id = id;
    this.name = name;
  }
}

async function main() {
  const db = open(":memory:");
  db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)");
  const insert = db.prepare("INSERT INTO users (name) VALUES (:name)");
  console.log(insert.runWith({ name: "Ann" }).lastInsertRowid); // 1
  db.transaction(() => {
    for (const name of ["Bob", "Cy"]) {
      try {
        insert.runWith({ name: name });
      } catch (e) {
        console.log(e.code); // any failure here rolls the whole transaction back
      }
    }
    return 0;
  });
  const users: User[] = db.prepare("SELECT id, name FROM users ORDER BY id").all();
  const bob: User | null = db.prepare("SELECT * FROM users WHERE name = ?").getWith(["Bob"]);
  console.log(users.length, bob?.id); // 3 2
  db.close();
}
```

Notes: SQLite has no boolean type. A column declared `BOOLEAN` decodes into a `bool` field; a
0/1 from an expression needs an integer field. `JSON.stringify` writes a whole-number `f64`
such as `3.0` as `3`, so it is bound as INTEGER. A `REAL` column converts it back, but a column
with no declared type keeps it as an integer. A connection is one SQLite connection with a lock
around each call: tasks can share a `Database`, but their statements run one at a time and
inside any transaction that is open. For parallel readers, open one connection per task with
`wal: true`. After `close()` through one copy of a `Database`, every copy throws
`SQLITE_MISUSE` (closing again does nothing). WebAssembly isn't supported (a native library
can't be used there).

## Layout

- `src/lib.vlt`: the API (re-exports); `src/database.vlt`, `statement.vlt`, `transaction.vlt`,
  `run_result.vlt`, `errors.vlt`: one type each; `src/native.vlt`: the native library's
  functions, the only `declare`s.
- `native/`: the Rust crate (`velt_native_sqlite`), on the native SDK (`velt_native`) and
  `velt_text` (row cursors and parameter sinks); its ABI is [docs/native_abi.md](docs/native_abi.md).
- `src/engine/`: the SQLite engine in Velt ([docs/engine.md](docs/engine.md); the design is
  [sqlite-engine.md](../../docs/internals/design/sqlite-engine.md)).
- `tests/`: `velt test` in this directory runs them (the repository runs it for every package,
  `crates/veltc/tests/packages.rs`).

The package uses only public std APIs: velt/db (`decodeRows`, `bindParams`, `readRows`),
velt/native (`IoResult`), velt/encoding (`utf8ByteAt`, `utf8Length`), velt/fs.

## From the prototype (0.1)

The first `packages/sqlite` (0.1.0) was a prototype of native packages with a smaller API of its
own. 0.2 is the standard library's former `velt/sqlite` instead, and the prototype's API is gone:

| 0.1 | 0.2 |
|---|---|
| `new Database(path)` | `open(path, opts)` (or `Database.open`) |
| `db.query<T>(sql, "[1, \"two\"]")` (parameters as JSON text) | `db.prepare(sql).allWith<T>([1, "two"])` (parameters as values) |
| `db.exec(sql)` | `db.exec(sql)` |
| `db.ref.query<T>(…)` (async, on the blocking pool) | `spawn(() => db.prepare(sql).all<T>())`: queries are synchronous |
| `version()` | `db.prepare("SELECT sqlite_version() AS v").get<V>()` |
| `Database` disposed with `using` | `db.close()`; a `Statement` releases itself when dropped |
