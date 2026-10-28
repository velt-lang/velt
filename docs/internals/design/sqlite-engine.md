# Design: a SQLite engine in Velt, fastest in class and multithreaded

Status: proposed; step 1 (reading the file format) is in progress.
Tracking: #1726, with one sub-issue per step and per language feature.

## Where we are

`velt:sqlite` ([docs](../../std/sqlite.md)) wraps C SQLite through `crates/velt_std_sqlite`
(rusqlite, bundled). Rows decode into typed values through `__intrinsic_read_rows`, which
calls `sqlite3_column_*` once per value across the FFI. That is fast, but it can't get faster:
every statement runs SQLite's bytecode interpreter (VDBE) behind an opaque call that LLVM can't
inline, and one connection runs on one thread.

The goal: replace the C library with an engine written in Velt that reads and writes the same
files (a database written by either can be opened by the other, with `sqlite3` alongside),
answers queries the way SQLite does, and is faster than C SQLite, better-sqlite3, bun:sqlite
and rusqlite on `bench/db` and `bench/sqlite`. No Rust: no `velt_std_sqlite`, nothing new in
`velt_rt`. Where Velt can't express something, or can't express it fast enough, that is a
language or compiler feature to add, which then helps every program
([velt-in-velt](json-engine.md#rule-the-engine-is-written-in-velt)).

## Where the speed comes from

| Idea | Why C SQLite can't | Step |
|---|---|---|
| Rows decode straight from page bytes into `T`'s fields, by code generated for `T` | `sqlite3_column_*` is a call per value, through a `Mem` cell | 3 |
| Plans run as compiled Velt code (closures composed at `prepare`), not a bytecode loop | VDBE dispatches every opcode | 3 |
| SQL that is a constant is planned when the program is compiled | `sqlite3_prepare` parses at run time | 7 |
| Readers on many threads share one page cache of immutable pages, without locks | one connection = one thread; the page cache is per connection | 5 |
| One query runs on several cores: scans, aggregates and sorts split by b-tree subtree | single-threaded by design | 6 |
| Pages are read with `pread` into the cache, or mapped (`mmap`) when the file allows | the same (mmap off by default) | 2 |

## Compatibility

- The file format is SQLite 3's, exactly: header, b-tree pages, record format, freelist,
  pointer maps (auto-vacuum), the rollback journal and the WAL with its `-shm` index. Locks are
  POSIX advisory locks at SQLite's byte offsets (`PENDING_BYTE`…), so C SQLite processes and
  Velt processes can use one database at the same time.
- SQL is SQLite's dialect, with its type affinity, NULL handling, collations, and its
  core, date and aggregate functions. What is not supported yet fails to prepare with an error
  naming it; it never answers differently.
- Checked by differential tests against the `sqlite3` CLI: databases written by Velt pass
  `PRAGMA integrity_check` in `sqlite3`; queries over generated schemas and data give the same
  rows (a fuzzer like `bench/json/fuzz`); the SQL Logic Test corpus as it becomes reachable.

## Language features this needs

Found so far; each gets its own issue and lands in the compiler, not in the engine:

1. **OS calls from Velt code, std and packages alike** (step 2): positioned reads and writes,
   `fsync`, `ftruncate`, `fcntl` locks, `mmap`, on POSIX and Windows. Today std reaches the OS
   only through `velt_rt_*` functions written in Rust, and a package only through its own
   native library. Decided: any module, including a package's, may `declare` the system's C
   library functions (libc, kernel32), with buffer parameters as `u8[]` ranges lowered to
   pointer + length. Velt moves off Rust onto Velt: what std can do, a package can do, so the
   engine can ship as the `sqlite` package without a native library.
2. **A string from a byte range** (step 1, #1677 step 3): `text(i)` copies the bytes into a
   new `u8[]` and then into a string (two copies, the second in `velt_rt`). It should be one
   copy and a UTF-8 check written in Velt.
3. **Byte views**: a page as a borrowed `u8[]` range of the cache, without a copy and without
   the bounds check per byte that `data[at + i]` costs now.
4. **Atomics and locks** (in progress elsewhere): the shared page cache and WAL index.

## Feature 1 in detail: system calls from Velt (#1716)

Today a `declare function` outside std must name an export of its package's native library
(`crates/veltc/src/native/mod.rs`, `check_declares`), and std declares only `velt_rt_*`. The
proposal keeps Velt memory-safe without a runtime in between:

```ts ignore
// Any module, std or package.
declare system function pread(fd: i32, buf: mut u8[], offset: i64): isize;  // libc
declare system function pwrite(fd: i32, buf: u8[], offset: i64): isize;
declare system function fsync(fd: i32): i32;
declare system function fcntl(fd: i32, cmd: i32, lock: mut Flock): i32;   // a #[repr(C)] struct
```

- **`declare system function`** binds a symbol of the platform's C library (libc, libSystem,
  kernel32/ntdll on Windows, WASI on wasm32). The compiler keeps a table of the symbols it
  allows per target, with their C signatures, and checks every declaration against it, as it
  checks native packages' exports today: a wrong signature is a compile error, not a crash.
- **Buffers** are `u8[]` (read) or `mut u8[]` (written) parameters, or a range of one
  (`buf.subarray(at, n)` as a borrowed view, feature 3). One Velt parameter lowers to the C
  pointer and length pair that follows it in the table's signature (`void *buf, size_t
  count`), so the length always comes from the array: no call can read or write past it. A
  written buffer's bytes past what the call reports stay as they were.
- **Structs** passed by pointer (`struct flock`, `struct stat`) are Velt `struct`s whose layout
  the table states per target; the compiler checks field order, types and size.
- **Results** are scalars; `errno` (`GetLastError`) is read by `lastOsError()` right after the
  call, which the compiler keeps adjacent (no allocation or other call in between).
- **What stays out**: raw pointers, callbacks into Velt, and functions that keep a buffer after
  they return (`aio_read`). `mmap` is not a declaration: it is a std type, `MappedFile`, whose
  pages are byte views (feature 3) that live as long as the mapping.
- A package's use of system functions is listed by `velt check` and in `velt.lock.json`, so a
  user sees which packages touch the OS directly.

This replaces the Rust in `std/fs` (`velt_rt_fs_*`) over time; std's file API is rewritten on
it, starting with positioned I/O for the pager.

## Steps

1. **Reading the file format** (`velt:sqlite/format`, `velt:sqlite/btree`): header, table and
   index b-tree cursors (first, next, seek by rowid), records, overflow chains, `sqlite_schema`.
   The whole file in one `u8[]`. Tested against files `sqlite3` writes.
2. **The pager and the OS layer**: page cache over `pread`, locks, change counter; files larger
   than memory.
3. **SQL**: tokenizer, parser, name resolution against the schema, a planner (rowid and index
   lookups and ranges, nested-loop joins, ORDER BY through an index or a sort, LIMIT), and
   execution as compiled closures. Expressions with SQLite's affinity and comparison rules,
   core functions, aggregates, subqueries. Typed row decoding replaces `__intrinsic_read_rows`
   for the Velt engine.
4. **Writing**: INSERT, UPDATE, DELETE, b-tree balancing, the freelist, the rollback journal,
   transactions, DDL. Then `velt:sqlite` switches to the Velt engine and `velt_std_sqlite` is
   deleted.
5. **WAL and concurrent readers**: the WAL file and `-shm` index, snapshots per reader, a
   page cache shared between threads.
6. **Parallel queries**: split a scan by subtrees of the root, run the parts as tasks, merge
   aggregates and sorted runs.
7. **Constant SQL compiled with the program**.

## Decided

- The engine may live in std or in a package: OS calls are a language feature for every
  module (feature 1), not a std privilege. `velt:sqlite`'s planned move to a package needs no
  native code once the engine is in Velt.
