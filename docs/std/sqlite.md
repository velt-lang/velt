# velt:sqlite

> This driver is moving from the standard library to a separately versioned package. The API
> stays the same.

`import { open, Database, Statement, SqliteError } from "velt:sqlite"`. Embedded SQLite, compiled
into the runtime (no system library), with an API close to better-sqlite3. It is
**synchronous**: a typical query takes microseconds, which is less than handing it to a thread
pool and back would cost. Run a long query inside `spawn(...)`.

- `open(path, opts = {}): Database`. `path` can be a file, `":memory:"` or a `file:` URI.
  Options: `readonly`, `fileMustExist` (throws `SQLITE_CANTOPEN` instead of creating the file),
  `timeout` (busy timeout in ms, default 5000) and `wal` (`journal_mode = WAL`).
- `Database` is a Copy handle, so you can pass it around and capture it in handlers and tasks.
  Methods: `exec(sql)` (a script, no parameters), `prepare(sql): Statement`,
  `pragma(source): string` (first value, for example `"wal"`), `inTransaction`,
  `transaction(fn)`, `begin(): Transaction` and `close()`.
- `Statement`: `run(): RunResult { changes, lastInsertRowid }`, `get<T>(): T | null`,
  `all<T>(): T[]`, plus `runWith(params)`, `getWith<T>(params)` and `allWith<T>(params)`, and
  `close()`. `T` is usually inferred from the annotation (`const u: User | null = s.getWith(…)`).
  A statement releases itself when dropped, and preparing the same SQL again is a cache hit.
- **Parameters** are a value that std passes through `JSON.stringify`. An object binds
  `:name`/`@name`/`$name` by field name (extra fields are ignored; a missing one throws
  `SQLITE_RANGE`). An array, or a single number or string, binds `?` by position. Mapping:
  integer → INTEGER, float → REAL, string → TEXT, bool → 0/1, null → NULL, `u8[]` → BLOB.
- **Rows** are decoded with `JSON.parse<T>`, so a column fills the field with the same name
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
import { open } from "velt:sqlite";

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
`wal: true`. A copy of a `Database` that was closed through another copy must not be used
(like velt:net sockets). WebAssembly isn't supported.
