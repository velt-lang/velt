# Design: TSX for server-side rendering

Status: implemented, as described in [the reference](../../reference/tsx.md) and
[the provider contract](../contracts/jsx.md); open: elements in the props of a component that
takes ownership of them (after semantics stage 2), class components and `ref`. This note keeps
the original design and its reasons; where it differs from the reference, the reference is
right.

TSX in Velt is for **server-side rendering** first: Velt renders HTML on the server and a client
framework takes over (resumable SSR, streaming, islands). The first intended consumer is
[sigx](https://sigx.dev). The design must be **generic**: any TSX provider (sigx, a Preact- or
Hono-style renderer, a plain HTML builder) plugs in the same way, like TypeScript's
`jsxImportSource`.

Principles ([TypeScript alignment](ts-alignment.md)): look exactly like TSX where TypeScript
already solved it; add nothing JavaScript-specific that causes bugs; keep Rust-level speed
(server-side rendering throughput is the point).

## Syntax (identical to TSX)

- Elements `<div class="a">{x}</div>`, self-closing `<br />`, fragments `<>…</>`, components
  `<Card title={t}>…</Card>` (a capitalized or dotted name is a component), spread props
  `{...p}`, `key`, string and expression attributes, children text with JSX whitespace rules,
  `{/* comments */}`.
- Type arguments on an opening tag, as in TSX: `<List<number> items={xs} />`, also after a
  member name (`<ui.List<number> …>`); the closing tag takes none (`</List>`). Inside a tag a
  `<` that does not follow `=` opens the type arguments, which are lexed as code up to the
  matching `>` (an element as an attribute value, `icon=<Star />`, still follows `=`).
- Allowed in every `.vlt` file: Velt has no `<T>expr` casts (only `as`), so TypeScript's `.ts`
  / `.tsx` split isn't needed. `velt fmt` formats JSX like Prettier.
- **The parser decides where an element starts**, not the lexer: in code the lexer always
  emits `<` as a plain `Lt`. Where the parser expects an expression and finds `<` directly
  followed by a name or `>`, it first tries a generic arrow (type parameters, a parameter list
  and `=>`, or a head starting `<T,`, `<T extends` or `<T =`); otherwise it has the lexer re-lex
  from that `<` in JSX mode (the parser pulls tokens on demand, so only its short lookahead is
  dropped). So generic arrows are written as in `.ts` files, `<T>(x: T): T => x` (`<T,>` works
  too, and `velt fmt` prints `<T>`), and `<` after a keyword used as a name is never JSX:
  `v.as<User>()`, `v?.as<User>()`, `class C { as<T>(): T {…} }`, `x as T`. The one program
  this reads differently from `.tsx` is an element named like a type parameter whose text is an
  arrow signature, `<T>(x: T): T => x</T>`. A `<` that fails both reads as a broken generic
  arrow gets a hint on the unclosed-element error.

## Semantics: TypeScript's automatic runtime, with an SSR precompile mode

Configuration, like TypeScript (`jsx: "react-jsx"` plus `jsxImportSource`):

- `jsx: { importSource: "sigx" }` in `package.vlt` (a package or `velt:jsx`), or per file
  `/** @jsxImportSource sigx */` among the comments before the first token. The default is
  `velt:jsx`. Velt reads the pragma from a line comment (`// @jsxImportSource sigx`) too, but
  `tsc` reads it only from a block comment, so files shared with a client use the block form
  (`jsx-pragma-comment` below).
- The provider module `<source>/jsx-runtime` exports the factory functions and the `JSX`
  namespace (types). Nothing is hard-wired to one framework.

Two lowerings, chosen by what the provider exports:

1. **Generic (always available)**: TypeScript's automatic runtime. `<div a={x}>{y}</div>`
   becomes `jsx("div", { a: x, children: y })`, with `jsxs` for static child lists and
   `Fragment`. Props are anonymous object types (fixed layout, no hashing); `key` is passed
   separately. Providers that build a tree (a virtual DOM, or a structure a client hydrates)
   use this.
2. **SSR precompile (when the provider exports `jsxTemplate`)**: the shape of Deno's precompile
   transform (supported by Hono and Preact). Every static part of an element tree becomes one
   constant string and dynamic parts become slots:
   `jsxTemplate(["<div class=\"card\"><h1>", "</h1>", "</div>"], jsxEscape(title), body)`,
   plus `jsxAttr(name, value)` for dynamic attributes and `jsxEscape` for text. Components
   inside a template are still called through `jsx(Component, props)`. This removes almost all
   per-node allocation: the server writes constant slices and escaped dynamic values straight
   into the response buffer. It is the default for `velt:jsx`; sigx can implement it for its
   SSR output (and emit its resumability and island markers from `jsx` at component
   boundaries). A provider whose client hydrates text nodes one by one exports
   `jsxTextSeparator` (sigx: `<!--t-->`), and the compiler writes it between adjacent text
   parts of the template strings (contracts/jsx.md, "Text separator"). One that renders a
   `null` or boolean child differently when it is its element's only child exports
   `jsxSoleEmpty` (sigx: `""`) for the compiler to write there (contracts/jsx.md, "Sole child").

Types:

- `JSX.Element` (the provider's node or fragment type), `JSX.IntrinsicElements` (the allowed
  attributes per tag, so typos in attributes are compile errors), `JSX.ElementChildrenAttribute`.
- Generic components `function List<T>(props: ListProps<T>)` work as in TypeScript: explicit
  type arguments on the tag, or inference from the props and the children (typed values and
  children first, then arrow functions, which get their parameter types from the result).
- Generic arrow functions (`const id = <T>(x: T): T => x;`) are generic functions, at module
  level and as a `const` in a function body (a nested generic function, instantiated per
  call). Their parameters need types; without a return type one is inferred from the body by
  the rules for functions (`const id = <T>(x: T) => x;` returns `T`; `async` gives
  `Promise<T>`). A Velt function value has exactly one type, so there are no generic function values:
  using one as a value needs a function type (`const f: (x: i64) => i64 = id;`), and a
  generic arrow in any other position is a compile error with a fix-it.
- Components are functions `(props: P) => JSX.Element`. **Async components**
  `(props: P) => Promise<JSX.Element>` are allowed on the server (data loading) and awaited by
  the renderer; streaming providers flush finished parts while later ones load (hybrid promises
  make sibling async components run concurrently, as in JavaScript).
- Children: `children?: JSX.Element | JSX.Element[] | string | number | null` (no `undefined`;
  `false` and `true` render nothing in `velt:jsx`, as in TypeScript and React; a provider may
  render a placeholder of its own instead, as sigx does for hydration).
- Safety: text and attribute values are **always escaped**; raw HTML only goes through an
  explicit provider API (`velt:jsx`'s `raw(html)`), so cross-site scripting by default can't
  happen.
- Event handlers (`onClick={…}`) can't run on the server: a provider decides how to serialize
  them (sigx: resumable handler references); `velt:jsx` rejects them at compile time.

## `velt:jsx` (the default provider)

`renderToString(el)`, `renderToStream(el, res)` (writes into a `velt:http` response while
rendering, flushing at async component boundaries), `Fragment`, `raw(html)`, and a full
`JSX.IntrinsicElements` for HTML. Precompile mode. Goal: the TechEmpower "fortunes" test written
in TSX at least as fast as the hand-written template (`bench/web`).

## Sharing components with the client

Velt is TypeScript-shaped, so a component written in the common subset (typed props, no
`undefined`, no truthiness on numbers or strings, template literals for text) compiles with both
`tsc` (for the client) and `velt` (for the server). That makes "write once, render on the server
in Velt, hydrate in the browser" possible without a second implementation.
`velt check --ts-compat` ([the CLI](../../tooling/cli.md#code-shared-with-typescript---ts-compat))
flags constructs outside the subset (#13).

### The common subset

A file in the subset passes `velt check`, passes `tsc --noEmit` (TS ≥ 5.2, `strict`, `target:
ES2022`, `lib: ["ES2023", "ESNext.Disposable", "DOM"]`, `moduleResolution: "bundler"`, `jsx:
"react-jsx"` with the provider as `jsxImportSource`), and behaves the same under both, or gets a
finding saying how it differs. The lint only looks at code Velt accepts: a file is checked first
and linted only without errors of its own. TypeScript that Velt rejects is a separate list
(#326).

The lint lives in `crates/velt_tscompat`. Its rules on the syntax tree walk each module with
`velt_syntax::visit`; the rules on types ask the checker's IDE analysis of the same program
(`velt_sema::ide`: `type_of(span)` with a structured type view, `def_at` for what a name refers
to; [the contract](../contracts/sema_ide.md#type-query)). Every finding carries a code, a
severity, a message (what TypeScript does, why Velt differs, what to write) and, when the
replacement is mechanical, a fix.

Errors where `tsc` rejects the code:

| Code | Construct | Fix | |
|---|---|---|---|
| `velt-number-type` | `i8` … `i64`, `isize`, `u8` … `u64`, `usize`, `f32`, `f64` | `number` (a fix for `f64`, the same type) | |
| `bool-type` | `bool` | `boolean` (a fix: the same type) | |
| `number-suffix` | `5i32`, `1.5f32`, also in literal types | a fix drops the suffix where the value stays the same | |
| `int-cast` | `x as i64` (any integer type) | `Math.trunc(x)`, with a `number` target | |
| `struct` | `struct P { … }`, named literals `P { … }` | `class`, or a type with object literals | |
| `extend` | `extend T { … }` | a function | |
| `throws` | `throws E` on functions, methods, arrows and function types | a fix removes it where Velt infers it (a body) | |
| `promise-error-type` | `Promise<T, E>` | `Promise<T>` (a fix on the return type of a function with a body) | |
| `interface-body` | default method bodies in an interface | a base class or a function | |
| `velt-import` | `velt:*` imports | keep them out of the shared files | |
| `outside-import` | relative imports of files not being linted | lint them too, or keep the import out | |
| `jsx-provider` | JSX on the default `velt:jsx` provider | a provider with both runtimes | |
| `velt-global` | prelude exports and builtins TypeScript doesn't have: `spawn`, `shared`, `attempt`, `assertEq`, `deepEqual`, `JsonValue`, `Comparable`, `process`, … | a hint per name | |
| `velt-member` | members TypeScript doesn't have: `xs.isEmpty()`, `m.upsert(…)`, `x.unwrapOr(d)`, `a.compareTo(b)`, `x.clone()`, `JSON.parse<T>(…)`, `Promise.withResolvers()` (ES2024), … | a hint per member | |
| `map-iter-as-array` | `m.keys()`, `values()`, `entries()` used as an array (a member, an index, a declared type, a return value) | `[...m.keys()]` (a fix) | |
| `null-into-optional` | `null` for an optional parameter or field (`x?: T`), whose TypeScript type is `T \| undefined` | a fix leaves it out | |
| `undefined-into-null` | a value that is `undefined` in JavaScript (below) where a `T \| null` is declared: an annotated variable, a return value, an argument, a field | `?? null` (a fix) | |
| `catch-unknown` | `e.x` in `catch (e)` where `instanceof` doesn't narrow `e` | `if (e instanceof C)` | |
| `comparable` | `<` on a `T extends Comparable<T>` (the type itself is `velt-global`) | a comparator parameter | Planned |

Accepted by `tsc`, but behaves differently:

| Code | Construct | Severity | |
|---|---|---|---|
| `declare-fn` | `declare function` (a `ReferenceError` in JS) | error | |
| `jsx-pragma-comment` | `// @jsxImportSource x`: `tsc` reads the pragma only from a block comment and builds with the client's configured provider | error (a fix: `/** @jsxImportSource x */`) | |
| `int-division` | `/` (and `/=`) whose operands have integer types: Velt truncates | error (a fix: `Math.trunc(a / b)`, or `(a as number) / 2` for integer literal types) | |
| `strict-null-eq` | `=== null` / `!== null` on values that are `undefined` in JS (below) | error (a fix: `==` / `!=`) | |
| `object-in-template` | `${x}` of an array, tuple, map, object or class instance without its own `toString()`: Velt prints the contents | error | |
| `default-sort` | `sort()` / `toSorted()` without a comparator on numbers | error (a fix: `(a, b) => a - b`, not for unsigned elements) | |
| `json-map` | `JSON.stringify` of a value holding a `Map` (in a field, element or union member) | error | |
| `null-default` | a destructuring default on a property whose type includes `null` (not optional): Velt applies it to `null`, JS only to `undefined` (#431) | error (a fix: `const x = p.x ?? d`) | |
| `nullable-in-template` | `${x}` where `x` may be `undefined` in JavaScript (an optional field or parameter, `m.get(k)`, `xs.find(…)`, `a?.b`, a variable holding one); a `T \| null` that is never `undefined` prints `null` in both | warning | |
| `unsigned-arith` | `-`, `-=`, `--` with an unsigned result (`xs.length - 1` wraps at zero) | warning | |
| `implicit-dispose` | `[Symbol.dispose]` outside `using` | warning | Planned |
| `init-order` | derived classes with field initializers (#273) | warning | Planned |

Notes on the rules as built, against the issue's first design:

- `boolean` is the same type as `bool` (#353) and `number` the same as `f64`, so their fixes are
  exact. The other number types have no fix: `number` would change integer arithmetic.
- `struct` also covers named object literals (`P { … }`), which `tsc` rejects too.
- `throws` covers arrows and function types as well as declarations; the fix is offered only
  where Velt infers the thrown types without the clause (a function with a body).
- `int-cast` reports the cast as a whole, not its type again as `velt-number-type`. It has no
  fix: `Math.trunc(x)` is a `number`, so code that expects an integer (`const i: i64 = …`)
  needs its type changed too.
- `number-suffix` offers its fix only when the literal is the same value as a `number`: not for
  integers past 2^53, nor for an `f32` literal that rounds (`0.1f32`).
- `promise-error-type` offers its fix only on the return type of a function, method or arrow
  with a body, where Velt infers what it rejects with (as `throws` does); elsewhere (function
  types, declarations without a body) dropping `E` would lose it.
- Defaults are linted too: of parameters (arrows included) and in destructuring patterns.
- `outside-import` applies to relative specifiers (`./`, `../`); package names and `paths`
  aliases aren't checked yet.
- `jsx-provider` reports one finding per module, at its first element.
- `declare-fn` is valid Velt only in a package with a native library.
- `jsx-pragma-comment` reports the pragma Velt uses: the first one in the comments before the
  first token, in a module with JSX (Velt loads no runtime for one without). Its fix rewrites a
  comment holding only the pragma; a line comment with other text gets the finding without one.
- Values that are `undefined` in JavaScript where Velt has `null` (for `strict-null-eq` and
  `undefined-into-null`): an optional field or parameter (`x?: T`), `Map.get`, `find` /
  `findLast`, `pop` / `shift`, `at` (arrays and strings), anything after `?.`, and a variable
  initialized with one of these. `process.env` would be one, but `process` itself is
  `velt-global` (the baseline has no Node types).
- `int-division` can only meet integer types that `velt-number-type` reports where they are
  written (or that come from another shared file), and unions of integer literal types
  (`(1 | 3)[]`, whose elements divide as integers). Its fix keeps Velt's quotient
  (`Math.trunc(a / b)`) where both operands have integer types; an operand of integer literal
  types divided by a number literal becomes `(a as number) / 2` (JavaScript's quotient), since
  Velt rejects `Math.trunc` there (#462). That fix is offered only where the quotient goes
  nowhere that declares an integer: not as the return value of a function returning one, the
  initializer of a variable annotated with one, an argument for an integer parameter, an
  integer field's value, nor in arithmetic or a comparison with any of those or with a declared
  integer, where a fraction wouldn't fit. There it has no fix.
- `object-in-template` passes enums, primitives and classes that declare `toString()`
  themselves: Velt calls a class's own `toString()`, as JavaScript does, but not an inherited
  one, so a subclass that only inherits it is reported.
- `velt-global` and `velt-member` look only at names that resolve to the prelude or to a
  builtin; a function, class or method the program declares is never one. Every prelude export
  and member is classified as TypeScript-standard or Velt-only
  (`crates/velt_tscompat/src/typed/prelude.rs`), and a test fails on one that isn't, so a new
  prelude member is decided on when it is added. Builtins (`spawn`, `attempt`, `console.log`,
  `xs.push`, `Promise.withResolvers`) are listed by hand. `x.clone()` is reported on any
  receiver where it is the compiler's own, that is, wherever the type doesn't declare a
  `clone()` itself.
- `null-into-optional` covers arguments of functions, methods and constructors the program
  declares, object literal fields and assignments to fields; its fix removes trailing `null`
  arguments and `null` properties.
- `catch-unknown` treats `e` as narrowed inside an `if (e instanceof C)` branch, after
  `e instanceof C &&` and in the `?` branch of `e instanceof C ? … : …`; not yet after an early
  exit (`if (!(e instanceof C)) throw e;`).
- `null-default` skips optional properties (`x?: T`): JavaScript leaves them `undefined`, so the
  default applies in both. Its fix rewrites the declaration when the destructured value is a
  variable or a field path, with one `const x = p.x ?? d` per such property.
- Not built: `comparable`'s `<` on a `T extends Comparable<T>` (the bound's type is
  `velt-global`), `implicit-dispose` and `init-order`. Nor `JSON.stringify` of an optional field
  of an object type, which Velt writes as `null` and JavaScript leaves out.

**The oracle.** The claims are checked against the real `tsc` every night
(`crates/velt_tscompat/tests/oracle.rs`, `gh workflow run nightly -f only=oracle`). It uses a
pinned `typescript` (`tests/tscompat-oracle/package.json` and its lock file, installed with
`npm ci`) and the baseline above (`tests/tscompat-oracle/tsconfig.base.json`; JSX goes to a
stand-in provider declared in `jsx/jsx-runtime.d.ts`, so nothing is fetched). `tsc` reads each
file through its API (`diagnostics.mjs`), which lists type errors even in a file with syntax
errors.

- Every rule has a claim: `tsc` rejects it (a sample in `rejected/<code>.ts`), `tsc` accepts it
  but JavaScript runs it differently (`behaviour/<code>.ts`), or `tsc` can't
  decide it, with the reason in the test (`outside-import`: `tsc` follows relative imports
  anywhere, and the rule is about which files the client shares; `jsx-provider`: `tsc` uses the
  provider the client configures, the rule is that `velt:jsx` has no JavaScript runtime). A rule
  in `velt_tscompat::RULES` without a claim and its sample fails `cargo test -p velt_tscompat`,
  with or without Node.
- A sample reports only its own rule, is valid Velt (`rejected/`; `veltc`'s `ts_compat` test),
  and `tsc` reports an error on every line the lint reports. A behaviour sample compiles. One
  that compiles only because `tsc` ignores the construct is checked with the lint's fix applied
  too, where `tsc` must report the error the test names: `jsx-pragma-comment`'s sample names a
  provider that doesn't exist, which `tsc` reports (TS2875) only once the pragma is a block
  comment.
- Every rule fixture, its `.fixed` snapshot and `clean.ts` go through `tsc` a top-level
  declaration at a time: a declaration the lint passes must compile, and one it reports with a
  rule `tsc` rejects must not.
- Each behaviour sample with a `main` prints differently under `velt run` and under Node (its
  types stripped, `main()` called), and with the lint's fixes applied (from `--json`) prints the
  same under both (`crates/veltc/tests/ts_compat_node`). It runs only with `VELT_TSC_ORACLE`
  set, which the nightly job does (Node 24; a Node that can't strip types, older than 22.7, is
  then a failure); elsewhere, the pull request gate included, it is skipped with a message.

Without Node or the installed packages the `tsc` part is skipped with a message, so the pull
request gate doesn't need Node; the nightly job sets `VELT_TSC_ORACLE=1`, which makes a missing
`tsc` a failure.

Documented but not linted: `i64` past 2^53, integer `/ 0`, out-of-bounds indexing, `-0` printing,
exit codes.

**Where it runs** (step 4). `tsCompat: ["src/components", "src/models"]` in `package.vlt` lists
the shared folders ([the manifest](../../tooling/manifest.md#tscompat)): `/`-separated, inside the
package, each once and none inside another. `velt check --ts-compat` without paths lints their
files; outside a package or without `tsCompat` it fails with a message pointing at the field. A
plain `velt check` doesn't lint them: the lint stays opt-in (one flag for CI), so a package's
check result doesn't change when a folder is shared. The language server lints an open document
inside the folders on its own analysis (`velt_tscompat::lint_program` takes the loaded modules
and the checker's diagnostics, so nothing is checked twice, and lints only the document, though
its imports are judged against every loaded file in the folders), publishes the findings as
diagnostics (code = rule, source `velt ts-compat`) and offers each fix as a preferred quick fix
([editors](../../tooling/editors.md#code-shared-with-typescript)). It caches each package's
folders by the manifest's text or modification time and re-analyzes the open documents when a
manifest closes or changes on disk, or a folder appears or disappears. In both, the files in
scope are those in the folders, found by one walk (`vpm::sources::walks_into`: no
`node_modules/`, `target/`, hidden, symlinked or nested package directories), so an import
leaving them is `outside-import`.

The typed rules need the checker's IDE analysis: the language server has it already, and
`velt check --ts-compat` runs it once more on the loaded program (a few milliseconds per run:
about 7 ms for one file with the prelude, 24 ms for a hundred files, in a release build).

## Implementation plan

1. `velt_syntax`: JSX lexing (context-sensitive `<`, JSX text, entities) and AST nodes;
   `velt fmt` printing; parser fuzz and robustness tests.
2. `velt_sema`: desugar JSX to the provider's calls (generic or precompile) using the configured
   import source; typing through the `JSX` namespace; diagnostics with TypeScript's wording.
3. `velt:jsx`, end-to-end tests, and bench/web fortunes in TSX; language server: completion of
   tags and attributes from `JSX.IntrinsicElements`, hover, go-to-definition of components.
4. Providers live in their own repositories: Velt ships the generic infrastructure (JSX syntax,
   the `jsxImportSource` lowering, precompile mode, `velt:jsx` as the reference provider,
   typing, editor support) and documents the provider contract (required exports, precompile
   functions, how markers and serialization hooks work). Framework-specific adapters are
   packages in their framework's repository.
