# A SQLite engine in Velt

Status: partly implemented. Issue: [#1726](https://github.com/velt-lang/velt/issues/1726).

## Summary

`sqlite/engine`, in the `sqlite` package (`packages/sqlite/src/engine/`, user documentation in
[packages/sqlite/docs/engine.md](../../../packages/sqlite/docs/engine.md)), is a SQLite engine
written in Velt. It reads SQLite 3 database files and answers SELECT statements the way C SQLite
does, with the statement API of the package's driver. The design extends it to write the same
files, to share one page cache between readers on many threads, and to split one query across
cores, so that the `sqlite` package needs no C library.

The engine contains no Rust and adds nothing to `velt_rt`. Where Velt cannot express something,
or cannot express it fast enough, the fix is a language or compiler feature, which then helps
every program; the same rule governs the JSON engine
([json-engine.md](json-engine.md#rule-the-engine-is-written-in-velt)).

## Motivation

The package's driver (`packages/sqlite/src/sqlite3.velt`, `statement.velt`) calls C SQLite,
compiled with the package from the amalgamation in `packages/sqlite/native/`, through
`declare c("sqlite3")` ([c-ffi.md](c-ffi.md)). Rows decode into typed values through `velt/db`'s
`readRows`, one `sqlite3_column_*` call per value. That cost has a floor: every statement runs
SQLite's bytecode interpreter (VDBE) behind calls LLVM cannot inline, and a connection is used
by one thread at a time.

An engine in Velt is compiled together with the program that uses it, so the code that reads a
row, compares a key or evaluates an expression is inlined and specialized like the program's own
code.

## Design

### Compatibility

- **File format.** SQLite 3's exactly: the header, table and index b-tree pages, the record
  format and overflow chains, and, once the engine writes, the freelist, pointer maps
  (auto-vacuum), the rollback journal and the WAL with its `-shm` index. The engine takes no
  locks yet; once it writes, its locks are POSIX advisory locks at SQLite's byte offsets, so C
  SQLite processes and Velt processes can share a database.
- **SQL.** SQLite's dialect, with its type affinity, NULL handling, collations, integer overflow
  to REAL, the text form of REAL values, and its core, date and aggregate functions. SQL the
  engine cannot run fails at `prepare` with an error naming what is unsupported; the engine
  never answers differently from SQLite.
- **Errors.** The engine throws the driver's `SqliteError` with SQLite's codes and messages
  (`no such column: x`, `misuse of aggregate: count()`), so one `catch` handles both.
- **Row order.** Rows a query does not order come in the order SQLite's plan gives them where
  the engine picks the same plan. Where SQLite's cost model picks another plan, they can come in
  another order, and DISTINCT or GROUP BY can then show another row of a group for its bare
  columns.

Compatibility is checked by the package's tests (`packages/sqlite/tests/engine_*.test.velt`)
over databases C SQLite writes, and by a differential fuzzer (`bench/sqlite/fuzz.velt`) that runs
random schemas, data and SELECTs through the engine and through the `sqlite3` shell and requires
identical rows.

### Where the speed comes from

| Idea | Why C SQLite cannot | Built |
|---|---|---|
| Plans run as closures composed at `prepare`, not a bytecode loop | VDBE dispatches every opcode | yes |
| Rows decode straight from page bytes into `T`'s fields, by code generated for `T` | `sqlite3_column_*` is a call per value, through a `Mem` cell | no |
| Readers on many threads share one page cache of immutable pages, without locks | the page cache is per connection | no |
| One query runs on several cores: scans, aggregates and sorts split by b-tree subtree | single-threaded by design | no |
| SQL that is a constant is planned when the program is compiled | `sqlite3_prepare` parses at run time | no |

### API

`openDatabase(path)` reads a database file into memory; `new Database(bytes)` takes its bytes.
`Database.prepare(sql)` parses and plans one SELECT and caches the plan by its text (the 200 used
last). A `Statement` has the driver's `all<T>()`, `get<T>()`, `allWith<T>(params)` and
`getWith<T>(params)`, plus `columns()`, `values()` (every value as a `Val`) and `plan()`, which
describes each FROM item in the words of SQLite's `EXPLAIN QUERY PLAN`.

## Compilation

The engine is ordinary Velt code in the package; the compiler has no knowledge of it.

| Part | Modules (`packages/sqlite/src/engine/`) |
|---|---|
| File and records | `db_file.velt` (the header, pages as offsets into the file's bytes), `format.velt`, `varint.velt`, `record*.velt`, `serial_sizes.velt` |
| B-trees | `cursor.velt`, `btree_*.velt`: table and index cursors (first, next, seek by rowid and by key), overflow chains |
| Schema | `schema/`: `sqlite_schema` read into tables, indexes and views |
| SQL | `sql/`: lexer, parser and AST of SQLite's statements |
| Planning | `exec/plan/`: rowid and index lookups and ranges, covering indexes, `OR` as `IN`, ORDER BY through an index or a sort |
| Execution | `exec/`: expressions compiled into closures with SQLite's affinity, collation and NULL rules (`expr_compiler.velt`), nested-loop joins (`exec/loop/`), aggregates, window functions, subqueries, CTEs (recursive included) and compound selects |

Rows fill `T` by column name through `velt/db`'s `readRows`, from the engine's values with no
text in between.

## Language features it relies on

The engine as built is ordinary Velt: it reads the file with `velt/fs`'s `readFileBytesSync` and
uses no OS calls or atomics of its own. The parts not yet implemented rely on:

- **OS calls from any module.** std and packages alike call the platform's C library through
  `declare c("c")` and `unsafe` ([c-ffi.md](c-ffi.md)), with arrays and their `subarray` views
  as buffers. std builds positioned reads (`std/fs/fd/`) and `MappedFile` (`mmap`,
  `std/fs/mapped_file.velt`) on them; the pager is to use the same mechanism, so the engine
  ships in a package without a native library.
- **Atomics** ([atomics.md](atomics.md)) for the shared page cache and the WAL index.

## Limitations

- The engine is read-only. The parser reads DDL, DML and PRAGMA statements too, but `prepare`
  runs one SELECT only and refuses any other statement with `only SELECT statements are
  supported yet` (`SQLITE_ERROR`).
- The whole database file is held in memory as one `u8[]`; files larger than memory cannot be
  opened.
- A database in WAL mode reads correctly only as checkpointed: the `-wal` file is not read.
- `text(i)` copies a column's bytes into a new `u8[]` and then into a string: two copies.

## Not yet implemented

1. **The pager and the OS layer.** A page cache over positioned reads, or a mapping when the
   file allows; locks and the change counter. `DbFile.page(n)` is the single point every reader
   goes through, so the cache replaces the in-memory file there.
2. **Writing.** INSERT, UPDATE, DELETE, b-tree balancing, the freelist, the rollback journal,
   transactions and DDL. When the engine writes, the driver runs on it and the package drops its
   C library.
3. **WAL and concurrent readers.** The WAL file and its `-shm` index, a snapshot per reader, and
   a page cache of immutable pages shared between threads.
4. **Parallel queries.** A scan split by subtrees of the root, the parts run as tasks, and
   aggregates and sorted runs merged.
5. **Constant SQL compiled with the program.** A `prepare` whose SQL is a constant is parsed and
   planned at compile time.
6. **Typed row decoding.** Code generated per `T` reads a row's fields straight from page bytes,
   replacing the decoding through `Val`s.
7. **A string from a byte range** ([#1717](https://github.com/velt-lang/velt/issues/1717)): one
   copy and a UTF-8 check written in Velt, instead of a copy into a `u8[]` followed by the
   runtime's conversion.
8. **Byte views** ([#1718](https://github.com/velt-lang/velt/issues/1718)): a page as a borrowed
   range of the cache, bounds-checked once per range rather than once per byte.
9. **SQL Logic Test.** The SQL Logic Test corpus joins the differential tests as the engine's
   coverage reaches it.
