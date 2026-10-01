# The Velt standard library

The standard library is written in Velt and ships with the toolchain. Import a module with the
`velt:` prefix, like Node's `node:` modules:

```ts ignore
import { readFile, writeFile } from "velt:fs";
import { Set } from "velt:collections/set";
```

The [prelude](prelude.md) (strings, arrays, `Map`, `Math`, `JSON`, `Error`, promises, …) is in
scope everywhere without an import. `velt doc --std` generates HTML API documentation from the
sources.

## Conventions

- **Errors** are thrown classes extending the prelude `Error { message }`. Catch them with
  `try { … } catch (e) { e.message }`; `e` is the union of the error classes the calls can
  throw, narrowed with `instanceof`. Each module has its own error class (`UrlError`,
  `CsvError`, …).
- **I/O errors** are `IoError { code, message }` from [`velt:io`](io.md). `code` is a
  Node-style name: `"ENOENT"`, `"EACCES"`, `"ECONNREFUSED"`, `"EOF"`, …
- **Strings** are UTF-8, and string positions (`slice`, `indexOf`, regex match offsets) are
  **byte offsets**.
- **Async functions return promises that start at once**, like JS (a direct `await` costs
  nothing). A promise that is neither awaited nor spawned is a compile error. `*Sync` variants
  block the calling thread.
- **Handles**: some modules return handle structs (`TcpStream`, `FileReader`, `ChildProcess`,
  `Database`, …) that you can pass around and capture freely and release exactly once with
  `close()`. **Planned**
  ([semantics stage 2 §7](../internals/design/semantics-stage2.md#7-identity-and-the-struct-keyword)):
  they become disposable classes with `using` support when the `struct` keyword is removed.

## Modules

| Area | Modules |
|---|---|
| Files and I/O | [fs](fs.md) · [fs_stream](fs_stream.md) · [io](io.md) · [stdin](stdin.md) · [path](path.md) |
| Network | [http](http.md) · [websocket](websocket.md) · [net](net.md) · [udp](udp.md) · [dns](dns.md) |
| Data formats | [json](json.md) · [csv](csv.md) · [encoding](encoding.md) · [url](url.md) · [html](html.md) · [jsx](jsx.md) (TSX rendering) |
| Collections | [collections/set](collections/set.md) · [collections/deque](collections/deque.md) · [collections/priority_queue](collections/priority_queue.md) · [collections/sorted_map](collections/sorted_map.md) · [arena](arena.md) |
| Numbers and time | [math](math.md) · [bigint](bigint.md) · [random](random.md) · [datetime](datetime.md) · [timers](timers.md) |
| Concurrency | [channel](channel.md) |
| Security | [crypto](crypto.md) · [uuid](uuid.md) |
| Text | [regex](regex.md) |
| Programs and the system | [process](process.md) · [cli](cli.md) · [child_process](child_process.md) · [os](os.md) |
| Databases (moving to packages) | [sqlite](sqlite.md) · [postgres](postgres.md) · [redis](redis.md) |

The database drivers are part of the standard library today. They are moving to separately
versioned packages; their APIs will stay the same.

## Runtime-backed and pure modules

Every module is Velt source. Some bind runtime functions (`velt_rt_*`, implemented in Rust) for
what Velt can't do on its own:

| Module | Runtime use |
|---|---|
| `velt:fs`, `velt:fs_stream`, `velt:net`, `velt:http` | file system, TCP, and an HTTP server and client on hyper; HTTPS and HTTP/2 through rustls |
| `velt:websocket` | tokio-tungstenite connections and server upgrades |
| `velt:process`, `velt:stdin`, `velt:os` | arguments, environment, working directory, standard input, platform facts |
| `velt:regex` | Rust's `regex` engine |
| `velt:child_process` | process spawning and pipes |
| `velt:udp`, `velt:dns` | sockets and the system resolver |
| `velt:bigint` | arbitrary-precision integers (num-bigint) |
| `velt:crypto`, `velt:uuid` | only the operating system's secure random generator; hashing, HMAC and formatting are pure Velt |
| `velt:random` | a per-thread wyrand generator |
| `velt:datetime` | only the local UTC offset; calendar math, parsing and formatting are pure Velt |
| `velt:html` | `escapeHtml` is one runtime pass |
| `velt:sqlite` | embedded SQLite (rusqlite); transactions and row decoding are Velt |
| `velt:postgres` | tokio-postgres connections, pool, statement cache, TLS and `COPY`; transactions and row decoding are Velt |
| `velt:redis` | a RESP2 client over tokio and rustls: multiplexed connections, pipelines, pub/sub |

Pure Velt: `velt:path`, `velt:math`, `velt:collections/*`, `velt:arena`, `velt:encoding`,
`velt:url`, `velt:csv`, `velt:cli`, `velt:timers` (built on `sleep` and `spawn`), `velt:json`
and `velt:io`; `velt:jsx` too (escaping through `velt:html`). The runtime ABI is documented in
[the internals](../internals/contracts/rt_abi_async.md).

## WebAssembly

On the WebAssembly targets the language and the pure modules work as on native targets, and
`velt:fs` uses the WASI file system. TCP, HTTP, child processes and the database drivers are not
available ([WebAssembly](../tooling/webassembly.md)).
