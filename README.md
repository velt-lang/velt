# Velt

**Familiar as TypeScript. Fast as Rust.**

Velt compiles typed, modern TypeScript (classes, interfaces, generics, `async`/`await`) ahead of
time to one native executable. No interpreter, no garbage collector, every core in use.

**[veltlang.com](https://veltlang.com)** · [Getting started](https://veltlang.com/docs/book/getting-started/) ·
[Tour](https://veltlang.com/docs/book/tour/) · [Docs](https://veltlang.com/docs/) ·
[Benchmarks](https://veltlang.com/benchmarks/) · [VS Code extension](https://marketplace.visualstudio.com/items?itemName=andtii.velt)

```ts
// src/main.velt of a package made with `velt new hello --template api`: `velt run`
import { serve } from "http-server";

interface User {
  id: string;
  name: string;
}

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
```

That is a complete HTTP server, and every core answers requests. It serves as many JSON requests
a second as Rust's axum and over ten times as many as Node, in a fraction of Node's memory
([benchmarks](https://veltlang.com/benchmarks/), with their method and raw data in [bench/](bench)).

## Why Velt

- **It is TypeScript.** Classes, interfaces, generics, unions, `async`/`await`, the JavaScript
  APIs you know. What is left out is left out on purpose: `eval`, prototypes, `with`, `delete`,
  `1 == "1"`.
- **Native speed.** Generics are specialized, and `number` becomes a machine integer wherever
  that gives the same result. Integer types from `i8` to `u64` are there when you want them.
- **Every core.** A multi-threaded scheduler runs your tasks on all cores, and data races are
  compile errors.
- **Memory you don't think about.** Values are freed when their last reference goes: no
  collector, no pauses.
- **Fixes JavaScript's traps.** Typed errors, and a forgotten `await` is a compile error.
- **One file to ship.** `velt build --release` writes a self-contained executable, and
  cross-compiles from any OS. Also to WebAssembly and Cloudflare Workers.
- **A fast edit loop.** `velt dev` hot-swaps the functions you change on every save.
- **C when you need it.** `declare c` calls C libraries, checked against their headers.

## Get started

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh   # Linux, macOS
irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex       # Windows (PowerShell)
```

The toolchain brings its own linker: no Visual Studio, Xcode or C compiler needed.

```sh
velt new hello              # or: --template api | cli | websocket | lib
cd hello
velt run                    # build and run src/main.velt
velt dev                    # run it, and hot-swap your changes on every save
velt test                   # run the tests
velt build --release        # an optimized native executable
```

Then read [Getting started](https://veltlang.com/docs/book/getting-started/) and
[the tour](https://veltlang.com/docs/book/tour/).

## Documentation

Everything lives on **[veltlang.com/docs](https://veltlang.com/docs/)**:
[the Book](https://veltlang.com/docs/book/) (guides),
[the Reference](https://veltlang.com/docs/reference/) (the language rules),
[the standard library](https://veltlang.com/docs/std/),
[packages](https://veltlang.com/docs/packages/) (the HTTP server, SQLite, PostgreSQL, Redis,
Cloudflare) and [tooling](https://veltlang.com/docs/tooling/).

This repository holds the compiler, the runtime, the standard library ([std/](std)) and the
first-party packages ([packages/](packages)). How the compiler works and why the language is
designed the way it is: [docs/internals](docs/internals/README.md).

## Status

Velt is **pre-1.0**; expect breaking changes between releases. It runs on Linux (x86_64,
arm64), macOS (arm64, x86_64) and Windows (x86_64). Releases and their changes are on the
[releases page](https://github.com/velt-lang/velt/releases); what comes next is in
[the roadmap](ROADMAP.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security issues as described in
[SECURITY.md](SECURITY.md). This project follows a [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
