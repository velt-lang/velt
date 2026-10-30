# Velt

**Your TypeScript, compiled to a native executable. Rust-class speed, every core, no runtime.**

[Install](#get-started) · [VS Code extension](https://marketplace.visualstudio.com/items?itemName=andtii.velt)
(with F5 debugging) · [Getting started](docs/book/getting-started.md) · [Tour](docs/book/tour.md) ·
[Docs](docs/README.md)

Velt compiles TypeScript ahead of time to machine code through LLVM. It is not a JavaScript
engine and doesn't wrap one: there is no interpreter, no JIT warm-up, no garbage collector and no
single-threaded event loop. Your program is a native binary that uses every core of the machine.

```ts
// server.ts: run it with `velt run server.ts`
import { serve } from "velt/net/http";

interface User {
  id: string;
  name: string;
}

async function main() {
  const users = new Map<string, User>();
  users.set("42", { id: "42", name: "Ada" });

  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const id = new URL(req.url).searchParams.get("id") ?? "";
    const user = users.get(id);
    if (user === undefined) {
      return Response.json({ error: `no user ${id}` }, { status: 404 });
    }
    return Response.json(user);
  });
}
```

That is a complete HTTP server, and every core answers requests. On the TechEmpower-style suite
below, this kind of server handles **7× as many JSON requests as Node and 1.2× as many as Rust's
axum**, in tens of megabytes of memory.

**Small and fast to start.** A minimal "Hello, World!" web server, built with
`velt build --release`:

| | Velt | Node |
|---|---:|---:|
| memory, idle | **8 MB** | 39 MB |
| memory under load, 220k req/s vs Node's 130k | **19–21 MB** | 129 MB |
| start a program and exit | **3 ms** on Linux, 33 ms on Windows | 69 ms on Windows |
| what you deploy | **one 6 MB executable** | Node plus `node_modules` |

Idle memory and the Windows start times were measured with velt 0.2.1 and Node 22.22 on an
Intel i9 laptop running Windows 11. The memory under load is from
[bench/http](bench/http/RESULTS.md), measured on an Apple M4 with Node 24, and the Linux start time is from
[bench/RESULTS.md](bench/RESULTS.md).

## Get started

Install Velt with one command. It works on Linux (x86_64 and arm64), macOS and Windows:

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh   # Linux, macOS
irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex       # Windows (PowerShell)
```

That is all you need. The toolchain brings its own linker, so you don't need Visual Studio,
Xcode or a C compiler, and `velt toolchain self-update` keeps it current. Then create a project and run it:

```sh
velt new hello              # or: --template api | cli | websocket | lib
cd hello
velt run                    # build and run src/main.vlt
velt dev                    # run it, and hot-swap your changes on every save
velt test                   # run the tests
velt build --release        # an optimized native executable in target/velt/
```

You can also run TypeScript you already have: `velt run app.ts`.

In VS Code, install the [Velt extension](https://marketplace.visualstudio.com/items?itemName=andtii.velt)
(`code --install-extension andtii.velt`). It gives you diagnostics as you type, completion, go to
definition, quick fixes and F5 debugging. Other editors can use the language server, `velt lsp`.

Next, read [Getting started](docs/book/getting-started.md) and
[the tour](docs/book/tour.md). Other platforms, options and building from source are covered in
[Platforms and installation](docs/tooling/platforms.md).

## Why Velt

- **Start from the TypeScript you have.** Velt reads `.ts` and `.tsx` files directly. That
  covers classes, interfaces, generics, unions and narrowing, closures, `async`/`await`,
  generators, destructuring, enums, ES modules, template literals and TSX. `velt run app.ts`
  compiles and runs your code. `.ts` and `.vlt` files import each other freely, so you can move a
  project over one file at a time.
- **Then go further than TypeScript can.** Typed errors, real integer types, methods on any type,
  and parallel tasks: the things a compiler makes possible. See
  [Beyond TypeScript](#beyond-typescript).
- **Native speed.** Generics compile to specialized code. `number` becomes a machine integer
  wherever that provably gives the same result. There is no boxing and no GC. Typical
  TypeScript-style code runs **several times faster than Node**, and on tight loops Velt matches
  Rust ([numbers below](#performance)).
- **Every core, not one.** Node runs your code on one thread and needs a cluster of processes to
  use the rest. A Velt program runs on a multi-threaded scheduler from the start: request
  handlers spread over all cores, and `spawn` puts any work on another core. **Data races are
  compile errors**, so parallel code is as safe to write as single-threaded code.
- **Memory you don't think about.** Objects are references, as in JavaScript. Memory is freed
  the moment the last reference to it goes, so there are no GC pauses and `using` cleanup runs at
  a known point. An idle Velt web server takes about 8 MB and stays around 20 MB under load,
  where Node needs well over 100 MB.
- **One file to ship.** `velt build --release` writes a native executable. There is nothing to
  install on the server, no `node_modules`, and startup takes milliseconds. You can cross-compile
  for Linux, macOS, Windows or WebAssembly from any of them.
- **Fixes JavaScript's classic traps.** `1 == "1"` is a compile error, not `true`. Adding a
  string to an object doesn't silently give `"[object Object]"`. A forgotten `await` is caught
  before it can drop an error. Every error message says what to write instead.
- **A fast edit loop.** `velt dev` swaps changed functions into the running program in about
  60 ms, keeping its memory, open connections and running tasks.
- **Editor and debugger included.** The [Velt extension for VS Code](https://marketplace.visualstudio.com/items?itemName=andtii.velt)
  gives you errors as you type, completion, go to definition, rename, inlay hints and quick
  fixes. F5 debugging needs no setup: breakpoints, stepping, and variables shown as Velt values.
  It works in LLDB, GDB and Visual Studio too, because Velt writes standard debug info. Any
  other editor can use the language server, `velt lsp`.
- **Everything else in one tool.** `velt` also runs tests (`velt test`, with **▶ Run test** in
  the editor), formats code (`velt fmt`), manages packages with a lockfile, and cross-compiles.
- **Any Rust crate, a few lines away.** Velt's own ecosystem is growing, and until a library
  exists in Velt, you can wrap the Rust crate that already does it: image processing,
  compression, a database driver. A package includes the crate and exports functions with
  `#[export]`, and its users never need Rust
  ([how it works](#any-rust-crate-a-few-lines-away)).

## Beyond TypeScript

TypeScript compiles to JavaScript, so it can't have these features. Velt can:

```ts
class NotFound extends Error {}

// Errors are part of the type: callers see exactly what can go wrong.
function findUser(id: string): string throws NotFound {
  if (id !== "42") {
    throw new NotFound(`no user ${id}`);
  }
  return "Ada";
}

// Real integer types: u32 arithmetic wraps as it does in C or Rust.
function fnv1a(text: string): u32 {
  let hash: u32 = 2166136261;
  for (let i = 0; i < text.length; i++) {
    hash = (hash ^ (text.charCodeAt(i) as u32)) * 16777619;
  }
  return hash;
}

function sumOfSquares(from: i64, to: i64): i64 {
  let total = 0;
  for (let i = from; i < to; i++) {
    total += i * i % 7;
  }
  return total;
}

async function main() {
  try {
    console.log(findUser("7"));
  } catch (e) {                     // e: NotFound, never `unknown`
    console.log(e.message);
  }
  console.log(fnv1a("velt"));

  // Four tasks on four cores; Promise.all collects the results.
  const parts: Promise<i64>[] = [];
  for (let c = 0; c < 4; c++) {
    parts.push(spawn(async () => sumOfSquares(c * 1000000, (c + 1) * 1000000)));
  }
  console.log((await Promise.all(parts)).reduce((a, b) => a + b, 0));
}
```

- **Typed errors.** `catch (e)` knows exactly what its `try` block can throw. Errors compile to
  plain return values: no unwinding, no exception tables, and no cost when nothing is thrown.
- **Integer and float types.** `i8` to `i64`, `u8` to `u64` and `f32` alongside `number`, with
  literal suffixes such as `255u8` and `1.5f32`.
- **`extend`** adds methods to any type, including `string`, numbers and arrays, at no run-time
  cost.
- **Tasks and shared state.** `spawn`, typed channels, `shared(x)` with atomic counters, `Mutex`,
  and `taskScope` for structured concurrency.
- **Typed JSON.** `JSON.parse<User>(text)` checks the shape and builds the object in one pass.

For the full list, and the few places where Velt deliberately differs from TypeScript, read
[Velt for TypeScript developers](docs/book/ts-developers.md).

## Any Rust crate, a few lines away

Velt's goal is an ecosystem written in Velt itself, because Velt code is what the compiler
optimizes best, and the standard library is moving that way: the Redis client and the CSV parser,
for example, are already Velt code. A young language can't have a library for everything yet,
though, so until one exists in Velt you don't have to wait: **any Velt package can include a Rust crate**,
which puts all of crates.io within reach. Need image processing, compression, PDF generation or
ML inference today? Wrap the crate that already does it, typically in a few dozen lines, and
replace it with a Velt version whenever one arrives; your users' code doesn't change. The SQLite driver in
[`packages/sqlite`](packages/sqlite) is a complete example: 160 lines of Rust around `rusqlite`,
and 110 lines of Velt.

Here is zstd compression as a package, end to end. Add the crate to the package's
`native/Cargo.toml`, next to `velt_native`:

```toml
[dependencies]
velt_native = { git = "https://github.com/velt-lang/velt" }   # until it is on crates.io
zstd = "0.13"
```

Export what you need from Rust. A `blocking` function runs on the runtime's thread pool for
blocking work, and Velt sees it as an `async` function:

```rust
use velt_native::{export, Error};

velt_native::package!(zstd);

#[export(blocking)]
fn velt_zstd__compress(data: Vec<u8>, level: i32) -> Result<Vec<u8>, Error> {
    zstd::encode_all(&data[..], level).map_err(|e| Error::other(format!("{e}")))
}
```

Declare it in the package's Velt code and give it the API you want, with a typed error:

```ts ignore
import { IoResult } from "velt/native";

declare async function velt_zstd__compress(data: u8[], level: i32): Promise<IoResult<u8[]>>;

export class ZstdError extends Error {}

export async function compress(data: u8[], level: i32 = 3): Promise<u8[], ZstdError> {
  const r = await velt_zstd__compress(data, level);
  if (r.code != 0) {
    throw new ZstdError(r.message);
  }
  return r.value;
}
```

Programs then use it like any other package: `velt add zstd`, then
`const packed = await compress(bytes);`.

- **Checked at compile time.** `velt` compares every `declare` with what the library actually
  exports, so a wrong name or type is a compile error, not a crash at run time.
- **Your users never need Rust.** `velt native build` and `velt publish` ship a prebuilt library
  for each platform. `velt add` downloads the right one and verifies its checksum against the
  lockfile.
- **It works everywhere a Velt program does.** `velt run`, `velt build --release` (still one
  self-contained executable) and `velt dev` all work as usual, and your Velt edits keep
  hot-swapping.
- **Resources clean up after themselves.** Native objects, such as a database connection, stay
  on the Rust side behind a handle. Wrap them in a class with `[Symbol.dispose]()`, and `using`
  closes them at a known point.

The full walkthrough, including strings, errors, handles and publishing for several platforms,
is [Packages with native code](docs/book/native-packages.md).

## Performance

Velt is benchmarked against Rust and Node on every change. A pull request that makes a benchmark
more than 3% slower needs an explicit OK. Here is a selection; the full tables, methods and
machines are in [bench/](bench/RESULTS.md).

**Web serving**: a TechEmpower-style suite on Linux arm64 with 10 cores
([bench/web](bench/web/RESULTS.md)).

| test | Velt | Rust (axum) | Go | Bun | Node | Node cluster |
|---|---:|---:|---:|---:|---:|---:|
| JSON, req/s | **741k** | 619k | 327k | 248k | 100k | 474k |
| plaintext, pipelined, req/s | **3.31M** | 1.24M | 621k | 76k | 161k | 679k |
| single query, req/s | **141k** | 133k | 119k | 42k | 39k | 106k |
| fortunes (HTML), req/s | 115k | **118k** | 89k | 33k | 23k | 90k |
| peak memory | 25–54 MB | 29–45 MB | 35–53 MB | 34–104 MB | 173–278 MB | 0.8–1.7 GB |

Velt is within 0.94–1.07× of Rust on every database test and handles 4–13× as many requests as
Node.

**Compute**: best of 10 runs on an Intel i9 laptop ([bench/RESULTS.md](bench/RESULTS.md)).

| benchmark | Velt | Rust `-O` | Node |
|---|---:|---:|---:|
| `fib(35)`, recursive calls | 32 ms | 32 ms | 147 ms |
| n-body, floating point | 162 ms | 179 ms | 1826 ms |
| hash map workload | 182 ms | 287 ms | 479 ms |
| class-heavy code | 201 ms | 293 ms | 521 ms |

**Async**, on the same i9 laptop ([bench/async](bench/async)):

- 10 million awaits take 55 ms. Rust's tokio takes 52 ms and Node 508 ms.
- A million spawned tasks take 614 ms. Tokio takes 703 ms and Node 1618 ms, and Velt uses half
  of Node's memory.

Velt is young, and not everything runs at Rust's speed yet. Code written the way TypeScript
usually is, with closures and lots of small objects and strings, takes up to about 4× as long as
Rust, though it stays well ahead of Node. The remaining gaps are tracked in
[bench/FINDINGS.md](bench/FINDINGS.md).

## Documentation

- [The Book](docs/book/README.md): [getting started](docs/book/getting-started.md),
  [a tour of Velt](docs/book/tour.md), [Velt for TypeScript developers](docs/book/ts-developers.md),
  and guides for HTTP servers, CLIs, async, errors, memory, packages, testing and hot reload.
- [The Reference](docs/reference/README.md): the precise language rules.
- [The standard library](docs/std/README.md): files, HTTP, WebSockets, JSON, regex, dates,
  crypto, collections, databases, and more.
- [Tooling](docs/tooling/README.md): the `velt` command, `velt dev`, packages, formatter,
  editors, debugging, WebAssembly, platforms.
- [Internals](docs/internals/README.md): the compiler pipeline and design notes.

## Status

Velt is **pre-1.0** and under active development; expect breaking changes. What works today, on
Windows (x86_64), Linux (x86_64, arm64) and macOS (arm64, x86_64): the language as documented in the Reference, the
standard library, packages with a lockfile, the formatter, the language server, debug info,
`velt dev` with hot swap, and WebAssembly (WASI and browser, without networking).

What is coming next is in [the roadmap](ROADMAP.md). Planned features are marked **Planned** in the
docs and never shown as working.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security issues as described in
[SECURITY.md](SECURITY.md). This project follows a [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
