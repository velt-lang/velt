# Velt standard library

Written in Velt. The compiler resolves `import { x } from "velt:<path>"` to `std/<path>.vlt`.

## Layout
| Path | Role |
|---|---|
| `prelude/*.vlt` | Implicitly imported into every module. |
| `prelude/array.vlt` | `T[]` methods: `forEach map filter reduce find findIndex some every indexOf lastIndexOf includes slice concat reverse isEmpty`, `join` on `string[]`. Callback methods rethrow their callback's errors (generic `E`). |
| `prelude/sort.vlt` | `sort()` on `i64 i32 u64 usize f64 string` arrays (pdqsort), stable `sort(cmp)` on any array. |
| `prelude/map.vlt` | `Map<K, V>`: insertion-ordered hash map (dense entries + linear-probing index). |
| `prelude/math.vlt` | `Math` static methods (f64). |
| `prelude/nullable.vlt` | `isNull unwrap unwrapOr map` on `T \| null`. |
| `prelude/assert.vlt` | `assert`, `assertEq`. |
| `prelude/error.vlt` | `Error { message }`, base class of std errors. |
| `prelude/string.vlt` | `string` methods (`slice indexOf split trim replace padStart …`), `String.fromCharCode`, `parseInt`, `parseFloat`, `Number`. |
| `prelude/json.vlt` | `JSON.stringify/parse/parseValue`, `JsonError`, `JsonValue`. |
| `prelude/sync.vlt` | `Mutex<T>` layout for `new Mutex(x)` / `.with(f)` (compiler-implemented). |
| `math.vlt` | `std/math`: integer helpers `clamp gcd lcm isPrime fib`. |
| `io.vlt` | `std/io`: `IoError`; std-internal `IoResult`/`IoStatus` plumbing over rt results. |
| `fs.vlt` | `std/fs`: async + `*Sync` file system API. |
| `fs_stream.vlt` | `std/fs_stream`: chunked/line `FileReader` (`openRead`) and buffered `FileWriter` (`openWrite`). |
| `net.vlt` | `std/net`: `listen`/`connect`, `TcpListener`, `TcpStream` (`net_bytes.vlt`: internal). |
| `http.vlt` | `std/http`: `serve` (HTTP/1.1, HTTP/2, HTTPS), `fetch` (http/https), `Request`, `Response`, `Server`, `FetchResponse`. |
| `websocket.vlt` | `std/websocket`: server upgrades (`upgradeWebSocket`) and clients (`connectWebSocket`), `WebSocket`. |
| `json.vlt` | `std/json`: `Value` (= prelude `JsonValue`). |
| `process.vlt` | `std/process`: `argv args env setEnv removeEnv cwd chdir exit`. |
| `path.vlt` | `std/path`: POSIX `join joinAll dirname basename extname normalize isAbsolute sep`. |
| `collections/set.vlt` | `std/collections/set`: `Set<T>`, insertion-ordered hash set with ES2025 set algebra. |
| `collections/deque.vlt` | `std/collections/deque`: `Deque<T>`, ring-buffer double-ended queue. |
| `collections/priority_queue.vlt` | `std/collections/priority_queue`: `PriorityQueue<T>`, binary heap ordered by a comparator. |
| `collections/sorted_map.vlt` | `std/collections/sorted_map`: `SortedMap<K, V>`, key-ordered map (sorted arrays) with `floorKey ceilingKey range`. |
| `encoding.vlt` | `std/encoding`: Base64 (std + URL-safe), hex, UTF-8 encode/decode/validate; `EncodingError`. |
| `crypto.vlt` | `std/crypto`: `sha256 sha1 hmacSha256 hmacSha1 randomBytes randomInt timingSafeEqual`. |
| `random.vlt` | `std/random`: `random randomInt` (fast, not for secrets). |
| `uuid.vlt` | `std/uuid`: `uuidv4 uuidv7 uuidParse uuidStringify uuidValidate uuidVersion NIL_UUID`. |
| `regex.vlt` | `std/regex`: `RegExp`, `RegExpMatch` over the runtime's linear-time regex engine. |
| `url.vlt` | `std/url`: WHATWG `URL`, `URLSearchParams`, `encodeURIComponent` family (`url/*.vlt`: internal). |
| `datetime.vlt` | `std/datetime`: UTC-first `DateTime` (ISO/HTTP dates, formatting, calendar math), `Duration` (`datetime/*.vlt`: internal). |
| `html.vlt` | `std/html`: `escapeHtml` (one runtime pass). |
| `csv.vlt` | `std/csv`: RFC 4180 `parseCsv parseCsvRecords stringifyCsv`, `CsvError`. |
| `cli.vlt` | `std/cli`: `ArgParser` (flags, options, positionals, help text), `ParsedArgs`, `CliError`. |
| `child_process.vlt` | `std/child_process`: `exec execSync execShell spawn`, `ChildProcess`, `ExecResult`. |
| `stdin.vlt` | `std/stdin`: `readLine readAll` (+ `*Sync`). |
| `os.vlt` | `std/os`: `platform arch availableParallelism tmpdir hostname eol`. |
| `udp.vlt` | `std/udp`: `bindUdp`, `UdpSocket`, `Datagram`. |
| `dns.vlt` | `std/dns`: `lookup lookupOne`. |
| `sqlite.vlt` | `std/sqlite`: embedded SQLite (`open`, `Database`, `Statement`, `Transaction`, `SqliteError`), synchronous, better-sqlite3-like. |
| `postgres.vlt` | `std/postgres`: PostgreSQL client (`connect`, `createPool`, `Client`, `Pool`, `Transaction`, `PgError`), async, node-postgres-like. |
| `redis.vlt` | `std/redis`: Redis client (`connect`, `RedisClient`, pipelines, `subscribe`) (`redis/*.vlt`: internal). |
| `timers.vlt` | `std/timers`: `setTimeout setImmediate clearTimeout delay`, `Timer`, `Ticker`. |

## Runtime bindings
std binds `velt_rt_*` symbols (docs/internals/contracts/rt_abi_async.md) with `declare function` /
`declare async function`. Lowering passes `string`/struct/array params as `const T*` and returns
struct results through a trailing out-pointer; a C out-parameter next to a scalar result is an
ordinary param that std always passes as a fresh local (`let out = "";` / a zeroed value, which
the runtime overwrites): sema cannot see foreign writes, so an out-parameter must never be a
param, field or element of the calling function. Opaque runtime
handles are `u64`. Handle-owning classes release them in their `[Symbol.dispose]()` drop hook (http
`Server`/`Response`/`FetchResponse`, `JsonValue`); the Copy handle structs of std/net
(`TcpListener`, `TcpStream`) are released by an explicit `close()` (see net.vlt).

## Rules for std code
- Only files under `std/` may call compiler intrinsics, spelled `__intrinsic_<snake_case>` after the
  `Intrinsic` enum in `crates/velt_sema/src/hir.rs` (`ArrayWithCapacity` → `__intrinsic_array_with_capacity`).
- A function taking a callback rethrows what the callback throws by being generic over its error
  type: `map<U, E>(f: (x: T) => U throws E): U[] throws E`.
- Every file starts with a `//` comment describing its role; keep files under 400 lines.
- Only `export` the public surface; helpers stay module-private. `extend` blocks apply wherever
  their module is loaded (they cannot be exported).
- Element-returning methods clone (`find`, `filter`, `slice`, `Map.get`, `Map.keys()`…). For Copy
  types a clone is a plain copy. Iteration and callbacks borrow.

## Performance notes
Std code is monomorphized and inlined like user code, so write plain index loops.
- Reserve capacity (`__intrinsic_array_with_capacity`) when the final length is known.
- Never clone to read: pass `xs[i]` straight to callbacks and comparisons (they borrow).
- Rearrange arrays with `__intrinsic_array_swap` / `__intrinsic_array_truncate`; moving an
  element out of an index is not allowed, and a swap avoids clones.
- Keep algorithms allocation-free where possible (both sorts are in place).
