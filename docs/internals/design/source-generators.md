# Source generators

Status: partly implemented. Issues: [#2203](https://github.com/velt-lang/velt/issues/2203), [#1967](https://github.com/velt-lang/velt/issues/1967).

## Summary

A source generator is a Velt function that runs at compile time and writes the body of a generic
function for each concrete instance the program uses. A library declares the function with
`declare generated("<generator module>")`; the compiler finds the instances the program reaches,
hands the generator a read-only model of their types, type-checks the Velt source it returns
with the program, and compiles it like any other code.

Per-type code (serializers, decoders, structural comparison) is therefore a library feature
written in Velt, not compiler code. Std's `JSON.stringify`, `JSON.parse<T>`, velt/db's row
decoding and parameter binding, and `deepEqual`/`assertEq`/`toEqual` are generators, and they
use nothing a third-party package cannot: a MessagePack, CBOR, ORM or validation library gets
the same per-type code generation std has.

## Motivation

Without generators, per-type code has two forms, both bad. Inside the compiler it is Rust that
builds VIR by hand, with powers no library has (writing private fields, building instances
without a constructor, dispatching over every subclass of a class), and every new format needs
more Rust. Outside the compiler a library cannot see a type's fields at compile time, so it
asks its users to write codecs by hand or reflects at run time, which is slow and keeps type
metadata in every binary.

The design follows .NET's source generators: generators are code in the language, their output
is source that is type-checked with the program and can be read, and the access they need is
one documented rule. Unlike .NET, generation is driven by use, so no annotations are needed:
`JSON.stringify(user)` and `JSON.parse(text) as User` work as in TypeScript.

Generators do not rewrite user syntax, do not add declarations that users call by name, and are
not runtime reflection.

## Design

### Generated functions

A library declares a generic function without a body and names its generator module:

```ts ignore
// std/encoding/json/generated.velt
/** Appends what `JSON.stringify(value)` writes to `w` (one function per type, generated). */
export declare generated("velt/encoding/json/generate/write") function writeJson<T>(
  w: StringBuilder,
  value: T,
): void;
```

- `declare generated("<specifier>") function` declares a function whose body a generator writes
  per instance (`FnSig::generator` in `crates/velt_syntax/src/ast.rs`). The specifier is
  resolved like an import of the declaring module: a relative path, a package's module or a std
  module. The generator module is not part of the program; it is built into a separate host
  program (see "Running generators").
- The signature is the contract. Callers are checked against it like any generic function, and
  its return type must be written.
- When the program reaches the function at concrete type arguments (`writeJson<User>`, directly
  or through generic code), the compiler asks the generator for that instance's body, checks
  it, and uses it for that instance.
- For everything else the function is an ordinary function: it can be a value, be called
  through a type parameter, and have a `throws` clause. A generated body throws only what the
  declaration's `throws` lists, and all bodies share the declaration's parameter-passing modes,
  as the implementations of one vtable slot do, so callers checked against the declaration call
  every body the same way.

The public API wraps generated functions in written ones: `JSON.stringify<T>(value)` (in
`std/prelude/json/json.velt`) makes a `StringBuilder`, calls the generated `writeJsonTop<T>` and
returns the text. Users never see the generated function.

### Generators

The generator module exports a function with the same name as the declaration. It receives an
`InstanceRequest` and writes to an `Output` (both from `velt/generate`, `std/generate/`):

```ts ignore
// a generator module (tests/golden/lang/_generators/gen.velt)
import { InstanceRequest, Output } from "velt/generate";

export function show(req: InstanceRequest, out: Output): void {
  const t = req.model.type(req.args[0]);
  const v = req.params[0];
  if (t.kind == "string") {
    out.line(`return "'" + ${v} + "'";`);
  } else if (t.kind == "array") {
    const elem = out.instance("show", [t.args[0]]);
    out.line(`const parts: string[] = [];`);
    out.line(`for (const e of ${v}) { parts.push(${elem}(e)); }`);
    out.line(`return "[" + parts.join(", ") + "]";`);
  } else {
    out.error(`show cannot write a ${t.kind} (\`${t.display}\`)`, t.at);
  }
}
```

`InstanceRequest` holds the function's name (`fn`), the instance's type arguments as model ids
(`args`), the names of its parameters (`params`) and the type model (`model`).

| `Output` member | What it does |
|---|---|
| `body(code)`, `line(code)` | Appends code (or a line of code) to the body. |
| `helper(code)` | Adds a top-level function or constant next to the instance. |
| `fresh(base)` | A helper name no other instance uses. |
| `instance(fn, args)` | The name of the instance `fn<args…>` of a generated function of the same generator module, generated in the same run when it is new. This is how a generator recurses into field types. |
| `typeName(id)` | A type expression for a model type, valid in generated code even when the type has no name or is not exported. |
| `literal(s)` | `s` spelled as a Velt string literal. |
| `error(message, at, labels)` | Reports an error at a place the model holds (a type's or field's `at`), or at every call that needs the instance (`at` = -1). Lines after the first are notes; `labels` mark other places of the model. |

The compiler writes the function's header itself (name, parameters, return type and `throws`
with the type arguments substituted), so generated code cannot get the signature wrong; the
generator writes only the body and its helpers.

### Demand

The instances to generate are found by use. A pass in sema
(`crates/velt_sema/src/generated/demand.rs`) carries calls of generated functions through
generic code to concrete type arguments: a call `writeJsonTop<T>` inside `stringify<T>`
makes `stringify` need `writeJsonTop<T>`, and a call `JSON.stringify<User>` then needs
`writeJsonTop<User>`, which is concrete. Closures pass
their needs to the enclosing function, and methods called through dynamic dispatch to every
function that mentions their class. The concrete instances, with the calls that reach each (for
diagnostics), are the demand. When no checked code calls or names a generated function, the
pass walks nothing.

Demand rather than "generate for every declared type" (Rust's `#[derive]`, .NET's
`[JsonSerializable]` contexts), because TypeScript code does not annotate types for
serialization, and generating for every type would cost compile time for types never used.

### The type model

`velt/generate` defines the model as Velt classes (`Model`, `Type`, `Field`, `Method`,
`EnumMember`), built by sema (`crates/velt_sema/src/generated/model.rs`) and sent to the
generator. Types refer to each other by index, so recursive types form a graph.

- **Closed.** The model holds every type reachable from the request's type arguments through
  fields, union members, elements, bases and subclasses. A generator never needs another round
  trip to learn about a type.
- **Kinds, not layouts.** `Type.kind` is one of `number`, `boolean`, `string`, `void`, `never`,
  `symbol`, `literal`, `enum`, `array`, `tuple`, `option`, `union`, `object`, `class`, `struct`,
  `interface`, `map`, `record`, `set`, `shared`, `date`, `function`, `generator`, `any`,
  `unknown`, `promise` or `opaque`. Boxing, reference counting and VIR types are not visible. A
  std type whose structure a generator may not see (a `Mutex`, a socket) is `opaque` with its
  `qualifiedName`, so a generator can still special-case it (`JsonValue`) or reject it.
- **What a type carries:** `display` (for messages), `qualifiedName`, `repr` (`f64`, `i32`, …
  for numbers), `args` (element, payload, key and value, by kind), `arms` (union members in
  order), `fields` (inherited first, in JavaScript's order), `base` and `subclasses` (every class
  of the program that extends it; the program is compiled as a whole, so the list is closed),
  `members` (enum members), `literal` (a literal's value as JavaScript source; for an `option`,
  whether it is empty as `null`, `undefined` or both), `methods` (the hooks std's operations
  call by name: `toJSON`, `toString` and the inspect hook, with parameter count and result
  type), `ctorAccess` and `factories` (a constructor that is not public, and the public static
  methods that return the class), `mayBeProxy` (a typed `Proxy` may stand for the value),
  `dynamic`, `hasProperties`, `classValue`, `privateField` and `at`.
- **What a field carries:** `name`, `type`, `optional`, `readonly`, `isPrivate`, `isEsPrivate`,
  `hasDefault`, `isGetter` (an object literal type's getter, whose `type` is its result),
  `stdPrivate` (a std type's internal state), `via` (a field an extension's objects hold, read
  through that type) and `at`.

The model is a public std API: it changes only by adding fields.

### Access and scope

One rule replaces the compiler's private powers: **code generated for a type has that type's
access** (`crates/velt_sema/src/generated/access.rs`). In a generated module, for every declared
class and struct its requests' models reach:

- `private` fields and methods may be read and written, and a private constructor called, as in
  the class's own code;
- `__construct<T>({ …fields })` builds the class or struct without running its constructor.
  Every field without an initializer must be given; one left out takes its initializer's value,
  an optional one is absent.

ES private fields (`#x`) stay private, as in JavaScript (Node's `JSON.stringify`,
`util.inspect`, `assert.deepStrictEqual` and `structuredClone` all skip them). A library gets no
access to a type the program does not hand it. A user who wants a type kept out of a
generator's reach declares the field `#x` or the constructor `private`; the model reports both,
and std's JSON generator writes such a class but does not decode it.

A generated module is checked in the scope of the module that declares the generated function:
that module's imports and top-level declarations, exported or not, are visible, as if the
generated code were appended to it. A generated module cannot import. It is never a std module,
so it has no compiler intrinsics. Besides `__construct`, generated code may use three names that
resolve only in generated modules:

| Name | Meaning |
|---|---|
| `__type<"<instance>", <index>>` | A type handle: the type at position `index` of the walk from the type arguments of the demanded instance with that fingerprint (`generated/handles.rs`). `Output.typeName` returns these, so generated code can name any type a request reaches, including types Velt syntax cannot write (a type declared inside a function, an object type with a symbol key, an intersection). |
| `__arm(v, i)` | Whether the union `v` holds its model member `i`; narrowing understands it, so a generator can tell apart members no JavaScript test distinguishes (`i32 \| f64`). |
| `__member<E>(i)` | Member `i` of enum `E` (`generated/member.rs`). |

Generated code may also set a readonly field of a value it is building (a `const` it initialized
with an object or struct literal in the same function), as a constructor sets `this`'s. A
readonly field of a value it was given stays readonly.

### Running generators

**Packaging.** A generator is an ordinary module of a package or of std; nothing in a manifest
declares it. Its dependencies are its package's.

**The host.** The compiler builds the generator modules a program needs, with std's
`velt/generate` `serve` loop as `main`, into a separate program, the generator host
(`crates/veltc/src/generate/host.rs`). One host is built per directory its generator modules
are imported from; its root sits beside the declaring module and imports each generator by the
specifier the declaration wrote. The host is built into an executable in the build cache, with
its own build stamp; without a build cache it is JIT-compiled and run by
`velt __generate-host`. The host does not link the program's native packages.

**Purity.** A generator is a pure function of its request: no I/O (files, network, environment,
child processes, standard input), no clock, no randomness and no native code. The compiler
checks this when it builds the host, from the runtime functions and externs the registered
generators can reach (`driver::check_generator_host`, `crates/veltc/src/driver/outside.rs`),
leaving out `velt/generate`'s own plumbing. Checking reachability rather than imports catches a
std function that reads the clock as surely as an import of `velt/fs`, with no list of modules
to maintain. Purity is what makes generator output cacheable and running package code at
compile time safe.

**A child process** (`crates/veltc/src/generate/process.rs`). The compiler writes the requests
to the host's standard input and reads its records from standard output. A crash, an uncaught
`throw`, a stack overflow or `process.exit` ends the host, not the compiler, which reports it at
the call that needed the instance. A run is limited to 30 seconds (`VELT_GENERATOR_TIMEOUT`
seconds; `0` means no limit), and to 128 MB on each of standard output and standard error; a
host that exceeds either is stopped and nothing of the run is cached. A host never outlives the
compiler: on Linux it gets `PR_SET_PDEATHSIG`, on Windows it joins a kill-on-close job, and
`velt lsp` stops the hosts still running when it exits. The runtime's process-wide state (the
allocator, stdout buffers, the panic hook, the async runtime) is therefore never shared with
the compiler.

**Records.** Each record the host writes starts with a marker the compiler draws for that run
and passes in the input, which generators never see: what a generator prints is skipped and
cannot forge a record. A generator can ask only for instances of its own module's generated
functions, at types the model holds.

**Cycles.** A host whose own code needs generated code is refused ("generators need generated
code to be built, in a cycle"). Std's generators build strings with `StringBuilder` and read the
model directly, so they use no generated functions.

**Ending.** A check asks its generators at most once for the same answer: one that failed, or
whose code needs a check of its own, is not asked again by the check's later rounds, and one
that made no module for a demand ends the asking, so the instances come back as the check's
demand and the driver's rounds (eight at most) report them.

**Modules loaded on demand.** A generated function may be declared in a std module the check
loads on demand (lazy-std.md "Loading on demand"). The driver finds that module among those it
served, and a later round of the check, which starts without the module, adds the code
generated for it once the round has loaded it again. Such a program's cached offer is not used:
the first check has not loaded the module yet, so the program is checked from scratch.

### Caching

Generator work is cached in the build cache (`crates/veltc/src/generate/cache.rs`):

- **Output** (kind `generated`): what a host wrote for a round's input, keyed by the compiler's
  identity, the host program and the input (the requests and their model). The files the host's
  build read (the generators and what they import) are recorded as build-stamp entries, so a
  changed generator misses the cache.
- **Offers** (kind `generated-offer`): per program root, the generated modules its last build
  used, each instance with a fingerprint (`Model::fingerprint`: the model reachable from its
  type arguments, renumbered from them, without positions, and the function's signature). The
  first check is offered them; when they cover the demand, the program is checked once and no
  generator runs. An offered module whose instances are no longer demanded is dropped, never
  reported.
- The generators' files are part of the program's build stamp.

The steady state (an edit that changes no type a generator sees) costs one check and no
generator run.

### Std's generators

| Declaring module | Generator | Functions |
|---|---|---|
| `std/encoding/json/generated.velt` | `std/encoding/json/generate/write.velt` | `writeJson`, `writeJsonTop`, `writeJsonKeyed` (`JSON.stringify`) |
| | `std/encoding/json/generate/read.velt` | `parseJsonTop`, `readJson` (`JSON.parse<T>`, `JSON.parse(text) as T`), `parseJsonCursor` (velt/db's `decodeRows`/`decodeRow`) |
| `std/db/generated.velt` | `std/db/generate/rows.velt`, `bind.velt` | `readRowsByColumn`, `readRowByColumn`, `bindValues`, `writeValues` |
| `std/lang/equal/generated.velt` | `std/lang/equal/generate/` | `equalByContent` (`deepEqual`, `assertEq`, `Map`'s deep comparison), `equalByFields` (`toEqual`) |

A generated function and its instances share one ABI, so the ownership passes join their pass
modes. A std declaration whose every instance modifies the same parameters lists them with
`@modifies` (std/lang/intrinsics/decorators.velt, for std's `declare generated` functions only):
`writeJson` and `writeJsonTop` list `w`, `readJson` lists `r`. Those parameters take their
final mode where std declares the function, so checking a program never raises them (in a check
by units std's declaration is frozen by then: build-architecture.md "Units in sema"). The list
is a floor: an instance also runs the program's code, so a `toJSON` that changes its object
still raises `value`, as any raise. A declaration whose instances differ by type (velt/db's,
`equalByContent`) lists nothing and is joined as before.

What each operation cannot handle is reported by its generator from the model, at the user's
call (`std/encoding/json/generate/check.velt`, `parse_check.velt`); sema names no JSON function.
Dynamic values (`any`, `unknown`, interface values, typed proxies) are written over
[velt/reflect](https://veltlang.com/docs/std/reflect/), the public runtime reflection API.

What stays in the compiler are the operators of the language: `==` on aggregates, `===`,
`Object.is`, the key comparison and hash of `Map` and `Set`. They compare objects by identity or
by layout and never walk a user type's fields as a library operation. The line: an operation
that walks the structure of user types is a generator; an operator of the language is the
compiler's.

## Compilation

**Sema** (`crates/velt_sema/src/generated/`). A generated function's own body is a placeholder
that is never built; callers are checked against its signature. After the program's bodies are
checked, the generated phase (`phase.rs`) computes the demand, walks each demanded instance's
types for the model, and asks the driver's generator callback (`Generator`, over `Demand`) for
the code of the instances without one. It adds the returned modules to the same check
(`Ctx::add_module`), collects and checks the modules whose instances are all demanded, and
repeats for the instances their code demands. Generated code that calls library code needing
further instances asks for them with `Output.instance`, so they are generated in the same run
and checked in the same round. Bound instances go to lowering as `hir::Program::generated`,
mapping `(function, type arguments)` to the generated definition (`bind.rs`).

**Rounds** (`crates/veltc/src/generate/mod.rs`). A program is normally checked once, cold or
warm. The driver checks again only when generated code needs a check of its own
(`phase::needs_rewrite`: a closure, a nested declaration, `globalThis`, `Object.defineProperty`,
which the rewrites before a check would change; `Checked::Again`), or when sema runs without a
generator callback (`Checked::Demand`). Rounds are bounded (8), with an error naming the
instance that still needs code. Termination follows from the model being finite: every
instance's type arguments are model types, and sema rejects instantiation cycles that would
grow types without bound.

**Lowering** (`velt_vir`). An instance of a generated function is built from its generated
definition, looked up by canonical type arguments (`generated_instance` in
`crates/velt_vir/src/lower/program.rs`). Generated functions are ordinary functions: LLVM
inlines small ones, and the std helpers only generated code calls are inlined in debug builds
too (`velt_opt` `inline::std_forwarders`).

**Generated sources.** Generated modules are files in the session's source map, one per
declaring module and round (`<generated>/<module>/<round>.velt`), so diagnostics, panic
locations, stack traces and debugger lines point at real text. `VELT_EMIT_GENERATED=<dir>`
writes them to disk.

**The editor.** `velt lsp`'s own analysis (`velt_sema::ide::check_for_ide`) runs no generator.
After each analysis, a background job checks a copy of the program with generators
(`ProgramLoader::slow_diagnostics`, `crates/veltc/src/generate/editor.rs`), one job at a time,
and adds what the generators reported to the document's diagnostics if the document is still
at the version it checked. Requests never wait for it; with warm caches it costs one check.

## Runtime cost

Generated code calls the same std code a hand-written serializer would (the JSON reader and
writer, `StringBuilder`), so what matters is that the per-type part, written as Velt source,
compiles as well as code built directly in VIR. It does because:

- a generator writes each object's constant text as one literal (`w.append(",\"name\":")`),
  which the string builder appends inline ([string-building.md](string-building.md));
- key matching is ordinary integer switches and loads over the reader's borrowed key bytes,
  with the constants computed by the generator;
- SIMD scanning stays in the reader;
- generated functions are inlined like any other, and a gap found there (a bounds check not
  removed, a copy) is fixed in the compiler, where it speeds up every program.

Measured against per-type code built directly in VIR by the compiler (Apple M4, instructions
retired, `/usr/bin/time -l`, median of 5 or more, interleaved): `bench/json/stringify.velt`
release -3.7%; `bench/json/parse_typed.velt` release +0.9%, debug +0.6%;
`bench/sqlite/rows.velt` and `bench/db/` within ±1.2%; a comparison-heavy `deepEqual`/`toEqual`
program release -2.6%, debug +1.0%.

**Compile time.** A program that uses no generated function pays only a scan of its modules for
`generated` declarations. A program that does pays for checking the generated code with the
program (most of it in the ownership passes), for the demand walk over every body (about 2.5 ms
of a 60 ms check of a small program), and once per build cache for building the host (about
3 G instructions). Against compiler-built code: `velt check bench/json/stringify.velt` +3.8%
warm, +6.2% cold; `velt check bench/json/parse_typed.velt` +2.6% warm; a program of 40 object
types and 40 classes each compared with `deepEqual` and `toEqual` (about 240 generated
functions) +22% warm, +32% cold. Typical programs generate a few functions: the test suites of
`packages/sqlite` and `packages/http-server` check within 2.5% either way.

## Diagnostics

| Situation | Reported as |
|---|---|
| The generator reports a problem (`Output.error`) | The generator's message at the place it chose, usually the user's field or type, or at each call that reaches the instance: ``show cannot write a function (`(i32) => i32`)`` |
| Generated code does not type-check | `` the code generated for `broken<User>` by `./gen` does not compile (a bug in the generator) ``, at the call, with a note giving the error at its line in the generated file |
| Generated code builds a type it was not given | ``generated code cannot build `Hidden` without its constructor`` (as a note of the error above) |
| The generator throws | `` the generator of `crash` threw: <message> `` |
| The host does not build or fails | `` the generator `./impure` failed ``, with the host's errors as notes, such as `a source generator cannot reach the clock: it runs at compile time and gets everything from its request` |
| The host runs too long | `` the generator `…` did not finish within 30 s ``, naming the instance and `VELT_GENERATOR_TIMEOUT` |
| A host needs generated code | `generators need generated code to be built, in a cycle` |
| Rounds do not converge | `` `…` still needs generated code after 8 rounds of generating `` |

A throw at run time from code a std generator wrote for a std module is reported at the user's
call, as a throw from std is.

`velt build`, `velt run` and `velt check` take `--no-generators` for audits: the program is
checked once with no offered or generated code, and each instance that needs code is an error
naming its generator.

## Compatibility

Generators are invisible to TypeScript code: `JSON.stringify(user)`, `JSON.parse(text) as User`,
`assert.deepStrictEqual` and `expect(x).toEqual(y)` are written as in TypeScript and behave as
in Node. `declare generated` is Velt syntax used only by libraries.

### Security

Running a dependency's code while compiling is what Rust's build scripts and procedural macros,
.NET's generators and npm's install scripts do, and it is a supply-chain risk. Velt's generators
are narrower than all of those: they reach no I/O, clock or native code, enforced when the host
is compiled rather than by a runtime sandbox a bug could open; they run in a separate process;
and their output is source the compiler checks like any other, with no power beyond the access
rule, which covers only types the program hands them.

## Alternatives considered

**.NET source generators.** Roslyn runs generators on syntax trees and the semantic model and
adds source files to the compilation. Velt takes the essentials: generators written in the
language, source output checked with the program, partial classes as the access model. .NET
generators are driven by syntax (a `[JsonSerializable]` context lists the types), because C#
needs a declared context to know what to generate; Velt has whole-program demand, so no context
type is needed. Roslyn also runs generators in-process, trusting them; Velt runs them in a
separate, I/O-free process.

**Rust `#[derive]` and serde.** A derive runs on one type's tokens before type checking: it sees
syntax, not types, and must be written on every type. Velt takes the idea that per-type code is
ordinary monomorphized, inlined code, and a format-independent model (as serde's data model
serves every format); it does not take per-type annotations or syntax-only input.

**Zig `comptime`.** Ordinary code interpreted at compile time inside the type checker, with
`@typeInfo` and `inline for` over fields. It needs an interpreter of checked code in sema and
generics checked per instance; Velt checks a generic body once and monomorphizes in lowering,
so a compile-time evaluator would be a second implementation of the language. A generator
reuses the whole compiler, and its output can be read.

**Compile-time reflection** (C++26 `std::meta`, Swift macros, Scala 3 `Mirror`). Loops over
`fieldsOf<T>()` unrolled per instance need the same compile-time evaluation plus new constructs
(iteration over fields, member access by computed name) that TypeScript does not have.
Generators keep the language as it is.

**Runtime reflection** (Go's `encoding/json`, Jackson). Cheap to build, several times slower to
run, and it needs type metadata in every binary.

**Pluggable intrinsics** (a hook trait in the compiler). Generator authors would write Rust
against VIR, which is neither Velt nor a stable API.

**Generators in the compiler's process.** Faster to start, but a crash, an endless loop or the
runtime's process-wide state would be the compiler's.

**Generating at lowering time.** Lowering knows every concrete instance, but generated code must
be type-checked, ownership-checked and throws-inferred with its callers (a generated function's
`throws` set is part of its callers' types), which only sema does.

## Limitations

- A library checked on its own (`velt check` of a library without `main`) runs no generator for
  its generic code: the generator's errors appear only in a program that instantiates it.
- A generator gets no per-program options; its output depends only on its request.
- Subclass dispatch in generated code is an `instanceof` chain, most derived class first;
  generators cannot add methods to a class.
- A generator's own code cannot use generated functions (see "Cycles").
- A call whose argument modifies the `Output` it is also called on is rejected, as for any
  value (`out.line(… out.instance(…) …)`): generators compute names into locals first.

## Not yet implemented

- **Decorators as generator input.** `declare decorator function` exists (it declares
  compile-time decorators such as `@inline`), but the model does not yet carry decorator uses.
  Each `Type` and `Field` is to list its decorators, with their arguments as constant trees and
  spans, so a generator such as JSON's can honor `@json.name` and validate combinations with
  `Output.error` at the decorator. Decorators are data: nothing runs at run time. The
  decorator mechanism is designed in [#2203](https://github.com/velt-lang/velt/issues/2203).
- **`structuredClone` and `inspect`/`console.log` of objects as generators.** They are still
  per-type code built by the compiler (`crates/velt_vir/src/lower/glue/clone.rs`,
  `format*.rs`); `inspect` is tracked in [#1967](https://github.com/velt-lang/velt/issues/1967).
- **`velt build --emit-generated <dir>`**, the CLI form of `VELT_EMIT_GENERATED`, and an editor
  command that opens the generated source of a call.
- **An OS sandbox for the host** (seccomp on Linux, a sandbox profile on macOS, a restricted job
  on Windows), as defence in depth beside the compile-time purity check.
