# The Velt standard library

The standard library is written in Velt and ships with the toolchain. A module is imported as
`velt/<module>`, the path of its folder under `std/` (`velt:` specifiers were removed):

```ts ignore
import { readFile, writeFile } from "velt/fs";
import { Deque } from "velt/collections";
```

The [prelude](prelude.md) (strings, arrays, `Map`, `Math`, `JSON`, `Error`, promises, …) is in scope
everywhere without an import, and so are Node's web globals: [`fetch`](net/http.md), `Request`,
`Response`, `Headers`, `Blob`, `File`, `FormData`, `AbortController`, `AbortSignal`, `URL`,
`URLSearchParams`, `Set`, `RegExp`, [`Symbol`](prelude/symbol.md), `TextEncoder`, `TextDecoder`, `btoa`,
`atob`, `DOMException`, `structuredClone`, `ArrayBuffer`, `DataView`, `Uint8Array` and the other
[typed arrays](prelude/typed_array.md), and [`ReadableStream`](io/streams.md). `velt doc --std` generates HTML
API documentation from the sources.

## Conventions

- **Errors** are thrown classes extending the prelude `Error { message }`. Catch them with
  `try { … } catch (e) { e.message }`; `e` is the union of the error classes the calls can
  throw, narrowed with `instanceof`. Each module has its own error class (`UrlError`,
  `CsvError`, …). Messages start with a capital letter (`Invalid URL: …`); `IoError` messages
  are lowercase, as in Node (`ENOENT: no such file or directory, open 'x'`, `handle is
  closed`). An error that escapes `main` prints `Uncaught <Class>: <message> at
  <file>:<line>:<col>`, naming the error's actual class and, for an error thrown inside the
  standard library, the line of your code that called it.
- **I/O errors** are `IoError { code, message }` from [`velt/io`](io.md). `code` is a
  Node-style name: `"ENOENT"`, `"EACCES"`, `"ECONNREFUSED"`, `"EOF"`, … A failed file-system
  call has Node's message: `ENOENT: no such file or directory, open 'data.txt'`.
- **Strings** are sequences of UTF-16 code units, as in JS: lengths and positions (`slice`,
  `indexOf`, regex match offsets, CSV and CLI column widths) count code units. Their text is
  stored and written as UTF-8; `Buffer.byteLength(s)` is that size.
- **Async functions return promises that start at once**, like JS (a direct `await` costs
  nothing). A promise that is neither awaited nor spawned is a compile error. `*Sync` variants
  block the calling thread.
- **Handles**: some modules return handle structs (`TcpStream`, `FileReader`, `ChildProcess`,
  `Database`, …) that you can pass around and capture freely and release with `close()` (closing
  again does nothing). **Planned** ([semantics stage 2
  §7](../internals/design/semantics-stage2.md#7-identity-and-the-struct-keyword)): they become
  disposable classes with `using` support when the `struct` keyword is removed.

## Modules

Each module has a page here at the same path as its folder in `std/` (`velt/net/http` is
[net/http](net/http.md)); a module with several parts has a page for each below its own.
`velt doc --std` generates the API reference from the sources, and `velt doc --std --format
types` the TypeScript declarations ([the `velt` type
package](../tooling/cli.md#typescript-declarations-the-velt-type-package)).

| Module | Pages |
|---|---|
| [cli](cli.md) | command-line programs: arguments, subcommands, `--help`, exit codes |
| [collections](collections.md) | [BTreeMap](collections/btree_map.md) · [ConcurrentMap](collections/concurrent_map.md) · [Deque](collections/deque.md) · [PriorityQueue](collections/priority_queue.md) · [Set](collections/set.md) · [SortedMap](collections/sorted_map.md) |
| [compress](compress.md) | gzip, zlib, DEFLATE, Brotli |
| [crypto](crypto.md) | hashing, HMAC, secure random values |
| [db](db.md) | typed rows from any source |
| [dev](dev.md) | the graceful stop under `velt dev` |
| [encoding](encoding.md) | base64, hex, UTF-8 · [csv](encoding/csv.md) · [json](encoding/json.md) |
| [fs](fs.md) | files and directories · [file streams](fs/stream.md) |
| [generate](generate.md) | what source generators are written with |
| [hash](hash.md) | stable, non-cryptographic hashing |
| [io](io.md) | `IoError`, readers and writers · [streams](io/streams.md) (`ReadableStream`, global) |
| [jsx](jsx.md) | TSX rendering · [attrs](jsx/attrs.md) · [jsx-runtime](jsx/jsx-runtime.md) · [generic/jsx-runtime](jsx/generic/jsx-runtime.md) |
| [math](math.md) | [BigInt](math/bigint.md) |
| [mem](mem.md) | `needsDrop`, for containers · [arenas](mem/arena.md) |
| [native](native.md) | the Velt side of packages with native code |
| [net](net.md) | TCP and TLS · [dns](net/dns.md) · [http](net/http.md) (`fetch`, global) · [http/h2](net/http/h2.md) · [udp](net/udp.md) · [url](net/url.md) · [websocket](net/websocket.md) (the client) |
| [os](os.md) | platform facts |
| [package](package.md) | the type of `package.vlt` |
| [path](path.md) | paths |
| [process](process.md) | arguments, environment, exit · [standard input](process/stdin.md) · [child](process/child.md) (child processes) |
| [random](random.md) | a fast per-thread generator · [uuid](random/uuid.md) |
| [simd](simd.md) | the vector types and their methods |
| [sync](sync.md) | `Atomic<T>` (in the prelude), `Backoff`, `VersionLatch` · [channels](sync/channel.md) · [epoch](sync/epoch.md) (reclamation) |
| [task](task.md) | cancellation, timeouts, task scopes |
| [test](test.md) | the runner `velt test` builds into test programs · [http](test/http.md) (matchers for a `Response`) |
| [text](text.md) | [regular expressions](text/regex.md) · [StringBuilder](text/string_builder.md) · [HTML escaping](text/html.md) |
| [time](time.md) | [DateTime and Duration](time/datetime.md) · [timers](time/timers.md) |

The prelude's globals have pages of their own too: [Symbol](prelude/symbol.md) and [typed
arrays, `ArrayBuffer` and `DataView`](prelude/typed_array.md). Servers are a package:
[http-server](../../packages/http-server/README.md) (`serve`, `serveHttp`, WebSocket servers).

The database drivers, [sqlite](sqlite.md) (with [sqlite/engine](sqlite_engine.md), the SQLite
engine in Velt, read-only for now), [postgres](postgres.md) and [redis](redis.md), are part of
the standard library today. They are moving to separately versioned packages; their APIs will
stay the same.

## Runtime-backed and pure modules

Every module is Velt source. Some bind runtime functions (`velt_rt_*`, implemented in Rust) for
what Velt can't do on its own:

| Module | Runtime use |
|---|---|
| `velt/fs`, `velt/net` | the file system, TCP and TLS streams (TLS through rustls) |
| `velt/net/http` | TCP and TLS connections, which the runtime pools between requests; HTTP/1.1, HTTP/2 (`velt/net/http/h2`) and body decoding (`velt/compress`) are Velt |
| `velt/net/websocket` | none of its own: the protocol is Velt on `velt/net` streams (the runtime only masks bytes, checks UTF-8 and draws masking keys) |
| `velt/process`, `velt/os` | arguments, environment, working directory, standard input, platform facts |
| `velt/process/child` | process spawning and pipes |
| `velt/net/udp`, `velt/net/dns` | sockets and the system resolver |
| `velt/sync`, `velt/task` | channel queues, abort signals and task groups |
| `velt/text` | Rust's `regex` engine; `escapeHtml` and `StringBuilder` are runtime passes over bytes |
| `velt/math` | arbitrary-precision integers (dashu-int) |
| `velt/crypto`, `velt/random/uuid` | only the operating system's secure random generator; hashing, HMAC and formatting are Velt (on the CPU's SHA instructions through `velt/simd` where it has them) |
| `velt/random` | a per-thread wyrand generator |
| `velt/time` | only the local UTC offset; calendar math, parsing and formatting are Velt |
| `velt/hash` | FNV-1a over a string's or array's bytes, without a copy |
| `velt/sqlite` | embedded SQLite (rusqlite); transactions and row decoding are Velt |
| `velt/postgres` | tokio-postgres connections, pool, statement cache, TLS and `COPY`; transactions and row decoding are Velt |
| `velt/redis` | a RESP2 client over tokio and rustls: multiplexed connections, pipelines, pub/sub |

Pure Velt: `velt/path`, `velt/mem`, `velt/collections`, `velt/encoding` (with
`velt/encoding/csv` and `velt/encoding/json`), `velt/cli`, `velt/jsx`, `velt/generate`,
`velt/net/http/h2`, `velt/net/url` and the timers of `velt/time` (built on `sleep`, `spawn` and
`velt/task` signals); some use only the runtime's UTF-8 conversions and error-code names. The
runtime ABI is documented in [the internals](../internals/contracts/rt_abi_async.md).

## WebAssembly

On the WebAssembly targets the language and the pure modules work as on native targets, and
`velt/fs` uses the WASI file system. TCP, the HTTP server, child processes and the database
drivers are not available; `fetch` links but rejects with `IoError` `ENOTSUP` (no network)
([WebAssembly](../tooling/webassembly.md)).
