# Velt

**Velt: a modern TypeScript without the funky stuff inherited from JavaScript — compiled to
native code with Rust-level performance.**

```ts
import { serve, Request, Response } from "velt:http";

class NotFound extends Error {}

function findUser(id: string): string throws NotFound {
  if (id != "42") throw new NotFound(`no user ${id}`);
  return "ada";
}

async function main() {
  const hits = shared(0);                          // a counter shared by all request handlers
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    hits.add(1);
    try {
      return Response.json({ user: findUser(req.query), hits: hits.get() });
    } catch (e) {                                  // e: NotFound, never `unknown`
      return Response.json({ error: e.message }, 404);
    }
  });
}
```

That is a complete, multi-threaded HTTP server. It compiles to a native executable with no
garbage collector and no runtime to install.

## Why Velt

- **It's TypeScript.** Classes, interfaces, generics, unions and narrowing, discriminated unions,
  closures, `async`/`await`, ES modules, template literals, destructuring, `using`. If you write
  TypeScript, you can read Velt today.
- **It's as fast as Rust.** Native code through LLVM, monomorphized generics, no boxing, no GC.
  On an Intel i9 laptop (best of 10, [bench/RESULTS.md](bench/RESULTS.md)): recursive `fib(35)`
  takes 32 ms in Velt and in Rust `-O`, 147 ms in Node; n-body 162 ms (Rust 179, Node 1826);
  a hash-map workload 182 ms (Rust 287 with std's `HashMap`, Node 479).
- **It serves like Rust.** On the TechEmpower-style suite ([bench/web/RESULTS.md](bench/web/RESULTS.md),
  Linux arm64, 10 cores shared with the load generator and PostgreSQL), Velt answers 741k JSON
  requests/s (Rust axum 619k, Go 327k, Node 100k) and is within 0.94–1.07× of Rust on every
  database test, at 25–54 MB of memory (Node: 173–278 MB).
- **No garbage collector, no pauses.** Objects are references, as in JavaScript, and memory is
  freed deterministically when the last reference goes; cleanup (`[Symbol.dispose]`, `using`)
  runs at a known point. The compiler infers ownership and mutation: no lifetimes, no borrow
  syntax, no `mut`, and no reference count for values with a single owner.
- **Errors are typed.** `catch (e)` knows exactly what the `try` block can throw. Errors compile
  to plain return values: no unwinding, no exception tables.
- **Async on every core.** Promises start eagerly and behave like JavaScript's, a directly
  awaited call costs nothing, `spawn` runs work on other cores, and data races are compile
  errors.
- **It drops JavaScript's bug sources**: no `undefined` (only `null`), no implicit
  `"5" + 1`, no truthy `0` and `""`, no loose equality, no mutable globals, and a forgotten
  `await` is a compile error. Every such error tells you what to write instead
  ([Velt for TypeScript developers](docs/book/ts-developers.md)).
- **A fast edit loop.** `velt dev` hot-swaps changed functions into the running program, keeping
  its state and open connections.

## Install

Velt is built from source for now. You need Rust (stable) and a system linker (the Visual Studio
Build Tools on Windows, `build-essential` on Linux, the Xcode command line tools on macOS);
LLVM/clang 16 or newer is optional, for optimized release builds.

```sh
git clone https://github.com/velt-lang/velt
cd velt
cargo build --release -p veltc -p velt_rt
./target/release/velt doctor      # checks the toolchain, builds and runs a hello world
```

Then put `target/release` on your `PATH`, or build an installable toolchain directory with
`scripts/package.sh` / `scripts/package.ps1` ([Platforms and installation](docs/tooling/platforms.md)).

## Quick start

```sh
velt new hello              # also: --template api | cli | websocket | lib
cd hello
velt run                    # build and run src/main.vlt
velt dev                    # run, then hot-swap your changes on every save
velt test                   # run tests/*.test.vlt
velt build --release        # an optimized native binary in target/velt/
```

Install the VS Code extension from [`editors/vscode`](editors/vscode) for diagnostics,
completion, go-to-definition and quick fixes; any LSP client works with `velt lsp`.

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

What is coming next, from [the roadmap](ROADMAP.md): JavaScript's shared-reference semantics for
objects (no more moves), the rest of TSX (`children` props, faster templates), `JSON.parse` into unions, the database
drivers as packages, and more platform and tooling work. Planned features are marked
**Planned** in the docs and never shown as working.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security issues as described in
[SECURITY.md](SECURITY.md). This project follows a [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
