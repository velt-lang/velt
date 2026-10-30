# Design: source generators (compile-time code written in Velt, for any library)

Status: in progress on branch `source-generators`: steps 1–6 of "Order of work" are built
("As built so far" at the end). Owners: `velt_syntax` (the declaration), `velt_sema` (demand, the type model, checking
generated code, the access rule), `velt_vir` (instantiating a generated function), `veltc` (the
generation loop, the generator host, caching), std (`velt/generate`, and each generator).
Related: [the JSON engine](json-engine.md) (whose steps 9–10, decorators, become generator input),
[standard library checked on demand](lazy-std.md), [hot reload](hot-reload.md) (the JIT host
reused here), [compiling std's Velt code once](std-object.md) (the build cache).

## Problem

`JSON.parse<T>` and `JSON.stringify` compile to one decoder or writer per type. The code that
builds them is Rust inside the compiler: `crates/velt_sema/src/json/` (about 1,150 lines: which
types have a JSON form, propagated through generic code) and `crates/velt_vir/src/lower/json/`
(about 4,350 lines of VIR built by hand). That code has powers no library has:

- it writes private fields and builds class instances without running a constructor
  (`alloc_object_raw` in `object.rs`);
- it dispatches over every subclass in the program (`json_descendants`), and over typed proxies
  and `any` values through runtime tags;
- sema infers a "has a JSON form" requirement on type parameters and reports a missing one at
  the concrete call site (`json::check_json_types`);
- lowering calls std's reader and writer by hard-coded qualified names
  (`"std/encoding/json/reader::Reader"`, `PRELUDE_JSON_VALUE`), and lazy std has a special case
  so that those modules are checked only when an intrinsic is instantiated (`JSON_GLUE`,
  `body/lazy_std_json.rs`).

The same std-only scheme generates, per type, structural equality (`Intrinsic::Eq`, `DeepEq`),
`structuredClone` (`Clone`), `inspect` and `console.log` of objects (`glue/format*.rs`), database
rows (`__intrinsic_json_parse_cursor`, `ReadRows`, `ReadRow`) and parameter binding
(`__intrinsic_db_bind`, `json/db_bind.rs`): about 6,500 lines of glue in `lower/glue/` besides
JSON.

So a third-party JSON, MessagePack, CBOR, ORM, validation or deep-equal library cannot match std:
it has no way to see a type's fields at compile time, and no way to produce code per type. The
best it can do is ask its users to write codecs by hand. And every new format std wants needs
more Rust in the compiler.

The decision (the owner's, on this branch's task): do it the way .NET does with source
generators. Per-type code generation becomes a library feature, written in Velt, and std's JSON
is the first library to use it.

## Goals

1. A library written in Velt can generate code per type at compile time, with what std's JSON
   needs: the fields (optional, private, readonly), unions and their discriminants, classes and
   their subclasses, enums, arrays, maps, records, tuples, and decorators.
2. No annotations for the common case: `JSON.stringify(user)` and `JSON.parse(text) as User`
   keep working as in TypeScript. Generation is driven by use.
3. One documented rule replaces the privileges: code generated *for* a type has that type's
   access. Std's generators use nothing a package cannot.
4. Generated code is as fast as today's glue (performance is a published claim of the JSON
   engine), and compile time does not regress for programs that use no generator.
5. Generated code is ordinary Velt: type-checked, inspectable, and debuggable.

Non-goals: macros that rewrite user syntax; generators that add declarations users call by name
(.NET's `partial` method stubs, Rust's attribute macros); runtime reflection.

## Design

### 1. A generated function

A library declares a generic function whose body is generated:

```ts ignore
// velt/encoding/json/index.vlt
import { StringBuilder } from "velt/string_builder";
import { writeString, writeF64 } from "./writer";

/** Appends the JSON text of `value` (one function per type, generated). */
export declare generated("./generate/write") function write<T>(out: StringBuilder, value: T): void;
```

- `declare generated("<specifier>") function` follows `declare system function`: a declaration
  without a body, here generic. The specifier names the **generator module**, resolved like an
  import of the declaring module. It is not loaded into the program; it is loaded into the
  generator's own build (§5).
- The signature is the contract. Callers are checked against it like any generic function.
  Its return type must be written.
- When the program instantiates the function with concrete types (`write<User>`, directly or
  through generic code: `JSON.stringify<T>` calls `write<T>`), the compiler asks the generator
  for the body of that instance (§3), checks it, and uses it for that instance.
- The function stays an ordinary function for everything else: a value of it, a call through a
  type parameter, a `throws` clause.

The public API stays as it is. `JSON.stringify<T>(value: T): string` is a written function in
std that makes a builder, calls `write<T>` and returns the text. Users never see the generated
function.

### 2. Generators are Velt functions

The generator module exports a function with the same name as the declaration. It is given a
**request** and writes to an **output**:

```ts ignore
// velt/encoding/json/generate/write.vlt
import { Request, Output, Type } from "velt/generate";
import { jsonQuote } from "../quote";   // the key as JSON text, computed at compile time

export function write(req: Request, out: Output): void {
  const t = req.model.type(req.args[0]);
  switch (t.kind) {
    case "number":
      out.body(`writeF64(out, ${req.params[1]} as f64);`);
      break;
    case "object":
      out.line(`out.append("{");`);
      let first = true;
      for (const f of t.fields) {
        if (f.isEsPrivate) continue;
        const key = (first ? "" : ",") + jsonQuote(f.name) + ":";   // literal key text
        out.line(`out.append(${out.literal(key)});`);
        out.line(`${out.instance("write", [f.type])}(out, value.${f.name});`);
        first = false;
      }
      out.line(`out.append("}");`);
      break;
    // …
  }
}
```

- `Request` carries: the function's name, the instance's type arguments (as ids into the model),
  the names of the parameters, and the **type model** (§4).
- `Output.body(code)` / `line(code)` write the body, `literal(s)` spells a string as a Velt
  string literal; `helper(code)` adds a top-level function or
  constant next to it (named by `out.fresh("name")`, so two instances never clash).
- `Output.instance(fn, args)` returns the name of another instance to call, of this or another
  generated function of the same generator module, and queues it. That is how a generator
  recurses into field types without a round trip through the compiler (§6).
- `Output.typeName(id)` returns a type expression for a model type, usable in the generated code
  even where the type is not exported or has no name (`{ a: number }[]`, a local class).
- `Output.error(message, at)` / `warning` report a diagnostic at a span the model holds (a field,
  a type's declaration, the call site), with notes. This is how a generator says "`User.save` is a
  function; functions have no JSON form" at the user's code.
- The compiler writes the function's header itself (name, the signature with the type arguments
  substituted), so the generated code cannot get the signature wrong; the generator writes the
  body.

### 3. Demand: which instances to generate

Sema already carries requirements of type parameters through generic code to concrete call
sites, with dynamic dispatch and closures (`inst_sites`, `dispatch::Rounds`; used by
`json::check_json_types`, `record_keys`, `type_level::instances::reach`). A new pass,
`generated::demand`, runs the same propagation for calls of generated functions: a call
`write<T>` in `stringify<T>` makes `stringify` need `write<T>`; a call `JSON.stringify<User>`
then needs `write<User>`, which is concrete: an instance to generate. The concrete instances,
with the span of the call that first reached each (for diagnostics), are the **demand**.

Why demand-driven, and not "generate for every type a program declares" (STJ's
`[JsonSerializable(typeof(T))]` contexts, Rust's `#[derive]`): TypeScript code doesn't annotate
types for JSON, and Velt promises that `JSON.parse(text) as User` just works. Generating for
every type would cost compile time for types never serialized and need either annotations or a
generator run per declared type. Demand is what today's intrinsics already compute.

### 4. The type model

`velt/generate` defines the model as Velt classes. It is a read-only description of the types a
request can reach, computed by sema and sent to the generator. Types refer to each other by
index (`TypeId`, a `u32`), so recursive types are a graph, not infinite trees.

```ts ignore
export type TypeId = u32;
export type Kind =
  | "number" | "bigint" | "boolean" | "string" | "null" | "undefined" | "void" | "never"
  | "literal" | "enum" | "array" | "tuple" | "option" | "union"
  | "object" | "class" | "struct" | "interface"
  | "map" | "set" | "record" | "date" | "function" | "any" | "unknown" | "opaque";

export class Type {
  id: TypeId;
  kind: Kind;
  /** As the user would write it (`User`, `Map<string, User[]>`): for messages. */
  display: string;
  /** Qualified name of a declared type (`velt/encoding/json::JsonValue`), "" otherwise. */
  qualifiedName: string;
  /** "f64", "i32", "u8", … for numbers. */
  repr: string;
  /** Element, option payload, map key and value, …: in a fixed order per kind. */
  args: TypeId[];
  /** Union arms, in declaration order. */
  arms: TypeId[];
  /** The discriminant of a discriminated union: the field name, and per arm its literal value. */
  discriminant: Discriminant | null;
  /** Fields of objects, classes and structs, own and inherited, in JavaScript's order. */
  fields: Field[];
  /** For a class: its base class, and every class in the program that extends it (the program
   *  is compiled as a whole, so the list is closed). */
  base: TypeId | null;
  subclasses: TypeId[];
  /** Methods by name that a generator commonly looks for (`toJSON`, `__inspect`): name, number of
   *  parameters, return type. Not bodies. */
  methods: Method[];
  /** The constructor's visibility, and whether the class may be built without it (§7). */
  constructible: "public" | "protected" | "private" | "none";
  /** Enum members: name and value (number or string). Literal value of a literal type. */
  members: EnumMember[];
  literal: string;
  /** Decorators on the declaration (§8). */
  decorators: Decorator[];
  /** Where it is declared, for diagnostics. */
  at: SpanRef;
}

export class Field {
  name: string;
  type: TypeId;
  optional: boolean;      // `x?: T`
  readonly: boolean;
  visibility: "public" | "protected" | "private";
  isEsPrivate: boolean;   // `#x`
  isGetter: boolean;      // an accessor of an object literal type
  hasDefault: boolean;    // a field initializer
  decorators: Decorator[];
  at: SpanRef;
}
```

- The model is **closed**: it holds every type reachable from the request's type arguments
  through fields, arms, elements, bases and subclasses. A generator never needs another round
  trip to learn about a type.
- Kinds hide what a library has no business depending on: layouts, boxing, whether a class is
  counted, VIR types. Std types with no structure the generator may see (a `Mutex`, a socket)
  are `"opaque"` with their `qualifiedName`, so a generator can still special-case them
  (`JsonValue`), or reject them with a message.
- The model is versioned with `velt/generate`: fields are only added. It is a public API, held to
  the same compatibility rules as the rest of std.
- It is also how a generator learns about **decorators**: data, never code (§8).

### 5. Generators are built and run by the compiler

**Packaging.** A generator is a module in a package (or in std) like any other; nothing in the
manifest declares it. The declaration `declare generated("./generate/write")` is what makes it a
generator. Its dependencies are the package's.

**Its own build.** The compiler builds the generator module, its imports and std as a separate
program (the *generator build*), whose `main` is std's `velt/generate/host` loop over the
generators the program needs. The generator build is ordinary Velt, compiled by the same
compiler, so generator authors get the whole language and std, with three restrictions that make
generators pure functions of their input (needed for caching, §9, and for security, §11):

- no I/O: the files, the network, the environment, child processes, standard input;
- no code outside Velt: no `declare function` externs of its own, no native packages;
- no clock and no randomness.

These are checked when the generator build is compiled, from what its generators can reach (the
runtime functions and externs reachable from the functions the host registers, as `velt test`
decides whether a test reaches outside, `driver/outside.rs`), with the host's own plumbing in
`velt/generate` left out: "a source generator cannot reach the clock: it runs at compile time and
gets everything from its request". Checking reachability rather than imports catches a std module
that reads the clock as surely as an import of `velt/fs`, and needs no list of modules to keep up
to date. (An earlier draft gave the clock and randomness fixed values; rejecting them is simpler
and keeps generators obviously deterministic.)

**Running it.** The generator build is compiled once into an executable kept in the build cache
(with a build stamp of its own, so a host whose generators did not change is neither compiled
nor linked again) and run as a child process; without a build cache it is JIT-compiled and run
by `velt __generate-host`, as `velt dev --host` runs a program ([hot-reload.md](hot-reload.md)).
The compiler writes requests to its stdin and reads results from its stdout. One host is built
per directory its generator modules are imported from: its root sits beside the declaring module
and imports the generator module by the specifier the declaration wrote, so a relative path, a
package's module and a std module (imported as `velt/…`) resolve as imports of the declaring
module do, with the program's packages. A child process rather than the compiler's own:

- a crash, a stack overflow, an uncaught `throw`, an endless loop (a timeout) or
  `process.exit` ends the host, not the compiler, which reports it as a diagnostic at the
  generator's declaration, with the host's stderr;
- the runtime keeps process-wide state (the allocator, stdout buffers, the panic hook, the tokio
  runtime) that the compiler must not share;
- the process can be sandboxed by the OS later (§11), and runs in parallel with the compiler.

In `velt dev` the host stays up between reloads, so its JIT cost is paid once per session.

**Bootstrapping.** A generator build that itself instantiates generated functions (a generator
that calls `JSON.stringify`, `console.log` of an object once `inspect` is a generator) needs
their generators first: the compiler builds those generator builds first. A generator that needs
itself, directly or through another, is a compile error that lists the cycle. Std's generators
avoid generated functions in their own code (they build strings with `StringBuilder` and read the
model by its methods), so std has no cycle.

### 6. Rounds and the fixed point

Generated code is Velt, so it must be type-checked like the rest of the program, and it can
demand more instances. Sema checks a program as a whole, with passes after body checking that
need every body (ownership, throws inference, the instance propagation itself). And demand
depends on the whole program: an instance demanded inside generic code is known only once its
callers are. So generation runs between whole-program checks, in **rounds**, as sema already
re-checks a program when the throws sets of function types grow (`check_timed`'s rounds):

1. The driver loads the program and checks it. Generated functions have no bodies yet; calls to
   them are checked against their signatures. Sema returns the demand instead of a program when
   an instance has no generated body.
2. The driver groups the demand by generator, runs the generators (each request queues the
   further instances it asks for with `Output.instance`, so a nested type is generated in the
   same host session), and gets one generated module per declaring module.
3. The driver checks the program again with the generated modules added. Sema binds each
   instance to its generated function (`hir::Program::generated`). Demand that is still missing
   (generated code called another library's generated function without `Output.instance`) starts
   another round.
4. When no demand is missing, lowering builds the program; an instance of a generated function
   is built from its generated definition.

**Termination.** Every instance's type arguments are types of the model, and the model is
finite: sema already rejects instantiation cycles that would grow types without bound
(`instantiation_cycles`). So the set of instances is finite and the queue drains. The number of
rounds is bounded anyway (8, like `FN_SITE_ROUNDS`), with an error that names the instances
still missing after the last one.

**Cost.** A program that uses no generated function pays nothing: no demand, one check, as
today. A program that does pays a second check, plus the generators. §9 removes both in the
common case.

### 7. Access: code generated for a type has that type's access

One rule replaces the special cases. In a generated module, for every type in the request's
model:

- its `private` fields and methods may be read and written, and its private constructor called
  (as code in the class could);
- a class may be built **without running its constructor**, with the sanctioned form
  `__construct<T>({ …fields })`. Every field without an initializer must be given; a field with
  an initializer that is left out gets the initializer's value, an optional one is absent.
  `__construct` means nothing outside generated code, and in it only for the types of its
  requests.

This is what .NET's partial classes give a generator, and what today's JSON glue does. A library
does not get access to a type the program never hands it: the model of a request holds only
types reachable from the instance's type arguments. A user who wants a type kept out of a
generator's reach declares the field `#private`, or the class with a private constructor; the
model reports both (`isEsPrivate`, `constructible`), and std's JSON generator keeps today's
rules (ES private fields are skipped, a class with a private constructor is written but not
decoded).

Generated modules are never std modules (`SourceModule::is_std` is false even for std's
generators).

The access is granted per generated module: its code has the access of every declared class and
struct that its requests' models reach (`GenModule::access`). ES private fields (`#x`) are not
included: as in JavaScript, nothing outside the class sees them, and no std generator needs them
(Node's `JSON.stringify`, `util.inspect`, `assert.deepStrictEqual` and `structuredClone` all skip
them). The sanctioned form is `__construct<T>({ … })`, a name only generated code resolves (as
`__qualified` is), so that generated code needs no import. They cannot call compiler intrinsics. What std's generated code needs is a public,
documented API that a package can call too: the JSON reader and writer (json-engine.md step 12
makes them public anyway), and a small `velt/generate/runtime` for what only the compiler knows
at run time (a typed proxy's tag, an object's class for subclass dispatch, the cycle guard of
the writer). That is the test of "std stops being special": std's generators compile with the
same powers as any package's.

**Scope.** A generated module is checked in the scope of the module that declares the generated
function: its imports and top-level declarations, exported or not, are visible, as if the
generated code were appended to it. It cannot import: what its code needs, the declaring module
imports (a generated module is added after the program is loaded, and the loader is not run
again for it). The names returned by `Output.typeName` are bound by the compiler in that scope,
so no import of the user's types is needed.

### 8. Decorators are generator input

The JSON engine's decorators (json-engine.md steps 9–10) need TypeScript's syntax (`@json.name`)
in the parser and sema; that design stands with one change: a decorator is declared by a library
next to its generator, not by a `@decorator` tag that only std may use.

```ts ignore
// velt/encoding/json/index.vlt
export declare decorator function name(key: string): FieldDecorator;
```

`declare decorator function` declares a decorator: a name, and the constant arguments it takes
(literals, arrays and object literals of them, enum members, type names). Sema checks uses of it
against the declaration and records them, with their arguments as constants, in the model
(`Type.decorators`, `Field.decorators`: name, arguments as a constant tree, span). Nothing runs
at run time and the decorated program pays nothing. A generator validates what the declaration
cannot ("two fields with the same JSON name after `@json.naming`") and reports it with
`Output.error` at the decorator's span.

### 9. Caching

A generator is a pure function of its request (§5). So its output can be cached by content, in
the build cache (`velt_build_cache`, kind `generated`):

- key: the toolchain version, the content hash of the generator build (the generator module and
  everything it imports), and the request: function name, type arguments and the part of the
  model reachable from them, serialized in a canonical order without file positions (spans are
  sent separately and mapped back, so moving a type in its file does not miss the cache);
- value: the generated body, its helpers, and the further instances it asked for (each looked up
  the same way).

Second, the driver remembers per program (keyed by the root module's path and the build
options) which generated modules the last build used, and offers them to the first check. When
the first check's demand is covered by them, there is one check, as for a program without
generators. A stale offered module (a renamed type) is dropped, not reported: the demand then
runs a second round as usual.

So the steady state (an edit that changes no serialized type) costs one check and no generator
run. An edit to a serialized type costs one generator request for that type, from the cache for
the others.

The compiled generator build is cached too: as an executable in the build cache (`put_file`),
once the JIT host's cost shows in measurements (each generator build is compiled once per
toolchain and generator version, and linked once).

### 10. Diagnostics and inspecting generated code

Generated modules are real source files in the session's source map, with paths such as
`<generated>/velt/encoding/json/index/write.vlt`, so every diagnostic, panic location, stack
trace and debugger line in generated code points at real text.

- An error the generator reports (`Output.error`) is the user's error: it is shown at the span
  the generator chose, usually the user's field or type.
- An error in generated code (it does not type-check) is a bug in the generator. It is reported at
  the call site that demanded the instance, as `the code generated for write<User> by
  velt/encoding/json does not compile`, with a note giving the error at its line in the
  generated file and a note naming the generator module.
- `velt build --emit-generated <dir>` writes every generated module to disk; the editor's "Show
  generated source" (an LSP command on a call of a generated function) opens the module the
  instance came from. The editor gets generated modules from the cache, so analysis never waits
  for a generator run (a missing one shows the call as checked against the signature only).

### 11. Security: running package code at compile time

Running a dependency's code while compiling is what Rust's build scripts and procedural macros,
.NET's analyzers and generators, and npm's install scripts do; it is a known supply-chain risk.
Velt's generators are narrower than all of those:

- they get no I/O: no file, network, process, environment or native code (§5). This is enforced
  when the generator build is compiled, from the module graph, not by a runtime sandbox that a
  bug could open;
- they run in a separate process, which a later step confines with the OS's sandbox (seccomp on
  Linux, a sandbox profile on macOS, a job object with restricted tokens on Windows) for defence
  in depth;
- their output is source code that the compiler checks like any other: it cannot do anything a
  library's ordinary code could not, except the access of §7, which is limited to types the
  program hands it;
- `velt build --no-generators` makes demand an error that lists the generators that would run,
  for audits.

### 12. Performance: generated Velt must compile to code as good as today's glue

The generated code calls the same std code today's glue calls (the Velt reader and writer of
json-engine.md step 0), so what changes is how the per-type part is written: Velt source instead
of VIR built in Rust. What keeps it as fast:

- **Literal key text.** A generator writes each object's constant parts as one string literal
  (`out.append(",\"name\":")`), which the string builder appends inline (string-building.md).
- **Key matching.** json-engine.md step 1's length switch and word compares are written as Velt
  over the reader's borrowed key bytes (`key.length == 4 && key.u32At(0) == 0x656d616e`); the
  generator computes the constants. A switch on an integer and loads from a byte view are
  ordinary code that both backends compile without calls.
- **SIMD scanning** stays in the reader, which generated code calls.
- **Inlining.** Generated functions are ordinary functions: LLVM inlines small ones into their
  callers, as today's glue is inlined. A gap found here (a bounds check not removed, a copy of a
  builder, an inlining threshold) is fixed in the compiler, where it speeds up every program,
  as json-engine.md's rule asks.

### Acceptance criteria

The port of each operation is merged only when, against `main` at its merge base:

- **run time:** instructions retired (cachegrind on Linux, `/usr/bin/time -l` on macOS), at most
  3% worse per row, for `bench/json/run.sh` (`stringify`, `parse_typed`, `parse_union`), the JSON
  corpora (`bench/json/corpora/run.sh`), `bench/typical` and the web server benchmark; debug
  builds measured too (generated code is compiled into each program, as the engine is);
- **compile time:** hello world unchanged (it has no demand; measured by instructions retired of
  `velt build`); a JSON-heavy program (`bench/json/stringify.vlt`, `parse_union.vlt`) at most 10%
  slower to build cold and at most 3% warm (the cache hit path), with `--timings` showing the
  rounds and the generator host separately;
- **behaviour:** every JSON golden and the Node-differential fuzz (`bench/json/fuzz/run.sh`) pass
  unchanged; error messages and paths stay the same.

## Migration

| Step | What | Removes |
|---|---|---|
| 1 | `declare generated`, demand, rounds, the host, the model, `Output`, a test generator; goldens | — |
| 2 | Caching of generator output; the offered modules; `--emit-generated`; `--timings` rows | — |
| 3 | The access rule and `construct<T>` | — |
| 4 | `JSON.stringify` as a generator in `velt/encoding/json` (with `velt/generate/runtime` for proxies, `any` and subclass dispatch) | `lower/json/write.rs`, `dynamic.rs`, `literal.rs`, the stringify half of `sema/json`, `JSON_GLUE`'s writer |
| 5 | `JSON.parse<T>`, then `JSON.parse` of rows and cursors (`velt/db`) and `__intrinsic_db_bind` | the rest of `lower/json/`, `sema/json/`, `JSON_GLUE`, `lazy_std_json.rs`, the intrinsics |
| 6 | Decorators (json-engine.md steps 9–10, declared with `declare decorator function`) | — |
| 7 | `deepEqual`/`toEqual` (`Eq`, `DeepEq`), `structuredClone` (`Clone`), `inspect` and `console.log` of objects | `glue/eq*.rs`, `clone.rs`, `format*.rs` |

What stays in the compiler: the operators of the language on its built-in types. `===`,
`Object.is`, SameValueZero for `Map` keys (`Same`, `SameValue`, `KeyEq`, `IncludesEq`) and
hashing (`Hash`) compare objects by identity and never walk a user type's fields; they are
`===`, not a library. The line: an operation that walks the structure of user types is a
generator; an operator of the language is the compiler's.

Each step keeps its golden tests unchanged and meets the acceptance criteria above. The Rust it
replaces is deleted in the same pull request.

## Alternatives considered

**.NET source generators** (the model). Roslyn runs generators in the compiler process, on the
syntax trees and the semantic model, and adds source files to the compilation; incremental
generators cache per pipeline step. Taken: generators are code in the language, output is
source, type-checked with the program, inspectable; partial classes as the access model;
attributes as input. Not taken: .NET generators are driven by syntax (a generator finds
`[JsonSerializable]` attributes and generates for the types they list), because C# needs a
declared context to know what to generate. Velt has whole-program demand, so no context type is
needed, and `JSON.parse(text) as User` stays TypeScript. And Roslyn runs generators in-process,
trusting them; Velt runs them in a separate, I/O-free process.

**Rust: `#[derive]` procedural macros and serde.** A derive runs on one type's tokens, before
type checking: it sees syntax, not types (a field of type `Id` is a path, it cannot know `Id` is
a `u64` or a union), and it must be written on every type. serde puts a generic data model
(`Serializer` with 29 methods) between types and formats, which costs nothing in Rust thanks to
monomorphization and inlining, and lets one derive serve every format. Taken: the idea that the
per-type code is ordinary code monomorphized and inlined, and the format-independent model
(§4 serves every format the way serde's data model does). Not taken: annotations on each type
(TypeScript code has none), syntax-only input, and the trait boilerplate.

**Zig `comptime`.** Zig interprets ordinary code at compile time inside the type checker
(`@typeInfo` gives a type's fields; `inline for` over them generates code). It is the most
elegant: no separate generator language or process, no text. Not taken now: it requires an
interpreter of checked code inside sema and generics checked per instance; Velt checks a generic
body once and monomorphizes in lowering, and its compile-time evaluator would be a second
implementation of the language next to the backends. A source generator reuses the whole
compiler (the generator is compiled and run like a program), and its output can be read. If Velt
grows `comptime` later, a generator can become a comptime function behind the same
`declare generated` declaration.

**Compile-time reflection** (C++26 `^^T` and `std::meta`, Swift macros, Scala 3 `inline` and
`Mirror`). A library calls `fieldsOf<T>()` in a generic function, and the compiler unrolls loops
over the result per instance. This keeps generated code invisible (no text), and needs the
language to evaluate the reflection queries and unroll at compile time in the checker: the same
interpreter Zig needs, plus new constructs (`for` over fields, member access by computed name)
that TypeScript does not have. Generators keep the language as it is; reflection would extend
it.

**Runtime reflection** (Go's `encoding/json`, Java's Jackson). Cheap to build and slow to run
(Go's reflection-based JSON is several times slower than generated code), and it needs type
metadata in every binary. It contradicts the speed claim.

**Keep the intrinsics, make them pluggable** (a hook trait in the Rust compiler). It would still
need generator authors to write Rust against compiler internals (VIR), which is neither
Velt-first nor a stable API.

**Generators run in-process by the JIT.** Fastest to start (no process), and possible: `velt`
links the runtime. Rejected for now for the isolation reasons of §5; measurements decide whether
an in-process mode is worth it later (the protocol is the same).

**Generate at lowering time instead of rounds of checking.** Lowering already knows every
concrete instance, so the generator could run there with no extra check. But generated code
must be type-checked, ownership-checked and throws-inferred with its callers (a generated
function's `throws` set is part of its callers' types), which only sema does, and sema cannot be
resumed after lowering started. Rounds use the structure sema already has.

## Open questions

1. **Generating methods.** Subclass dispatch (writing a class value as its dynamic class) is an
   `instanceof` chain in generated code (most-derived first), where today's glue switches on the
   class. If measurements show a cost for deep hierarchies, generators could add a method to a
   class (`write` as a hidden virtual method, dispatched through the vtable). That is a bigger
   power (it changes layouts) and is left out of v1.
2. **Instances inside generic code that the program never instantiates concretely** (a library
   checked on its own, `velt check` of a library without `main`): no demand, so the generator's
   errors only appear in a program that uses the library. Should `velt check` of a library run the
   generator for its own concrete uses only (the proposal), or not at all?
3. **Per-program options** (STJ's `JsonSerializerOptions`): a generator gets no settings from the
   program today. A manifest section per generator would make a dependency's output depend on the
   program that imports it (json-engine.md's open question 2); the proposal is none in v1.
4. **The model's span references** across rounds and the cache: spans are mapped by
   (file, declaration path) so a cached result stays valid when a type moves within its file. Is
   that enough for the editor, which changes files without saving?
5. **Debug builds.** The JSON engine's gate measures debug speed because std's JSON is compiled
   into each program; generated code is too. If debug speed regresses, should generated modules
   be optimized in debug builds (as std's cached functions are, std-object.md)?

## Order of work

Each row is a commit or pull request that builds, passes its crates' tests and clippy, and adds
goldens:

1. Parser and AST: `declare generated("…") function` (`FnSig::generator`, a contract change:
   `ast.rs`, additive).
2. Sema: generated functions checked by signature; `generated::demand`; the type model built from
   the checked program; generated modules checked in the declaring module's scope with the bound
   type names; `hir::Program::generated` (additive) mapping `(function, type arguments)` to the
   generated definition.
3. Lowering: an instance of a generated function is built from its generated definition.
4. `velt/generate` (the model, `Request`, `Output`, the host loop) and `velt generate --host`;
   the driver's rounds; a test generator and goldens (a generated `describe<T>()` that lists a
   type's fields), including errors reported by the generator and errors in generated code.
5. Caching and the offered modules (§9); compile-time measurements.
6. The access rule (§7).
7. `JSON.stringify` (§Migration step 4) with benchmarks before and after; then `JSON.parse`.

## As built so far

- `declare generated("<spec>") function` (`FnSig::generator`), the demand pass
  (`velt_sema/src/generated/demand.rs`), the model (`model.rs`), spellings with
  `__qualified<"module", "Name">` (`spell.rs`), binding (`bind.rs`, `hir::Program::generated`),
  the redirect in lowering (`velt_vir` `generated_instance`), the driver's rounds and the host
  (`veltc/src/generate/`), `velt/generate` (`std/generate/index.vlt`).
- Generated code is a module of its own per declaring module and round
  (`<generated>/<module>/<round>`, never std), checked in the declaring module's scope
  (`generated/access.rs` `inherit_scopes`); `VELT_EMIT_GENERATED=<dir>` writes the modules out
  until `--emit-generated` exists.
- The access rule (`generated/access.rs`): `private` members and `__construct<T>` for the
  declared types its requests reach.
- A generated function and its generated bodies share pass modes: they are joined like a vtable
  slot's (`ownership/mutation.rs`), so callers checked against the declaration call the bodies
  with the same ABI. A body throws only what the declaration's `throws` lists.
- Caching (§9): the output of each host run by its input (`generate/cache.rs`), the offered
  modules with a fingerprint per instance (`Model::fingerprint`: the model closure of its type
  arguments renumbered from them, without positions, and the function's signature), the host
  executable in the build cache (`generate/host.rs`), and the generators' files in the
  program's build stamp.
- The no-I/O rule (§5): `driver::check_generator_host`, from what the generators reach.
- Generator modules resolved as imports of the declaring module: relative, package, std.

Not yet: decorators (§8), `--emit-generated` and `--no-generators`, per-request output caching
(today one entry per host run: an edit to one serialized type runs the generator for every
instance of the round; the offered modules make that rare).

### Measured (Apple M4, a shared machine)

- **No generators:** no demand, one check; the only new work is a scan of the modules' items
  for `generated` declarations.
- **With generators, no cache:** `bench/json/generated/stringify.vlt` builds in 1.48 G
  instructions against 1.07 G for `bench/json/stringify.vlt` (+39%): a second check (31 ms) and
  the host (116–400 ms wall, almost all of it the host compiling `velt/generate` and the
  generators and JIT-compiling them). Far over the 10% criterion: caching the output (§9) and the
  compiled host are required before any std operation moves onto generators.
- **With generators, cached** (the steady state: the last build's modules offered): 679 M
  instructions against 629 M (+8%; one check, the offered module parsed and checked with the
  program, the offer and the generators' files read from the cache).
- **Run time:** the prototype `JSON.stringify` generator (`bench/json/generated`, public APIs
  only) writes byte-identical text and costs about 1.57 G instructions against the glue's 1.01 G
  for the same work (+55%; `compare.sh`, release runtime). A literal append (`Writer.raw`,
  `StringBuilder.append`) is a runtime call of about 80 instructions, where the glue writes
  literal text inline (string-building.md step 1). Parity needs string-building.md step 2
  (appends of `StringBuilder` compiled to the same inline path) in the compiler.
- **Ergonomics:** `out.line(… out.instance(…) …)` is rejected by Velt's rule that one call
  cannot modify a value and read it in another argument; generators compute names into locals
  first. `Output` could keep its state outside the object the generator holds to lift that.
