# WebAssembly

Velt programs compile to WebAssembly for two hosts:

| Target | Runs in | Output |
|---|---|---|
| `wasm32-wasip1` (alias `wasm32-wasi`) | wasmtime, wasmer, Node's WASI, … | a WASI command module `target/velt/<stem>.wasm` |
| `wasm32-unknown-unknown` | the browser (and Node) | `target/velt/<stem>.wasm` plus the JavaScript glue `velt_web.mjs` |

```sh
velt run --target wasm32-wasip1 hello.vlt             # builds, then `wasmtime run --dir=. …`
velt build --target wasm32-unknown-unknown hello.vlt  # hello.wasm + velt_web.mjs
velt run --target wasm32-unknown-unknown hello.vlt    # runs it with node and the glue
```

## Setup

WebAssembly builds go through LLVM's `opt` and `llc` (Apple's clang has no WebAssembly backend,
so clang is not used) and a WebAssembly linker. With rustup, everything comes from the Rust
toolchain:

```sh
rustup component add llvm-tools                            # opt, llc
rustup target add wasm32-wasip1 wasm32-unknown-unknown     # wasi-libc; rust-lld links wasm
cargo build -p velt_rt_wasm --target wasm32-wasip1          # the runtime, per target
cargo build -p velt_rt_wasm --target wasm32-unknown-unknown
brew install wasmtime                                      # or https://wasmtime.dev
```

The linker is the Rust toolchain's `rust-lld`, which matches the wasi-libc rustup installs (an
older LLVM `wasm-ld` can fail on it with undefined symbols such as `__wasm_first_page_end`);
without Rust, `wasm-ld` on `PATH`. `velt doctor` shows which one is used.

Overrides: `VELT_LLVM_BIN` (a directory with `opt` and `llc`), `VELT_LINKER` (`wasm-ld`),
`VELT_WASI_SYSROOT` (a directory with wasi-libc's `crt1-command.o` and `libc.a`), `VELT_RT_LIB`
(the runtime library), `VELT_WASM_RUNNER` (runs WASI modules for `velt run`).

## What works

The language and the standard library as on native targets, single-threaded:

- `async`/`await`, `spawn`, `Promise.all`, `sleep` and `yieldNow` run on a current-thread
  executor (tasks interleave at `await` points; `shared` and `Mutex` work, with one thread);
- `velt:fs` uses the WASI file system (`velt run` grants the current directory);
  `velt:process` arguments and environment, `performance.now()` and `Date.now()` work on both
  targets;
- panics print `panic: …` and exit with 101, like native programs.

**Not available**: TCP, HTTP, child processes and the database drivers; the link fails with a
note naming the missing runtime function. Browser modules have no file system.

## In the browser

```js
import { runVelt } from "./velt_web.mjs";
const code = await runVelt(fetch("hello.wasm"), {
  write: (stream, bytes) => console.log(new TextDecoder().decode(bytes)),
});
```

`runVelt` instantiates the module, runs `main` and resolves to the exit code; output arrives as
bytes per stream (1 = stdout, 2 = stderr). Run long programs in a Web Worker: `sleep`
busy-waits, because a browser's main thread cannot block.

## The playground

```sh
velt playground            # http://127.0.0.1:8090/  (--port, --host)
```

An editor with examples, a Run button and an output pane. The compiler is native (it uses LLVM
and a linker), so the playground compiles on the server (the `velt playground` process) and the
page runs the resulting module in a Web Worker in your browser. Only `velt:` imports are
accepted. Anyone who can reach the port can compile programs (which run in their own browser,
not on the server); keep the default `127.0.0.1` unless you mean to share it. The playground
needs the browser runtime: `cargo build -p velt_rt_wasm --target wasm32-unknown-unknown`.
