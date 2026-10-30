# System functions

`declare system function` binds a function of the platform's C library (libSystem on macOS,
glibc or musl on Linux, kernel32 on Windows) and calls it directly, with no runtime in between.
Any module may declare one: the standard library, a package or a program. Velt code stays
memory-safe because the compiler, not the declaration, decides what crosses: it keeps a table of
the functions it can call safely, with their C signatures on each target, and checks every
declaration against it.

```ts
declare system function pread(fd: i32, buf: u8[], offset: i64): isize;
declare system function pwrite(fd: i32, buf: readonly u8[], offset: i64): isize;
declare system function fsync(fd: i32): i32;
declare system function lastOsError(): i32;
```

[`velt/fs`](../std/fs.md#file-descriptors)'s `openSync`, `readSync`, `writeSync`, `fsyncSync`
and `fstatSync` are Velt code over these declarations.

## What crosses

| C | Velt declaration | at the call |
|---|---|---|
| an integer (`int`, `off_t`, `size_t`, `DWORD`, `HANDLE`) | the integer type the table names (`i32`, `i64`, `usize`, ...) | passed as it is |
| `void *buf, size_t len` that the function **writes** | one parameter `buf: u8[]` | the array's data and length |
| `const void *buf, size_t len` that it only **reads** | one parameter `buf: readonly u8[]` | the array's data and length |
| either, given a view | the same parameter | the bytes the view sees ([below](#views)) |
| `const char *path` | `path: readonly u8[]` | the array; unless its only 0 byte is its last, the program stops |
| `LPCWSTR` (Windows) | `name: readonly u16[]` | the array; unless its only 0 element is its last, the program stops |
| `struct flock *`, `struct stat *`, `OVERLAPPED *` | a `struct` of integers whose fields are named after the C struct's | copied into the C layout of the target, and back after the call |
| a pointer the declaration leaves out (`CreateFileW`'s `lpSecurityAttributes`) | nothing | `NULL` |
| the result | the integer type the table names | |

The length always comes from the array or the view, so no call reads or writes past its end. A buffer the
function writes is `u8[]` and one it only reads is `readonly u8[]` (TypeScript's `readonly`
array type): the declaration must say which, so a reader sees it and the compiler knows the
caller's array changes. A read-only array (a variable, parameter, module constant or class field
declared `readonly u8[]`, or a function's `readonly u8[]` result), whole or through `subarray`,
cannot be passed for a buffer the function writes: it is a compile error. A struct's fields may come in any order and may be a subset of the C
struct's (the others are 0 going in and ignored coming out); a C field narrower than the Velt
type is converted (`st_mode` is a `u16` on macOS and a `u32` in Velt).

```ts
struct Lock {
  l_type: i16;
  l_whence: i16;
  l_start: i64;
  l_len: i64;
  l_pid: i32;
}
declare system function fcntl$lock(fd: i32, cmd: i32, lock: Lock): i32;
```

A name may end in `$` and any suffix, which the table ignores: `fcntl$lock` binds `fcntl`. A
module that needs two forms of a variadic function (`fcntl` with an `int` and with a
`struct flock *`) declares them under two names; the parameter types choose the form.

## Views

A buffer parameter (`u8[]` or `readonly u8[]`, a path too) takes a view of bytes as well as an
array, and the call gets the address and length of the bytes the view sees, with no copy:

- a typed array (`Uint8Array`, `Float64Array`, ...) or a `DataView`: the bytes of its range of
  its `ArrayBuffer`, whatever its element type;
- a `subarray(begin, end)` of one;
- `xs.subarray(begin, end)` of a `u8[]` `xs` (Node's `buf.subarray` of a `Buffer`), with JS's
  positions: a negative one counts from the end, and both are clamped to the array. As an
  argument it borrows `xs` for the call (written, for a buffer the function writes), and no
  `Uint8Array` is made; elsewhere it is the `Uint8Array` view of the array
  ([prelude](../std/prelude.md)).

The array is the one `xs` holds when the argument is evaluated, before the positions and the
later arguments, as in JS: a later argument that gives the variable another array leaves the
call at the first one, which the call keeps alive until it returns. A whole array argument
names its array the same way. The array's data and length are taken at the call, after every
argument is evaluated, so an argument that grows the array (which moves its bytes) cannot leave
the call at bytes that were freed, and the positions are clamped to the length the array has
then.

```ts
declare system function pread(fd: i32, buf: u8[], offset: i64): isize;
declare system function pwrite(fd: i32, buf: readonly u8[], offset: i64): isize;

function readPage(fd: i32, cache: u8[], slot: usize, page: i64): isize {
  // Page `page` of the file into slot `slot` of the cache: 4096 bytes, no copy.
  return pread(fd, cache.subarray(slot * 4096, (slot + 1) * 4096), page * 4096);
}

function writeHeader(fd: i32, header: DataView): isize {
  return pwrite(fd, header, 0);
}
```

A typed array or `DataView` argument is the view the expression gives when it is evaluated,
kept alive for the call; its buffer's bytes are taken at the call too, after every argument, so
an argument after it that grows the `u8[]` under the view (`g.subarray(0, 4096)` held in a
variable or a field) cannot leave the call reading or writing freed bytes. A view of a buffer
whose `u8[]` is then shorter than the view's range (Velt's `ArrayBuffer` cannot shrink, but its
owner can, a later argument too) stops the program rather than reach past the array.

`lastOsError()` reads `errno` (`GetLastError()` on Windows) of the calling thread. The compiler
captures `errno` into a slot of the thread right after every system call, and `lastOsError()`
reads that slot, so a system call followed by `lastOsError()` reports that call's error even when
anything the statement does after the call (freeing a temporary argument, `console.log`) runs in
between, and even when the call was made in a function this one called; call `lastOsError()`
after the failing call and before the next system call, which overwrites the slot. Read it before any `await` too: the slot belongs to the thread, as C's `errno` does, so across an `await` other tasks' system calls (on this thread or, once resumed, another one) may have replaced it. Keep the value in a local if it is needed after the `await`. `velt/os` exports it (`import { lastOsError } from "velt/os"`).

```ts
import { lastOsError } from "velt/os";

declare system function close(fd: i32): i32;

if (close(-1) != 0) {
  const e = lastOsError();
  console.log(e); // 9: EBADF
}
```

## Checked at compile time

Every declaration is checked against the table of the target being compiled for. An unknown
name, a wrong parameter count, a wrong integer type, a pointer written as an integer, a buffer
declared with the wrong direction, a `string` where the function takes a path, a struct with a
field the C struct lacks, a function parameter (no callbacks), `async`, type parameters or a missing result are
errors that show the C prototype and the declaration that would match:

```text
error: `declare system function pread` does not match the C function `pread`
  = note: `pread` writes into `buf`: declare it `u8[]`, not `readonly u8[]`
  = note: C: `ssize_t pread(int fd, void *buf, size_t len, off_t offset)`; Velt: `declare system function pread(fd: i32, buf: u8[], offset: i64): isize;`
```

A function another target has but this one lacks (POSIX `pread` on Windows, anything on
WebAssembly) may be declared; a call of it in the program is the error, so portable code can
choose by platform:

```text
error: system function `pread` is not available on wasm32-wasip1
  = note: `main` calls it
  = note: this target has no system functions
```

A standard-library function that calls one is reported at the program's call of it:

```text
app.vlt:4:14: error: `openSync` (velt/fs) is not available on x86_64-pc-windows-msvc
  = note: it calls the system function `open`, which x86_64-pc-windows-msvc lacks
```

## Checked at the call

Some arguments change how the C function uses memory, so the call checks them and stops the
program (a panic) when they are wrong: a path without its 0 byte or with a 0 before it (C would end the path there: `"a.txt\0../b\0"`
would reach C as `a.txt`), a view outside its buffer,
an `fcntl` command of the other form (a lock command with an `int` argument would make the kernel
read a `struct flock` at that address), and `FILE_FLAG_OVERLAPPED` for `CreateFileW` (Windows
would finish reads and writes after the call returned).

On Windows, `ReadFile`, `WriteFile`, `LockFileEx` and `UnlockFileEx` are checked but cannot be
called yet: a call of one is a compile error, as for a function the target lacks. They take an
`OVERLAPPED`, and on a handle bound to an I/O completion port (the runtime binds pipes to one)
the kernel would queue a completion that points at it after the call returned; they become
callable once each call passes an event handle that keeps the completion off the port.

## What stays out

No raw pointers, no callbacks into Velt, and no function that keeps a buffer after it returns
(`aio_read`, `mmap`). Memory-mapped files are planned as a std type whose pages are byte views
([design](../internals/design/sqlite-engine.md#feature-1-in-detail-system-calls-from-velt-1716)).
File descriptors and handles are plain integers: closing one the runtime owns is not a memory
error, but it breaks what the runtime does with it, as it would in C.

## The table

| function | macOS, Linux | Windows |
|---|---|---|
| `open(path: readonly u8[], oflag: i32, mode: u32): i32` | ✓ (`mode` is the variadic argument) | |
| `close(fd: i32): i32` | ✓ | |
| `read(fd: i32, buf: u8[]): isize`, `write(fd: i32, buf: readonly u8[]): isize` | ✓ | |
| `pread(fd: i32, buf: u8[], offset: i64): isize`, `pwrite(fd: i32, buf: readonly u8[], offset: i64): isize` | ✓ | |
| `fsync(fd: i32): i32`, `fdatasync(fd: i32): i32`, `ftruncate(fd: i32, length: i64): i32` | ✓ | |
| `lseek(fd: i32, offset: i64, whence: i32): i64`, `unlink(path: readonly u8[]): i32` | ✓ | |
| `fcntl(fd: i32, cmd: i32, arg: i32): i32` | `F_GETFD`, `F_SETFD`, `F_GETFL`, `F_SETFL`; macOS also `F_NOCACHE`, `F_FULLFSYNC`, `F_BARRIERFSYNC` | |
| `fcntl(fd: i32, cmd: i32, lock: Flock): i32` | `F_GETLK`, `F_SETLK`, `F_SETLKW`; Linux also the `F_OFD_*` locks | |
| `fstat(fd: i32, st: Stat): i32` | `st_dev`, `st_ino`, `st_mode`, `st_nlink`, `st_uid`, `st_gid`, `st_rdev`, `st_size`, `st_blksize`, `st_blocks`, `st_atime`, `st_mtime`, `st_ctime` and their `_nsec` | |
| `CreateFileW`, `FlushFileBuffers`, `SetEndOfFile`, `SetFilePointerEx`, `GetFileSizeEx`, `CloseHandle` | | ✓ |
| `ReadFile`, `WriteFile` (with an `OVERLAPPED` for the offset), `LockFileEx`, `UnlockFileEx` | | checked, not callable yet |
| `lastOsError(): i32` | `errno` | `GetLastError()` |

WebAssembly targets have no system functions yet.
