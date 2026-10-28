# Contracts

Contracts are the interfaces between crates. They are **maintainer-owned**:
agents code against them but do not edit them. If you need a change, propose it in an issue or in
your pull request (see `CONTRIBUTING.md`), work around it locally if possible (e.g. a private
helper), and explain it in your final report.

| Contract | File | Producer → Consumer |
|---|---|---|
| Spans & diagnostics | `crates/velt_common/src/lib.rs` | everyone |
| AST | `crates/velt_syntax/src/ast.rs` (with `ast/blocks.rs`, `ast/type_parts.rs`, `ast/type_level.rs`, `ast/jsx.rs`) + `parse_file` (`parse_with` for the loader: `define`, ambient declarations) | frontend → sema |
| HIR | `crates/velt_sema/src/hir/mod.rs`, `hir/intrinsic.rs`, `hir/module_state.rs` + `hir_encodings.md` + `check`, `SourceModule` (`is_std`: loaded from the std root; drivers set it, sema never derives it from the path), `effects::may_change_memory` (can evaluating an expression run code that changes memory: a call other than an intrinsic, an intrinsic handed a place to modify or move, an assignment, `new`, `await`; IR lowering holds earlier operands and re-forms element addresses on it) | sema → IR |
| VIR | `crates/velt_vir/src/vir.rs` + `lower`, `verify`; values are built with `Function::new`, `LocalDecl::new` and `Program::default()` (struct update for the rest), where a new field gets its "no information" default | IR → codegen |
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
      ├─ velt_syntax::parse_with       per file      → ast::Module (+ ambient declarations;
      │                                                 parse_file for std and the formatter)
      ├─ velt_sema::check              whole program → hir::Program (check_with: library roots)
      ├─ velt_vir::lower_checked (+ verify)           → vir::Program (with source locations)
      ├─ velt_opt::optimize                           → vir::Program (release: all passes)
      ├─ velt_codegen_cl::emit_object                 → main.o / main.obj (debug builds)
      │  or velt_codegen_llvm::emit_objects_timed     → one object per codegen unit (release)
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
  calls `lower_checked`, the same with errors for what lowering cannot represent yet, see
  hir_encodings.md "Undefined"); `velt_vir::lower` keeps its signature and output (no locations). With a source map,
  compiler-emitted panics end in ` at <path>:<line>:<col>` (bounds checks, division by zero,
  `panic()`, and standard-library helpers such as `unwrap()`/`assert*` report their caller; a
  location in a standard-library file reads `std/<path below the std root>`, the same on every
  machine, while debug info keeps the path as loaded), and an
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

## Namespaces and call signatures (additive AST extension)
- `ItemKind::Namespace(Namespace)` (`ast/blocks.rs`): `[declare] namespace N { … }` with its
  items (type aliases, interfaces, nested namespaces; each `Item::exported` as written).
  `namespace A.B { … }` is `A` holding the exported namespace `B`. `declare global { … }` (an
  `ItemKind::Augment`) may hold namespaces too. Sema flattens them into items named `N.T`.
- `CallSig { sig: FnSig, construct, span }` (`ast/type_parts.rs`): a call signature
  `(x: T): R` / `<T>(x: T): R`, or with `construct` a construct signature `new (x: T): C`, in an
  interface or object type, as written (`sig.name` is empty; parameters keep their names, `?`
  and initializers). `InterfaceDecl::call_sigs` holds an interface's;
  `TypeExprKind::Callable { sigs, members }` is an object type with signatures (`members`: the
  `Object` or `IndexSig` of its other members). Sema gives them TypeScript's type: one
  signature alone is that function type, otherwise the intersection of the signatures and the
  members (and an interface's bases).

## Who owns what
| Area | Crates / files |
|---|---|
| maintainers | contracts above, `tests/golden/**`, `CLAUDE.md`, `docs/**`, root `Cargo.toml` |
| frontend | `crates/velt_syntax` (except `ast.rs`) |
| semantics | `crates/velt_sema` (except `hir/`) |
| ir | `crates/velt_vir` (except `vir.rs` data types — `Display` impl may be improved) |
| optimizer | `crates/velt_opt` |
| codegen | `crates/velt_codegen_cl`, `crates/velt_codegen_llvm` |
| runtime | `crates/velt_rt`, `std/**` |
| tooling | `crates/veltc`, `crates/velt_link`, `crates/vpm` (except `tests/golden.rs`), `crates/velt_doc`, `crates/velt_tscompat`, `crates/velt_registry`, `crates/velt_http`, `crates/velt_lsp`, `crates/velt_fmt`, `crates/xtask` |
| native SDK | `crates/velt_native`, `crates/velt_native_macros` (versioned with the native ABI), `packages/**` |
| runtime (wasm) | `crates/velt_rt_wasm`, `crates/velt_rt_host` (mirror of velt_rt deps) |
| runtime packaging | `crates/velt_rt_shared` (the runtime as a shared library for debug builds; mirror of velt_rt deps) |

## SIMD (additive VIR and HIR extension)
- `vir::Ty::V128`: a 128-bit vector, a scalar (registers, signatures) of 16 bytes aligned to 8
  (`Ty::scalar_align`). Only moved, loaded and stored; its one constant is `Const::Int(0)`;
  `Rvalue::Unary`, `Binary` and `Cast` on it are rejected by `verify`. Operations are calls of
  `velt_simd_*` externs, which backends emit inline (`velt_vir::simd`, rt_abi.md "SIMD").
- `hir::TyKind::Simd(SimdTy)`: the vector and mask types; Copy, no drop, not equatable.

## Parameter attributes (additive VIR extension)
- `vir::Function::param_attrs: Vec<ParamAttrs>` — empty (no information) or one entry per param
  (checked by `verify`); `ParamAttrs` (`noalias`, `readonly`, `frozen`, `nonnull`,
  `dereferenceable: u64`), only on `Ptr` params. `Function::param_attr(i)` tolerates the empty list.
- Invariant (vir.rs 9): set only where sema's exclusivity guarantees hold, and every caller
  upholds them. Lowering sets them on user functions: `BorrowMut` (inferred-modified) aggregate
  (or class `this` borrowed mutably) → `noalias`; `Borrow` aggregate → `readonly` (not for
  values holding a `Mutex` inline, nor params the body modifies — `LocalDef::mutable`, e.g.
  closure params); the same `Borrow` params → `frozen` also when the value holds an `Atomic`
  (nothing changes the pointee during the call but atomics' words, which only `velt_atomic_*`
  calls access); neither `noalias` nor `frozen` for a type whose values module state can reach
  (a module-level `const` / `let`, a `static` field or an imported module's variable may hold
  the argument, and a callee may write it through that variable; `readonly` still holds, as it
  covers only writes through the param); aggregate / `Result` out-pointer → `noalias`; all of
  these `nonnull` + `dereferenceable(size)`.
- `velt_opt` keeps them on clones/specializations (inlined bodies simply lose them) and uses
  `noalias` and `frozen` for redundant-load elimination; LLVM emits them as parameter
  attributes (`frozen` has no LLVM counterpart); Cranelift ignores them.

## Debug variables (additive VIR extension)
- `vir::Program::debug_types: Vec<DebugTy>` and `vir::LocalDecl::debug: Option<LocalDebug>`
  (vir/debug.rs): which source variable a local holds (declaration, end of its scope, debug
  type, held by reference, param) and how each source type is laid out, in terms of existing
  aggregates (fields are VIR field indexes; a local's own VIR type gives the encoding of a scalar
  or of an enum without payloads, which may be narrower than its natural one).
- Filled only by `velt_vir::lower_with` with `LowerOptions::debug_info`, which the driver sets
  for debug builds made for debugging (`velt build`, `velt dev --exe`; not `velt run` or
  `velt test`); empty otherwise. Invariant (vir.rs 10): every reference resolves (checked by `verify`).
- `velt_opt`: dead-code elimination keeps described locals; `numrep` moves a description to
  the narrowed local; inlined locals lose theirs. Backends turn them into debugger variables
  or ignore them.

## Imported functions (additive VIR extension)
- `vir::Linkage::Import` (vir.rs invariant 11; #1728, design/std-object.md): a function defined
  in another object of the same executable, such as the cached std object. It has no blocks (a
  declaration; the only functions without an entry block) or a body kept only for `velt_opt` to
  inline; the printer shows a declaration as `fn#<i> import <symbol>(<params>) -> <ret>`.
- Backends declare it under its symbol and never emit its body: LLVM `declare hidden` with its
  param attributes, in every codegen unit that refers to it (no unit defines or copies it);
  Cranelift a `Linkage::Import` function.
- `velt_opt`: treated as `Export` for anything that rewrites a signature, param attributes or
  caller assumptions (`numrep` param facts, the inline-once rule, specializations); a pass may
  clone one only when its body is kept, and the clone is the program's own `Internal` function. `keys::disambiguate` never renames an
  `Import` symbol.
