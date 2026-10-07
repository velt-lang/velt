# Velt internals

How the compiler and runtime are built. Read this before changing them; for contribution rules
and the test tiers, see [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Pipeline

```
.vlt ─▶ velt_syntax (lexer, parser) ─▶ AST
      ─▶ velt_sema (resolve, types, ownership and mutation inference, typed errors) ─▶ HIR
      ─▶ velt_vir (monomorphize, layouts, drops, async state machines) ─▶ VIR
      ─▶ velt_opt (inline, constant folding, SROA, DCE, numrep, …)  [all of it in release builds]
      ─▶ velt_codegen_cl (Cranelift: debug builds, JIT) | velt_codegen_llvm (LLVM IR → clang -O3)
      ─▶ object file ─▶ velt_link (system linker) + velt_rt (runtime static library)
```

1. **Load**: the driver (`veltc`) reads the root file, follows imports (relative, `velt:` std,
   `paths` aliases, package dependencies through `vpm`), and loads the prelude.
2. **Parse** (`velt_syntax`): a hand-written lexer and Pratt parser with error recovery; one
   `ast::Module` per file.
3. **Check** (`velt_sema`): name resolution, type checking with literal and union types,
   narrowing, generics, ownership and mutation inference over the whole program, exclusive-access
   checks, thread-safety checks, typed `throws` inference, and JSON code generation checks. The
   output is a typed HIR. The same passes answer editor queries (`velt_sema::ide`).
4. **Lower** (`velt_vir`): monomorphization, data layouts, vtables, drop and clone glue, async
   functions as state machines, errors as result returns, with source locations for debug info
   and panic messages. VIR is a typed, MIR-like control-flow graph with a verifier.
5. **Optimize** (`velt_opt`, release builds): inlining, constant folding, copy propagation,
   scalar replacement of aggregates, closure specialization, dead-code elimination, CFG
   simplification, Map probe reuse (a `get` and `set` of the same key probe once), and `numrep`,
   which stores a `number` (`f64`) as an `i32` or `i64` where its facts (interval, whole, never
   NaN, `-0` unobservable) prove the integer computes the same values
   ([design #525](https://github.com/velt-lang/velt/issues/525)). Debug builds run only the cheap
   part: CFG simplification, the int32 helpers inlined, and `numrep`.
6. **Generate code**: Cranelift for debug builds and the `velt dev` JIT; textual LLVM IR compiled
   by clang `-O3` for release builds and WebAssembly.
7. **Link** (`velt_link`): the system linker (MSVC `link.exe`, or `cc`) with the runtime library.

The **runtime** (`velt_rt`) is a Rust static library: memory allocation (mimalloc), strings,
formatting, JSON, a tokio-based multi-threaded executor for async code, and the I/O behind the
standard library (file system, TCP, hyper HTTP, TLS through rustls, WebSockets, processes,
SQLite, PostgreSQL, Redis). `velt_rt_wasm` is its single-threaded WebAssembly counterpart.

## Crates

| Crate | Role |
|---|---|
| `velt_common` | spans, source maps, diagnostics |
| `velt_syntax` | lexer, parser, AST |
| `velt_sema` | semantic analysis, HIR, the editor query API |
| `velt_vir` | HIR → VIR lowering, VIR verifier |
| `velt_opt` | VIR optimizer and interpreter |
| `velt_codegen_cl` | Cranelift backend (objects and JIT, hot swap) |
| `velt_codegen_llvm` | LLVM backend (textual IR, clang) |
| `velt_link` | system linker driver, runtime discovery |
| `velt_rt` | native runtime |
| `velt_rt_wasm`, `velt_rt_host` | WebAssembly runtime; host-side mirror for tests |
| `velt_fmt` | formatter |
| `velt_lsp` | language server |
| `velt_doc` | API docs and the docs website |
| `velt_tscompat` | `velt check --ts-compat`: the lint for code shared with TypeScript |
| `vpm`, `velt_registry`, `velt_http` | package manager, registry server, HTTP client for registries |
| `veltc` | the `velt` CLI: driver, dev supervisor and host, test runner, playground |

## Contracts

The interfaces between stages are documented and treated as contracts: changing one is a
deliberate, reviewed change.

| Contract | Document |
|---|---|
| Overview, ownership, source locations, parameter attributes | [contracts/README.md](contracts/README.md) |
| How sema encodes language features in HIR | [contracts/hir_encodings.md](contracts/hir_encodings.md) |
| Runtime C ABI (sync core) | [contracts/rt_abi.md](contracts/rt_abi.md) |
| Runtime C ABI (async, I/O, dev mode) | [contracts/rt_abi_async.md](contracts/rt_abi_async.md) |
| The CLI as tests rely on it | [contracts/cli.md](contracts/cli.md) |
| The package manifest | [contracts/manifest.md](contracts/manifest.md) |
| The sema query API for editors | [contracts/sema_ide.md](contracts/sema_ide.md) |

## Design notes

Decisions and their rationale, including what is still planned:

- [JavaScript semantics without a garbage collector](design/semantics.md): strings as values,
  hybrid promises, the staged move to shared references, cycles, the JS fidelity decisions.
- [TypeScript alignment](design/ts-alignment.md): inferred mutation, discriminated unions, typed
  errors, `extend`.
- [TypeScript compatibility, round 1](design/ts-compat.md): JS numbers from the standard
  library, scripts, callback indexes, arrow defaults, rest parameters, `x!`, `??=`, destructuring
  defaults, `Date`.
- [Hot reload](design/hot-reload.md): `velt dev`'s supervisor, JIT host and hot swap.
- [TSX for server-side rendering](design/tsx.md): the JSX support, and the common subset of
  TypeScript and Velt that `velt check --ts-compat` lints.
- [`Record<K, V>`](design/record.md): TypeScript object syntax over an insertion-ordered
  dictionary (implemented).
- [`unknown` for dynamic JSON](design/unknown.md): narrowing JSON values the TypeScript way
  (proposed; open questions).
- [JavaScript string semantics](design/strings.md): UTF-16 code-unit lengths and positions over
  WTF-8 storage with a cached UTF-16 view (decided, issue #377).
- [Native code in packages](design/native-packages.md): a `native/` Rust crate in a package, prebuilt
  per-target binaries and the runtime function table.
- [Data models shared with TypeScript](design/shared-models.md): field-only interfaces as object
  types, `readonly` fields in object types, `Partial`/`Required`/`Readonly`/`Pick`/`Omit`
  (implemented, issue #326).
- [A package manifest written in Velt](design/package-manifest.md): `package.vlt`, data-only and
  typed, replacing `velt.toml` (implemented, issue #128).
- [Iteration, generators and `for await`](design/iteration.md): the iterator protocol with typed
  errors, `for...of` over iterables, generators, async generators and `for await`
  (implemented, issue #62).

## Testing

- Unit tests next to the code; cross-module tests in each crate's `tests/`.
- **End-to-end tests** (`tests/golden/**`): every `.vlt` file with a `.out` (and optionally a
  `.code` exit code) is compiled and run in debug and release modes, and its output compared.
  Directories marked `.pending` (such as `tests/golden/bugs/`, known bugs) are reported but
  don't fail the run.
- **Documentation tests**: every `ts` code block in the user documentation is compiled
  (`crates/veltc/tests/docs.rs`).
- **Reload tests** (`tests/reload/`), **differential tests** against Node (`tests/difftest/`),
  and **fuzzing** (`fuzz/`).
- **Benchmarks** in `bench/`, with results in [bench/RESULTS.md](../../bench/RESULTS.md) and
  [bench/web/RESULTS.md](../../bench/web/RESULTS.md).
