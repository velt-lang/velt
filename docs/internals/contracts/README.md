# Contracts

Contracts are the interfaces between crates. They are **maintainer-owned**:
agents code against them but do not edit them. If you need a change, propose it in an issue or in
your pull request (see `CONTRIBUTING.md`), work around it locally if possible (e.g. a private
helper), and explain it in your final report.

| Contract | File | Producer → Consumer |
|---|---|---|
| Spans & diagnostics | `crates/velt_common/src/lib.rs` | everyone |
| AST | `crates/velt_syntax/src/ast.rs` + `parse_file` | frontend → sema |
| HIR | `crates/velt_sema/src/hir.rs` + `hir_encodings.md` + `check`, `SourceModule` (`is_std`: loaded from the std root; drivers set it, sema never derives it from the path) | sema → IR |
| VIR | `crates/velt_vir/src/vir.rs` + `lower`, `verify` | IR → codegen |
| Codegen API | `crates/velt_codegen_cl/src/lib.rs` (`emit_object`, `host_triple`) | codegen → driver |
| Link API | `crates/velt_link/src/lib.rs` (`link`, `find_runtime_lib`) | tooling → driver |
| Runtime ABI | `docs/internals/contracts/rt_abi.md` + `rt_abi_async.md` | runtime ↔ IR lowering |
| CLI | `docs/internals/contracts/cli.md` | tooling ↔ golden tests |
| Manifest, lockfile, registry | `docs/internals/contracts/manifest.md` | vpm ↔ tooling, registries |
| Native libraries of packages | `docs/internals/contracts/native_abi.md` + `crates/velt_native` | packages' Rust crates ↔ runtime, vpm, compiler |
| Language semantics | `docs/reference/` + `tests/golden/**` | everyone |

## Pipeline
```
velt build main.vlt
  └─ driver (veltc): load main.vlt, follow imports, SourceMap
      ├─ velt_syntax::parse_file       per file      → ast::Module
      ├─ velt_sema::check              whole program → hir::Program (check_with: library roots)
      ├─ velt_vir::lower_with (+ verify)              → vir::Program (with source locations)
      ├─ velt_codegen_cl::emit_object                 → main.o / main.obj
      └─ velt_link::link  (+ velt_rt staticlib)       → executable
```
Diagnostics from any stage are rendered with `Diagnostic::render` to stderr; exit code 1 on errors.

## Source locations (additive VIR extension)
- `vir::SrcLoc { file: u32, line: u32, col: u32 }` (1-based line, 1-based byte column).
- `vir::Program::files: Vec<String>` — source path per `FileId` (`SrcLoc::file` indexes it).
- `vir::Function::locs: Vec<Vec<Option<SrcLoc>>>` — per block, one entry per statement plus a
  last one for the terminator; empty = no information (vir.rs invariant 8, checked by `verify`).
  Helpers: `Function::loc(block, stmt)`, `term_loc(block)`, `first_loc()`.
- `velt_vir::lower_with(&hir, &LowerOptions { source_map, std_root, native_inits })` fills them (the driver
  calls it); `velt_vir::lower` keeps its signature and output (no locations). With a source map,
  compiler-emitted panics end in ` at <path>:<line>:<col>` (bounds checks, division by zero,
  `panic()`, and standard-library helpers such as `unwrap()`/`assert*` report their caller), and an
  uncaught error reports the location of its `throw` (`velt_rt_set_throw_loc`/`velt_rt_throw_loc`,
  rt_abi.md).
- `velt_opt` keeps `locs` aligned with every statement it moves, splits or deletes; inlined code
  keeps the callee's locations.
- Backends emit debug info exactly when the VIR has locations: the driver keeps them for debug
  builds and `-g`, and strips them for plain `--release`. LLVM: `DICompileUnit`/`DIFile`/
  `DISubprogram`/`DILocation` per statement (CodeView on Windows → PDB via `link /DEBUG`; DWARF 4
  elsewhere). Cranelift: DWARF 4 line tables (`DW_TAG_subprogram` per function and a line
  program; no types or variables) in ELF and Mach-O objects (debuggers checked on Linux; macOS
  untested), and for JIT code an in-memory ELF image per version registered through the GDB JIT
  interface (Linux; macOS untested, LLDB needs `plugin.jit-loader.gdb.enable on`); on COFF no line
  tables, and internal functions become external symbols so the PDB names them.

## Who owns what
| Area | Crates / files |
|---|---|
| maintainers | contracts above, `tests/golden/**`, `CLAUDE.md`, `docs/**`, root `Cargo.toml` |
| frontend | `crates/velt_syntax` (except `ast.rs`) |
| semantics | `crates/velt_sema` (except `hir.rs`) |
| ir | `crates/velt_vir` (except `vir.rs` data types — `Display` impl may be improved) |
| codegen | `crates/velt_codegen_cl` |
| runtime | `crates/velt_rt`, `std/**` |
| tooling | `crates/veltc`, `crates/velt_link`, `crates/vpm` (except `tests/golden.rs`), `crates/velt_doc`, `crates/velt_tscompat`, `crates/velt_registry`, `crates/velt_http` |
| native SDK | `crates/velt_native`, `crates/velt_native_macros` (versioned with the native ABI), `packages/**` |
| runtime (wasm) | `crates/velt_rt_wasm`, `crates/velt_rt_host` (mirror of velt_rt deps) |
| runtime packaging | `crates/velt_rt_shared` (the runtime as a shared library for debug builds; mirror of velt_rt deps) |

## Parameter attributes (additive VIR extension)
- `vir::Function::param_attrs: Vec<ParamAttrs>` — empty (no information) or one entry per param
  (checked by `verify`); `ParamAttrs { noalias, readonly, nonnull, dereferenceable: u64 }`, only
  on `Ptr` params. `Function::param_attr(i)` tolerates the empty list.
- Invariant (vir.rs 9): set only where sema's exclusivity guarantees hold, and every caller
  upholds them. Lowering sets them on user functions: `BorrowMut` (inferred-modified) aggregate
  (or class `this` borrowed mutably) → `noalias`; `Borrow` aggregate → `readonly` (not for
  values holding a `Mutex` inline, nor params the body modifies — `LocalDef::mutable`, e.g.
  closure params); aggregate / `Result` out-pointer → `noalias`; all of these `nonnull` +
  `dereferenceable(size)`.
- `velt_opt` keeps them on clones/specializations (inlined bodies simply lose them) and uses
  `noalias` for redundant-load elimination; LLVM emits them as parameter attributes;
  Cranelift ignores them.
