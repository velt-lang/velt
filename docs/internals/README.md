# Velt internals

How the compiler and runtime are built. Read this before changing them; for contribution rules
and the test tiers, see [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Pipeline

```
.velt ─▶ velt_syntax (lexer, parser) ─▶ AST
      ─▶ velt_sema (resolve, types, ownership and mutation inference, typed errors) ─▶ HIR
      ─▶ velt_vir (monomorphize, layouts, drops, async state machines) ─▶ VIR
      ─▶ velt_opt (inline, constant folding, SROA, DCE, numrep, …)  [all of it in release builds]
      ─▶ velt_codegen_cl (Cranelift: debug builds, JIT) | velt_codegen_llvm (LLVM IR → clang)
      ─▶ object file ─▶ velt_link (bundled lld or system linker) + velt_rt (runtime static library)
```

1. **Load** (`velt_loader`, called by the driver in `veltc`): reads the root file, follows
   imports (relative, `velt/` std, `paths` aliases, package dependencies through `vpm`), and
   loads the prelude and the globals a program mentions. Tests and tools that must see a program
   as `velt build` does load it through the same crate, without the backends.
2. **Parse** (`velt_syntax`): a hand-written lexer and Pratt parser with error recovery; one
   `ast::Module` per file. std's files are parsed once per compiler: the loader keeps their
   modules in the build cache in a compact form (`velt_syntax::encode_module`,
   `crates/velt_loader/src/std_ast.rs`).
3. **Check** (`velt_sema`): name resolution, type checking with literal and union types, narrowing,
   generics, ownership and mutation inference over the whole program, exclusive-access checks,
   thread-safety checks, typed `throws` inference, and JSON code generation checks. The output is a
   typed HIR. The same passes answer editor queries (`velt_sema::ide`). A program is checked again,
   whole, while the error sets of function types written without `throws` grow, and once more with
   sharing when a held borrow caused an error (the checks after that one share too;
   `check_on_current_thread` in `lib.rs`). `VELT_SEMA_TIMINGS=1` prints each pass's time in each of
   these rounds to stderr. Work that only reads the AST or the HIR runs on several threads
   (`par.rs`): the source rewrites and scans in front of the check (each module on its own) and the
   move check's per-function dataflow (`moves`). Their results are applied in order, so the output
   does not depend on the thread count, and work below `par::MIN_WEIGHT` stays on the checking
   thread; `VELT_SEMA_THREADS=n` caps the threads (`1`: none).
4. **Lower** (`velt_vir`): monomorphization, data layouts, vtables, drop and clone glue (a
   self-referential class drops its chain in a loop, `lower/glue/drop_chain.rs`; other drops that
   can nest are bounded by the runtime, `lower/glue/drop_depth.rs`), async functions as state
   machines, errors as result returns, with source locations for debug info and panic messages. VIR
   is a typed, MIR-like control-flow graph with a verifier. The program is lowered again while a
   pass finds types that must be counted (`lower/boxing/`). Those passes only find them: their
   functions leave out the versions of view loops, which are made once the counted types are
   complete (`lower/deferred_versions.rs`; `VELT_LOWER_WHOLE=1` lowers every pass whole, for
   comparing). `VELT_LOWER_PROFILE=1` prints each pass's time, what it found, and its slowest
   functions (their own time, with their size) to stderr.
5. **Optimize** (`velt_opt`, release builds): inlining, constant folding, copy propagation,
   scalar replacement of aggregates, closure specialization, dead-code elimination, CFG
   simplification, Map probe reuse (a `get` and `set` of the same key probe once), module state
   lookups once per function call, borrowed string copies, and `numrep`.
   The passes and their order are in `crates/velt_opt/src/lib.rs`: `dead_funcs` first, then
   `gated_clones` (a gated function's calls of callees that check its CPU extension go to clones
   where the check is true), then per function `numrep`, `frozen` and, for functions with
   `frozen` parameters, `bounds` (range checks a dominating check already proves are decided at
   compile time); then up to three rounds of `inline` (bottom-up; a small member of
   a cycle that does not call itself is inlined into the loops of callers outside its cycle
   once, its calls back into the cycle staying calls, so a rarely taken recursion such as
   `Cursor.int` ⇄ `Cursor.real` does not keep its entry out of the loops that call it),
   `const_fields`, and per function (on several threads when there are enough functions, like
   every per-function loop of the release pipeline: `velt_opt::parallel`) `vtable_loads`,
   `constfold`, `copyprop`, `addr_forward`, `heap_sroa`, `enum_sroa` (an enum local such as a
   throwing call's `Result`, split into one local per slot its variants use), `sroa`, `dce`,
   `thread_tags` (a jump into the test of an inlined callee's `Result` tag that the jumping
   block just set goes where the tag sends it) and `simplify_cfg`; then `dead_funcs`,
   `fits_pair` (two `fits` tests of one index as one test) and the counted part of
   `borrowed_copies` (below); then
   `module_state` (a function asks the runtime for the module state block once per call, not
   at every access to a module-level `let`), `map_probe`, `borrowed_copies` (a string local that
   only reads a copy of a string, such as `const src = this.src;`, holds the source's words
   without a reference of its own when nothing can change the source while its text is read:
   no `velt_rt_str_clone` / `velt_rt_str_drop` pair; its counted part, run first over the whole
   program with callees first, does the same for
   counted pointers: `const data = this.db.data;` keeps no count of its own, neither increment
   nor release, when no call that may free a counted object runs while `data` is still used and
   the reference never leaves the local), `numrep`, `divisions` and
   `dead_fills` once per function; then `noalias`, `frozen` (scalar fields of a `frozen`
   parameter's pointee read in a loop are loaded once on entry) and
   `frame_slots`, followed by a cleanup when one changed the function; `invariant_reads` (a read
   through object and
   array header pointers inside a loop nest that writes no memory, calls nothing and tests only
   counters (a type test such as `instanceof` decides which class the read sees), such as a
   typed array's buffer and its bytes' length, is made once in front of the outermost such
   loop, each pointer tested for null first: LLVM cannot move such a chain out of an outer loop
   when the inner loop may run no iteration, and then keeps the inner loop's bounds check);
   `range_versions` (an innermost loop whose range tests, `fits`/`fits_both` of any lane type or
   `aAt + i + k` compared with a bound it does not change, one test of the counter's bound,
   the offsets and the lengths in front of it proves true, runs in a copy where they are
   `true`, followed by a cleanup; where that test fails the original loop runs: Velt's integer
   arithmetic wraps, so LLVM proves none of them from another); `cold_calls` (a function's calls of one callee that never returns, a bounds check's report,
   become one call, reached from every site; the panic text keeps each site's location, an
   argument, but the merged call instruction carries the first site's debug location, so a
   debugger or a backtrace shows that line for every merged check); `null_exits` (a call of a
   function that returns at once when a pointer it is passed points at null, as every drop
   glue does, tests that pointer first, so dropping a null field costs no call); `heap_calls`
   (allocations of a constant size call std's allocator hooks, `std/lang/hooks/heap.velt`, when
   lowering included them: release builds for native targets); and
   `dead_funcs` again at the end. Five change how objects and numbers are
   represented or reached:
   - `heap_sroa` keeps a class instance that never escapes its function (after inlining) in
     locals instead of on the heap: no allocation, zero fill or free. Each name of the object
     gets its own copy; a write through one name is copied to the other names that hold the
     same object on every path and are read later (a variable and an inlined method's
     `this`). When another name may hold the object on some paths only, it stays on the heap.
     Fields holding counted values qualify too: lowering drops and shares a pointer field
     from a copy of it, never through the field's address.
   - `sroa` then splits those aggregate locals, and others whose address is never taken, into
     one local per field.
   - `numrep` stores a `number` (`f64`) as an `i32` or `i64` where its facts (interval, whole, never
     NaN, `-0` unobservable) prove the integer computes the same values ([design
     #525](https://github.com/velt-lang/velt/issues/525)). It runs after the two above, so the
     fields they turned into locals can become integers too, and once before the inlining rounds,
     while an array is still one value whose length (at most 2^53 - 3; enforced lower) bounds the
     loops over it. A parameter that every call sets to a constant takes that constant's facts. A
     `number[]` that never leaves its function (its data pointer, after `sroa`, is only offset,
     compared, grown and freed) and whose stores only ever write whole numbers within ±2^53, never
     `-0`, keeps its elements as `i64` (#1599): loads and stores convert exactly, and the narrowed
     values use the integers directly. The element facts are a fixpoint over the stores, so a
     permutation shuffled in place qualifies; a value that grows from the elements themselves
     (`xs[i] = xs[i - 1] + 1`) has no provable bound and stays a double.
   - `dead_fills`, for the objects that stay on the heap, drops the zero fill of `new` when the
     code right after the allocation writes every field (padding aside) before anything can
     read the object: before a branch, and before the pointer is passed, stored or compared.
   - `vtable_loads` makes virtual calls on objects of a known class direct: it forwards the vtable
     pointer a new object's header gets to the loads of it that follow (while the object is still
     private to the function), and folds loads of method slots from vtables, which are read-only
     statics. `constfold` then calls the method directly, the next round inlines it, and `heap_sroa`
     can keep the object in locals. It and `dead_fills` share `fresh`, the walk of the code right
     after an allocation.

   Debug builds run only the cheap part: the int32 helpers and std's forwarders inlined, CFG
   simplification, `numrep`, and removal of unused functions (`dead_funcs`).
6. **Generate code**: Cranelift for debug builds and the `velt dev` JIT (its IR verifier runs in
   the compiler's own debug builds or with `VELT_CL_VERIFY=1`; `VELT_CL_REGALLOC=single_pass`
   trades slower programs for faster builds); textual LLVM IR,
   compiled by clang (`-O3`, or `VELT_LLVM_OPT`, for release builds; unoptimized for `--backend
   llvm`) and, for WebAssembly, by LLVM's `opt` and `llc`. Large programs are split into codegen
   units that clang compiles in parallel (`velt_codegen_llvm::units`); the root module's
   functions (the entry file's) get a unit of their own, so a release build after an edit of the
   entry file recompiles that unit and takes the library units from the build cache, unless the
   edit changes library code (a new call of a library function, a string a library also uses).
   The program's other modules are not told apart from packages and stay with the libraries.
   Calls that would grow a
   function past velt_opt's caller limit are `noinline` (`velt_codegen_llvm::inlining`), so LLVM's
   inliner never merges a program into one huge function.
7. **Link** (`velt_link`): the toolchain's bundled lld with a link kit for the target, else the
   system linker (MSVC `link.exe`, or `cc`), with the runtime library ([Linking](linking.md)).

The **runtime** (`velt_rt`) is a Rust static library: memory allocation (mimalloc), strings,
formatting, JSON, regular expressions, promise combinators, and the tokio-based I/O the standard
library builds on (file system, sockets, timers, processes). TLS (`std/net/tls`) and HTTP are
written in Velt (`std/net/http`), and the database drivers are packages (`packages/sqlite`,
`packages/postgres`, `packages/redis`). On native targets the executor that runs tasks, microtasks
and timers is written in Velt (`std/lang/hooks/sched.velt`): lowering calls its hooks where it would
call the runtime's executor (`crates/velt_vir/src/lower/sched.rs`), and the runtime's I/O wakes its
tasks through a C waker. `velt_rt_wasm` is the single-threaded WebAssembly runtime, which keeps its
own executor.

## Crates

| Crate | Role |
|---|---|
| `velt_common` | spans, source maps, diagnostics |
| `velt_syntax` | lexer, parser, AST |
| `velt_sema` | semantic analysis, HIR, the editor query API |
| `velt_loader` | the program loader: root file → every module the program needs (std, packages, npm sources), parsed |
| `velt_vir` | HIR → VIR lowering, VIR verifier |
| `velt_opt` | VIR optimizer and interpreter |
| `velt_codegen_cl` | Cranelift backend (objects and JIT, hot swap) |
| `velt_codegen_llvm` | LLVM backend (textual IR, clang) |
| `velt_link` | linker driver (bundled lld + link kits, or the system linker), runtime discovery; `velt-kit` builds the kits |
| `velt_rt` | native runtime |
| `velt_rt_shared` | the same runtime built as a shared library, which debug builds link against |
| `velt_rt_wasm` | the WebAssembly runtime and its JavaScript glue |
| `velt_rt_host` | the runtime's sources built as an rlib, which hosts JIT-compiled programs in `velt dev` |
| `velt_text` | Velt's string encoding, number formatting and database JSON, shared by the runtime and std's native libraries |
| `velt_tls` | a rustls client setup (Mozilla's roots, extra CAs) for native libraries (`packages/postgres/native`); programs' TLS is Velt's, std/net/tls/ |
| `velt_build_cache` | the machine-wide build cache: compiled code keyed by content, bounded in size |
| `velt_fmt` | formatter |
| `velt_lsp` | language server |
| `velt_doc` | API docs (`velt doc`: HTML pages and JSON) |
| `velt_tscompat` | `velt check --ts-compat`: the lint for code shared with TypeScript |
| `velt_lint` | lint profiles, levels, `velt-allow` comments and fixes; the `perf/…` rules of `velt check --perf` |
| `velt_native`, `velt_native_macros` | the Rust side of a package's native library, and its `#[export]` attribute |
| `vpm`, `velt_registry` | package manager, registry server |
| `velt_http` | a minimal HTTP/1.1 server and client for the developer tools |
| `velt_toolchain` | side-by-side toolchain versions: the `velt` pin of `package.velt`, installed versions, downloading a release |
| `velt_launcher` | the `velt` on `PATH`: picks the toolchain version and runs its `velt` |
| `veltc` | the `velt` CLI: driver, dev supervisor and host, test runner, playground |
| `xtask` | repository tooling: the quality gate and its check selection (`cargo xtask`) |

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
| How TSX is lowered, and what a JSX provider exports | [contracts/jsx.md](contracts/jsx.md) |
| Native libraries of packages | [contracts/native_abi.md](contracts/native_abi.md) |
| Host builds for JavaScript platforms (`velt build --host`) | [contracts/workers.md](contracts/workers.md) |

## Design notes

Each note explains one part of the design: what it is, why, how it compiles and what it costs.
The notes follow the rules in [design/README.md](design/README.md); the status line of each says
whether it is implemented, partly implemented, accepted or proposed.

**Language model**

- [JavaScript semantics without a garbage collector](design/semantics.md): values, sharing and
  reference counts, strings, promises, modules and threads.
- [Reference cycles](design/cycles.md): cycle-capable types, their collector, and `weak`.
- [Weak references without a tracing collector](design/weak-refs.md): `WeakMap`, `WeakSet` and
  `WeakRef`.
- [JavaScript string semantics](design/strings.md): UTF-16 lengths and positions over WTF-8
  storage.
- [Building strings fast](design/string-building.md): `StringBuilder` and in-place appends.
- [Casts across layouts](design/casts-across-layouts.md): checked downcasts, widening and method
  slots.
- [Declared write effects](design/write-effects.md): what a function may write, checked at its
  callers.
- [Velt's own unwinding](design/unwinding.md): panics that Velt code raises and catches, with
  no Rust frame involved.

**TypeScript surface**

- [TypeScript alignment](design/ts-alignment.md): inferred mutation, discriminated unions, typed
  errors, `extend`, retroactive `implements` and the smaller alignments.
- [Data models shared with TypeScript](design/shared-models.md): field-only interfaces as object
  types and the utility types.
- [Utility types and `keyof` on type parameters](design/deferred-types.md): `Partial<T>`,
  `keyof T`, `T[K]` and `T & U` in generic code.
- [`Record<K, V>`](design/record.md): TypeScript object syntax over an insertion-ordered
  dictionary.
- [Iteration, generators and `for await`](design/iteration.md): the iterator protocol with typed
  errors.
- [Operator overloading](design/operators.md): operators as methods with well-known symbol keys.
- [TSX for server-side rendering](design/tsx.md) and
  [typed causes of render errors](design/jsx-render-errors.md).
- [Doc comments](design/doc-comments.md): JSDoc and `///` comments in `velt doc` and the editor.

**Performance and native code**

- [Intrinsics](design/intrinsics.md): SIMD, CPU features and the math operations std builds on.
- [Packages that compile to hardware](design/package-machine-code.md): inlining decorators,
  CPU gating, vendor instructions and layout decorators for packages; `@intrinsic` stays std's.
- [Atomics](design/atomics.md): atomic values, `shared` references, epochs and `ConcurrentMap`.
- [Generic instances shared by shape](design/shape-sharing.md).
- [Native code through `declare c` and `unsafe`](design/c-ffi.md): calling C and the platform's
  C library, checked against C headers.
- [Native code in packages](design/native-packages.md): a package's Rust crate and prebuilt
  per-target binaries.
- [C calls on wasm32-wasip1](design/wasi-libc.md).

**Standard library and engines**

- [Standard library layout](design/std-layout.md): `velt/<area>` imports and the rules
  `std-lint` enforces.
- [Standard library checked and loaded on demand](design/lazy-std.md).
- [Compiling std's Velt code once](design/std-object.md).
- [Printing typed values](design/inspect-shapes.md).
- [Source generators](design/source-generators.md): Velt code that generates Velt code at
  build time.
- [The JSON engine](design/json-engine.md), [a SQLite engine in Velt](design/sqlite-engine.md)
  and [HTTP/2](design/http2.md).
- [Velt on Cloudflare Workers](design/cloudflare-workers.md).

**Packages and tooling**

- [A package manifest written in Velt](design/package-manifest.md): `package.velt`.
- [Workspaces](design/workspaces.md): monorepos of Velt packages.
- [Build architecture: units with interfaces](design/build-architecture.md): caching and
  separate compilation.
- [Fast dev loop and hot reload](design/hot-reload.md) and
  [a warm front end for `velt dev`](design/dev-warm-front-end.md).
- [Variables in the debugger](design/debug-variables.md).
- [Lint profiles and performance findings](design/lint-profiles.md).

## Testing

- Unit tests next to the code; cross-module tests in each crate's `tests/`.
- **End-to-end tests** (`tests/golden/**`): every `.velt` file with a `.out` (optionally a
  `.code` exit code and a `.stderr` the run's stderr must contain) is compiled and run in debug
  and release modes, and its output compared; one with a `.err` must fail to build with those
  messages. `examples/*.velt` with a `.out` are included.
  Directories marked `.pending` (such as `tests/golden/bugs/`, known bugs) and the goldens listed
  in `crates/veltc/tests/golden_known_failing.txt` (each with its issue) are run, but their
  failures fail the run only with `VELT_GOLDEN_STRICT=1`. The WebAssembly goldens
  (`crates/veltc/tests/wasm_goldens.rs`) run most of the M1-M4 and `lang` goldens on WebAssembly.
- **Documentation tests**: every `ts` code block in the repository's own user-facing Markdown (the
  README, the docs of `packages/http-server` and `packages/web`) is compiled
  (`crates/veltc/tests/docs.rs`); the user documentation on the website checks its examples itself.
- **Reload tests** (`tests/reload/`), **differential tests** against Node (`tests/difftest/`),
  and **fuzzing** (`fuzz/`).
- **Benchmarks** in `bench/`, with results in [bench/RESULTS.md](../../bench/RESULTS.md) and
  [bench/web/RESULTS.md](../../bench/web/RESULTS.md).
- **Measuring compile time.** Count instructions retired (cachegrind in a Linux VM, or
  `/usr/bin/time -l` on macOS), not wall-clock time on a busy machine. Rebuild to the same
  output path, as an edit-and-build loop does: the stamp beside the executable keeps the hashes
  of the files the last build read, which the next one reuses, so a build to a new path reads
  and hashes all of them again (on a small program, a tenth of a warm build). An unchanged
  program rebuilt to the same path skips the link entirely: edit a comment between builds.
