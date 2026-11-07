# Contracts

Contracts are the interfaces between crates. They are **maintainer-owned**:
agents code against them but do not edit them. If you need a change, propose it in an issue or in
your pull request (see `CONTRIBUTING.md`), work around it locally if possible (e.g. a private
helper), and explain it in your final report.

| Contract | File | Producer → Consumer |
|---|---|---|
| Spans & diagnostics | `crates/velt_common/src/lib.rs` | everyone |
| Well-known std modules | `crates/velt_common/src/std_modules.rs`: the std modules the compiler names (`std_module!`, `std_item!`, …; docs/internals/design/std-layout.md §5) | std ↔ every compiler crate |
| Compiler hooks | `crates/velt_common/src/std_hooks.rs`: the std functions and classes the compiler calls (`std_hook!`), with their signatures (docs/internals/design/std-layout.md §10b); `hook_demand.rs`: the HIR constructs whose lowering calls a hook, and the set of hooks sema demands (`hir::Program::hooks_demanded`) | std ↔ sema, lowering |
| AST | `crates/velt_syntax/src/ast.rs` (with `ast/blocks.rs`, `ast/type_parts.rs`, `ast/type_level.rs`, `ast/jsx.rs`, `ast/num_array.rs`: `ExprKind::NumArray`, a packed array literal of 16 or more number literals, `Expr::unpacked` giving back the `Array` it stands for) + `parse_file` (`parse_file_unpacked`: the same without packing, for the formatter; `parse_with` for the loader: `define`, ambient declarations; `encode_module`/`decode_module`: a module in a compact binary form, `AST_FORMAT` first, spans stored without their file and decoded for the file id given, a damaged value `None`) | frontend → sema |
| Program loader | `crates/velt_loader/src/lib.rs`: `load_program(sm, root, LoadOptions, diags) -> Result<Loaded, String>` and the resolution rules a tool must share with it (§ Program loader below) | loader → driver, language server, tests and tools that load programs as `velt build` does |
| HIR | `crates/velt_sema/src/hir/mod.rs`, `hir/intrinsic.rs`, `hir/module_state.rs`, `hir/const_array.rs` (`ExprKind::ConstArray`: an array of number literals of one number type, as the `ArrayLit` of its elements) + `hir_encodings.md` + `check`, `SourceModule` (`is_std`: loaded from the std root; `is_dependency`: loaded from a package the program depends on, a library like std for the analyses that report at the program's call into one, with no privilege; drivers set both, sema never derives them from the path), `effects::may_change_memory` (can evaluating an expression run code that changes memory: a call other than an intrinsic, an intrinsic handed a place to modify or move, an assignment, `new`, `await`; IR lowering holds earlier operands and re-forms element addresses on it) | sema → IR |
| VIR | `crates/velt_vir/src/vir.rs` + `lower`, `verify`, `fingerprint::{Canon, Mode, Fingerprint, fingerprint, attrs_hold, is_state_layout}` (function fingerprints for `velt dev` and the cached std object; `Canon::reaches_mutable_static`); values are built with `Function::new`, `LocalDecl::new` and `Program::default()` (struct update for the rest), where a new field gets its "no information" default | IR → codegen |
| Codegen API | `crates/velt_codegen_cl/src/lib.rs` (`emit_object`, `host_triple`) | codegen → driver |
| Link API | `crates/velt_link/src/lib.rs` (`link`, `find_runtime_lib`) | tooling → driver |
| Runtime ABI | `docs/internals/contracts/rt_abi.md` + `rt_abi_async.md` | runtime ↔ IR lowering |
| CLI | `docs/internals/contracts/cli.md` | tooling ↔ golden tests |
| Manifest, lockfile, registry | `docs/internals/contracts/manifest.md` | vpm ↔ tooling, registries |
| Native libraries of packages | `docs/internals/contracts/native_abi.md` + `crates/velt_native` | packages' Rust crates ↔ runtime, vpm, compiler |
| Host builds | `docs/internals/contracts/workers.md`: `velt build --host <package>`'s output, host packages, the glue's server core, the host module `velt:worker` | glue ↔ host packages' entries, the Velt code serving through `velt:worker`, packages' host modules |
| Language semantics | https://veltlang.com/docs/reference/ + `tests/golden/**` | everyone |

## Pipeline
```
velt build main.velt
  └─ driver (veltc)
      ├─ velt_loader::load_program     main.velt + imports → SourceModules, SourceMap
      │   └─ velt_syntax::parse_with   per file      → ast::Module (+ ambient declarations;
      │                                                 parse_file for std and the formatter;
      │                                                 std's parses come from the build cache)
      ├─ velt_sema::check              whole program → hir::Program (check_with: library roots)
      ├─ velt_vir::lower_checked (+ verify)           → vir::Program (with source locations)
      ├─ velt_opt::optimize                           → vir::Program (release: all passes)
      ├─ velt_codegen_cl::emit_object                 → main.o / main.obj (debug builds)
      │  or velt_codegen_llvm::emit_objects_timed     → one object per codegen unit (release)
      └─ velt_link::link  (+ velt_rt staticlib)       → executable
```
Diagnostics from any stage are rendered with `Diagnostic::render` to stderr; exit code 1 on errors.
A `velt_common::Diagnostic` may carry a lint `code` (`Option<Box<str>>`: `perf/number-double`,
`ts-compat/bool-type`; `None` for the compiler's own) and `fixes: Vec<Fix>` (changed: both
added, docs/internals/design/lint-profiles.md). A `Fix` is a `title`, `edits: Vec<(Span,
String)>` (replacements, possibly in several files, applied together or not at all) and an
`applicability`: `Safe` (the program behaves the same; `velt check --fix` and editors' "fix all"
apply it) or `Review` (it may change what the program does; `--fix=review`). `render` does not
print them; a lint adds its code as the last note (`perf(number-double)`).

## Program loader (crate boundary, #2197)
`crates/velt_loader` loads a program: the root file and every module it needs (the prelude, the
globals loaded on demand with their `// load for:` / `// load when` conditions, std, packages,
npm sources, `#name` variants), parsed, in the order sema expects. It depends on `velt_common`,
`velt_syntax`, `velt_sema` (for `SourceModule`), `vpm`, `velt_build_cache` and
`velt_toolchain`, never on veltc, VIR or a backend, so whatever must load programs exactly as
`velt build` does (velt_vir's tests, the language server, a per-package cache) can depend on it.
Its public API is kept small:
- `load_program(sm: &mut SourceMap, root: &Path, opts: LoadOptions, diags: &mut Diagnostics)
  -> Result<Loaded, String>`: `Err` only when the root cannot be read; syntax and import problems
  go into `diags`. `Loaded { modules: Vec<SourceModule>, root: usize, state: LoadState }`: the
  prelude first, then the root, its imports breadth-first, then the extra roots, then (without
  `demand_std`) the modules a check could demand.
- `load_more(sm, state: &mut LoadState, base: &[SourceModule], demanded: &[String], opts:
  &LoadOptions, diags) -> velt_sema::StdBatch`: goes on from a load with the std modules a check
  demands (canonical paths) and what they import; each must follow `std_index`'s rule. Sema calls
  it through the driver's `velt_sema::LoadFn` (design/lazy-std.md "Loading on demand").
  `demand_std_on()`: whether the environment lets a check do that.
- `LoadOptions` (all fields default to "none"): `demand_std` (leave what a check demands to
  `load_more`), `std_root`, `packages: Option<&dyn
  PackageResolver>` (implemented for `vpm::PackageGraph`), `extra_roots`, `root_source`,
  `overlay` (unsaved editor buffers), `std_root_as_std`, `test_harness`, `target`,
  `host_condition`, `check_variants`, `std_ast_cache: Option<StdAstCache { cache, compiler }>`
  (std's parses kept in the build cache; the caller names the cache and the compiler identity).
- The resolution rules tools must share with the loader: `std_root()`, `prelude_files`,
  `in_std`, `resolve_spec` → `ModuleRef`, `resolve_module`, `module_path`, and
  `PackageResolver` / `PackageImports`.
- The std index (#2161 step 1, `std_index`): `std_index::load(std_root, Option<StdAstCache>)
  -> StdIndex`, per std package (`std/<module>/`) and module: the hook table's entries it
  defines, its `extend` members as (type, member, static), its exports and imports, and the
  (a1) rule's `violations` (`ModuleIndex::in_place`). Stored per package in the build cache (kind
  `pkg-interface`, `std_index::KIND`: the first fields of package-cache.md's layer 1; versioned
  by `std_index::FORMAT`). Nothing in a build calls it yet.

Everything else (resolution, globals, visibility, variants, the std parse cache) is private to
the crate. The same inputs give the same modules, diagnostics and order for every caller.

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

## Declared intrinsics (additive AST extension)
- `FnSig::is_intrinsic` (`ast.rs`): `declare intrinsic function name<T>(…): R;`, an
  `ItemKind::ExternFn` like `declare system function` (`intrinsic` is a word only between
  `declare` and `function`; never `async`). Only std declares them, in `std/lang/intrinsics/`
  (`std_modules`: `lang_intrinsics`); elsewhere sema reports the declaration. A call is checked
  against the declaration and is the same HIR `Call { Intrinsic(..) }` as before
  (hir_encodings.md); the name maps to the `Intrinsic` in `velt_sema::intrinsic_decls`, whose
  test fails on a declaration without a table entry or an entry without a declaration
  (docs/internals/design/std-layout.md §10).

## Host functions (additive AST, HIR and VIR extension, #1735)
- Decorators (`ast.rs`, https://github.com/velt-lang/velt/issues/2203): `Decorator { expr, span }`
  on `FnSig`, `Param`, `Field`, `TypeDecl` and `EnumDecl` (`decorators`, empty without any), and
  `FnSig::is_decorator` for `declare decorator function name(…): Target;`, where `Target` says
  where it may be used (`FunctionDecorator`, `MethodDecorator`, `TypeDecorator`, `FieldDecorator`,
  `ParamDecorator`, a union of them, `Repeatable<…>` around them). Those names mean this only
  there. A function's decorators are on its implementation's `FnSig`, wherever they are written
  among its overloads. `Method::is_property` marks the getter and setter of an abstract property
  (`abstract x: T;`), whose decorators are a field's. `AST_FORMAT` 12. Sema records the uses in `Program::decorators` (hir_encodings.md
  "Decorators") and `DefKind::Decorator` names the declarations for editors.
- `FnSig::host: Option<HostModule { module, span }>` (`ast.rs`): `declare host("<module>")
  function name(…): R;`, an `ItemKind::ExternFn` like `declare system function` (`host` is a
  word only between `declare` and `(`). Any module may declare one
  (https://veltlang.com/docs/reference/host-functions/).
- `hir::ExternFnDef::host: Option<HostImport { module, params: Vec<HostTy>, ret: HostTy }>`
  (`hir/extern_fn.rs`): a function the JS host provides, the WebAssembly import
  `module`.`symbol`, with what crosses per parameter and for the result (`HostTy`: an integer,
  `F64`, `Bool`, `Str`, `Bytes { readonly }`, `Ref { nullable }` for std's `HostRef`, `Void`). Not
  an extern of the link: sema has checked the types, and every declaration of one import has the
  same ones. Calls pass `u8[]` parameters `BorrowMut` and take views of bytes there, as for
  system functions (hir_encodings.md).
- `vir::ExternFn::host: Option<HostImport { module, name, params: Vec<HostValue>, ret }>`
  (`vir/host.rs`): the extern is that import, not a link symbol; its `symbol` only names it in
  the object. The extern's signature per value and the manifest the glue reads (the custom
  section `velt.host`, a line per import) are in `vir/host.rs`. Lowering makes one only for
  `wasm32-unknown-unknown`; on any other target a reachable call is an error, as for a system
  function the target lacks. The LLVM backend declares it with `wasm-import-module` /
  `wasm-import-name` and writes the manifest; other backends never see one.
- Async host functions (#1737): `declare host("m") async function f(…): Promise<T>` has
  `ExternFnDef::is_async` with `host` (`HostImport::ret` is what `T` crosses as) and `ret`
  `Promise<T, IoError>`; `vir::HostImport::is_async` (additive) marks its import, which returns
  the `u32` id of the JS promise (manifest result `async:<type>`). Its call is the import, the
  leaf `velt_rt_host_promise` and std's hook awaiting it (rt_abi_async.md §1.4), whose
  `HostSettled<W>`, `hostAwait<T>`, `hostAwaitVoid`, `hostAwaitRef` and `hostAwaitRefOrNull`
  (std/host/promise.velt) are `std_hooks` entries; the global std/prelude/global/host_async.velt
  loads them for a module declaring one. On JS hosts `velt_main` hands `main`'s root to
  `velt_rt_host_main` with the code after it (VIR `Work::MainFinish`) instead of calling
  `velt_rt_block_on` (rt_abi_async.md §2.5).
- std's own host functions (#1785): the import module `velt:fetch`
  (std/net/http/fetch/client/transport/js_host.velt), `fetch`'s transport on JS hosts, which the
  glue provides itself (rt_abi.md "Host values").

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
| tooling | `crates/veltc`, `crates/velt_loader`, `crates/velt_link`, `crates/vpm` (except `tests/golden.rs`), `crates/velt_doc`, `crates/velt_tscompat`, `crates/velt_registry`, `crates/velt_http`, `crates/velt_lsp`, `crates/velt_fmt`, `crates/xtask` |
| native SDK | `crates/velt_native`, `crates/velt_native_macros` (versioned with the native ABI), `packages/**` |
| runtime (wasm) | `crates/velt_rt_wasm`, `crates/velt_rt_host` (mirror of velt_rt deps) |
| runtime packaging | `crates/velt_rt_shared` (the runtime as a shared library for debug builds; mirror of velt_rt deps) |

## SIMD (additive VIR and HIR extension)
- `vir::Ty::V128`: a 128-bit vector, a scalar (registers, signatures) of 16 bytes aligned to 8
  (`Ty::scalar_align`). Only moved, loaded and stored; its one constant is `Const::Int(0)`;
  `Rvalue::Unary`, `Binary` and `Cast` on it are rejected by `verify`. Operations are calls of
  `velt_simd_*` externs, which backends emit inline (`velt_vir::simd`, rt_abi.md "SIMD").
- `hir::TyKind::Simd(SimdTy)`: the vector and mask types; Copy, no drop, not equatable.

## Intrinsics (additive VIR extension)
- `vir::Rvalue::Intrinsic(Intrinsic, Vec<Operand>)` and `vir::Intrinsic` (#1981; vir.rs
  invariant 12): a pure operation the compiler knows, as a statement instead of a call: it reads
  only its operands and touches no memory, so passes may fold, move, duplicate or delete it like
  arithmetic. `Intrinsic::signature` gives operand and result types, `Intrinsic::name` its
  spelling in dumps (`math.sqrt(_3)`). `verify` checks the types.
- `Intrinsic::Math(MathOp)` (`velt_vir::math`): means exactly the call of the runtime function
  `MathOp::symbol`, which the program must declare (checked by `verify`); backends emit it
  inline as they do the call, and call that function for `pow` and ToInt32's cold path.
  Lowering emits every call of a math operation's runtime function in this form (`call` in
  `lower/cfg.rs`), and `verify` rejects a direct call of one, so passes and backends know math
  operations in this form only (an indirect call through the function's address is an
  ordinary call). `velt_opt`'s `constfold` evaluates it on
  constants (`MathOp::eval`) and `numrep` narrows through it; the interpreters run it as the
  call.

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

## Pointer arguments
- A callee only borrows a `Ptr` argument for the duration of the call: it carries the
  argument's address, or one computed from it, out of the call only through a `Ptr` result,
  never by storing it in memory (module state, memory it was given, an aggregate it returns
  through its out-pointer). `velt_rt_set_throw_loc` keeps its pointer, which is always a
  static string's. The Cranelift backend relies on this to share stack slots
  (`velt_codegen_cl` `function/slots`): a local whose address a call receives is live up to the
  call, or up to the last use of a `Ptr` the call returns.
- A function's out-pointer (its last param, which lowering names `ret.out`) points to memory
  the caller has not initialized: the callee writes its whole result there before it reads it,
  so a call that passes `&x` there writes all of `x` (lowering passes a local of the result's type).

## Cold functions (additive VIR extension)
- `vir::Function::cold: bool` — no call of the function returns normally: its declared result
  is `never` (it panics, throws, exits or loops forever). False promises nothing; `Function::new`
  sets it false. Set by lowering on every `never` function instance; `velt_opt` keeps it on
  clones.
- A call of one that throws (its result through an out-pointer) is followed, as for any
  throwing callee, by a branch on the result's tag; the Ok side of that branch is
  `Unreachable`, so the code after the call is the error path only.
- `velt_opt` never inlines a cold function (and `cold_calls` makes a function's calls of one a
  single call); the LLVM backend marks it `cold noinline` (as it does the helpers that never return, such as
  `_Goob_u64`); Cranelift ignores it.

## Inlining markers (additive VIR extension)
- `vir::Function::inline: vir::Inline` (`Default`, `Always`, `Never`): a std function's own
  statement `inline()` (`Always`) or `noInline()` (`Never`, #1884), declared in
  `std/lang/intrinsics/inline.velt` (`hir::Intrinsic::Inline` / `NoInline`, which lower to no
  code). One field, so "both" cannot be represented. Sema rejects a marker that is not the first
  statement of a declared function's or method's body, or is in a closure, a generator or an
  async function (whose body lowering compiles into a resume function), so a function has at
  most one; and `inline()` on a function on a cycle of direct calls. `Function::new` sets
  `Default`; lowering sets it from the body; `velt_opt` keeps it on clones.
- `velt_opt`'s inliner inlines an `Always` function at every direct call site, whatever its
  size and the caller's, without charging the program's growth budget (the recursion and
  gated-target rules still apply), and never inlines a `Never` one. A debug build runs only the
  inlining of std's trivial forwarders (`inline::std_forwarders`), which skips `Never` functions
  and does not honour `Always`: debug code keeps its calls, so a debugger steps into them.
- The LLVM backend marks `Always` `alwaysinline` and `Never` `noinline` (not `cold`); a
  function that never returns stays `cold noinline` whatever its marker. Cranelift ignores the
  field.

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
- A body-less `Import` is opaque to every pass (#1862): it may write through any pointer it is
  given and call back; per-function passes skip it.
- **A declared entry** (#1728 step 3): code compiled on its own to link into a program (the std
  cache, cli.md "Std cache") declares `velt_main` (`Import`, `() -> I32`) instead of defining
  it; verification accepts that (vir.rs invariant 5). A program still defines it.

## Named statics (additive VIR extension)
- `vir::StaticData::symbol` and `::linkage` (vir.rs invariant 13; #2189, design/std-object.md
  "The std cache"): a static is private to the program's objects (`Internal`, no symbol, as
  before) unless it is `Export` (defined here, visible to the link's other objects under its
  symbol) or `Import` (defined by another object under its symbol; its `bytes`, `relocs` and
  `mutable` are its contents there, for reading only). Symbols are unique among the program's
  statics, functions and externs, and an `Import` static's relocations name only
  `Export`/`Import` functions and statics or externs (both checked by `verify`).
- Backends: LLVM emits a named static under its symbol, `hidden` (`Export`) or
  `available_externally hidden` (`Import`), in every unit that refers to it, as units already
  share statics under `.s<n>`; Cranelift declares it as `Hidden` or `Import` data under its
  symbol and defines only the `Export` one.
- `fingerprint` (both modes) prints a named static's symbol and linkage with its contents.
- Lowering never names a static; only the std cache does (`VELT_STD_OBJECT=1`), so builds without
  it are unchanged.
