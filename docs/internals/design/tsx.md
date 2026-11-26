# TSX for server-side rendering

Status: implemented. Issue: [#12](https://github.com/velt-lang/velt/issues/12).

## Summary

TSX in Velt is for **server-side rendering** first: Velt renders HTML on the server and a client
framework takes over (resumable SSR, streaming, islands). The design is **generic**: any TSX
runtime (sigx, a React-, Preact- or Hono-style renderer, a plain HTML builder) plugs in the same
way, through TypeScript's `jsxImportSource`, and the compiler names no framework. Velt ships the
infrastructure (the syntax, the lowerings, typing, editor support) and `velt/jsx`, the default
runtime; framework runtimes are packages in their framework's repository.

Three principles follow from [TypeScript alignment](ts-alignment.md): look exactly like TSX where
TypeScript already solved the problem; add nothing JavaScript-specific that causes bugs; keep
native speed, because rendering throughput is the point of server-side rendering.

The Reference ([TSX](https://veltlang.com/docs/reference/tsx/)) is the normative text for what a
program means, and [the provider contract](../contracts/jsx.md) is the normative text for what a
runtime exports and how each construct lowers. This note gives the reasons.

## Design

### Syntax

The syntax is TypeScript's TSX:

- Elements `<div class="a">{x}</div>`, self-closing `<br />`, fragments `<>…</>`, components
  `<Card title={t}>…</Card>` (a capitalized or dotted name is a component), spread props
  `{...p}`, `key`, string and expression attributes, children text with JSX whitespace rules,
  `{/* comments */}`.
- Type arguments on an opening tag: `<List<number> items={xs} />`, also after a member name
  (`<ui.List<number> …>`); the closing tag takes none (`</List>`). Inside a tag, a `<` that does
  not follow `=` opens the type arguments, which are lexed as code up to the matching `>` (an
  element as an attribute value, `icon=<Star />`, follows `=`).
- JSX is allowed in `.velt` and `.tsx` modules. In a `.ts` module it is an error, as in
  TypeScript. Velt has no `<T>expr` casts (only `as`), so `.velt` needs no split of its own.
- `velt fmt` formats JSX like Prettier.

**The parser decides where an element starts**, not the lexer: in code the lexer always emits
`<` as a plain `Lt`. Where the parser expects an expression and finds `<` directly followed by a
name or `>`, it first tries a generic arrow (type parameters, a parameter list and `=>`, or a head
starting `<T,`, `<T extends` or `<T =`); otherwise it has the lexer re-lex from that `<` in JSX
mode. The parser pulls tokens on demand, so only its short lookahead is dropped. Consequences:

- Generic arrows are written as in `.ts` files, `<T>(x: T): T => x`. `<T,>` (the `.tsx` spelling)
  works too, and `velt fmt` prints `<T>`.
- A `<` after a keyword used as a name is never JSX: `v.as<User>()`, `v?.as<User>()`,
  `class C { as<T>(): T {…} }`, `x as T`.
- The one program that reads differently from `.tsx` is an element named like a type parameter
  whose text is an arrow signature, `<T>(x: T): T => x</T>`.

### Choosing the runtime

The import source of a module is, in order, a `/** @jsxImportSource <source> */` pragma among the
comments before its first token, `jsx: { importSource: "<source>" }` in `package.velt`, and
otherwise `velt/jsx`. Velt also reads the pragma from a line comment (`// @jsxImportSource x`),
but `tsc` reads it only from a block comment, so files shared with a client use the block form
(the `jsx-pragma-comment` rule below). A module with JSX implicitly imports
`<source>/jsx-runtime`, whose exports are the factory functions and the `JSX` namespace.

### Lowerings

What the runtime exports chooses how its JSX compiles:

1. **TypeScript's automatic runtime**, for a runtime that exports `jsx` or `jsxs` and no
   `jsxComponent` (React, Vue, Preact, Hono, sigx). Every element is one call, as `tsc` emits it
   with `"jsx": "react-jsx"`: `<div a={x}>{y}</div>` is `jsx("div", { a: x, children: y }, key)`,
   with `jsxs` for two or more children and `Fragment` for fragments. Components are passed as
   they are. A runtime written for `tsc` compiles unchanged.
2. **The provider contract**, for a runtime that exports `jsxComponent` (`velt/jsx`). Velt has no
   runtime reflection over object fields, so the compiler does what a JavaScript runtime does by
   iterating `props`: an intrinsic tag gets its attributes as a list of constant names and a list
   of values (`jsx(tag, names, values, children, key)`), children are always an array (there is
   no `jsxs`), and a component is passed uncalled to `jsxComponent` or `jsxAsyncComponent` with
   its props and a compile-time name (`"<module path>#<Name>"`) that providers use for hydration,
   island and resumability markers. The provider decides when and around what a component runs
   (parent-first rendering, context, client-only islands, error boundaries), so a component shared
   with a TypeScript client behaves the same on the server.
3. **SSR precompile**, for a provider-contract runtime that also exports `jsxTemplate`, `jsxAttr`
   and `jsxEscape` (the shape of Deno's precompile transform). Every maximal subtree of intrinsic
   elements becomes one `jsxTemplate(strings, slots)` call: static tags, attributes and text are
   escaped at compile time into constant strings, a dynamic text child or attribute is folded into
   the surrounding string through a template literal (`` `<td>${jsxEscape(f.message)}</td>` ``),
   and components, fragments and other elements are slots. The server writes constant slices and
   escaped values straight into the output with almost no allocation per node. `velt/jsx` uses
   it. A provider whose client hydrates text nodes one by one exports `jsxTextSeparator` (sigx:
   `<!--t-->`), which the compiler writes between adjacent text parts; one that renders a `null`
   or boolean only child differently exports `jsxSoleEmpty`. Precompiled output is byte-identical
   to the provider-contract lowering of the same tree.

### Types

- `JSX.Element` (what every element evaluates to), `JSX.IntrinsicElements` (the attributes each
  tag allows, so a misspelled attribute is a compile error), `JSX.IntrinsicAttributes` and
  `JSX.ElementChildrenAttribute`, found as `tsc` finds them.
- Components are functions `(props: P) => JSX.Element`. Generic components
  (`function List<T>(props: ListProps<T>)`) work as in TypeScript: explicit type arguments on the
  tag, or inference from the props and the children (typed values and children first, then arrow
  functions, which get their parameter types from the result).
- **Async components** `(props: P) => Promise<JSX.Element>` load data on the server. A provider
  that supports them calls them when the element is created, so sibling async components run
  concurrently (Velt's promises start eagerly, as JavaScript's do), and awaits them while
  rendering; a streaming provider sends the finished parts while later ones load.
- Children are `JSX.Element | JSX.Element[] | string | number | null` (no `undefined`). `true`
  and `false` render nothing in `velt/jsx`, as in React; a provider may render a placeholder
  instead, as sigx does for hydration.
- Generic arrow functions (`const id = <T>(x: T): T => x;`) are generic functions, at module level
  and as a `const` in a function body (a nested generic function, instantiated per call). Their
  parameters need types, or a generic function type where the arrow is used; without a return
  type one is inferred from the body (`async` gives `Promise<T>`). Used as a value, a generic
  arrow is a value of a generic function type such as `<T>(x: T) => T`.

### Safety

Text and attribute values are **always escaped**. Raw HTML goes only through an explicit provider
API (`velt/jsx`'s `raw(html)`), so markup injection cannot happen by default. Event handlers
(`onClick={…}`) cannot run on the server: a provider decides how to serialize them (sigx:
resumable handler references), and `velt/jsx` has none in its `IntrinsicElements`, so they are a
compile error.

### `velt/jsx`

The default runtime exports `renderToString(el)`, `renderToStringSync(el)` (for a tree without
async components), `renderToStream(el)` (a `BodyStream` that a `velt/http` response sends while
rendering, flushing the markup before each pending async component), `Fragment`, `raw(html)`,
`RenderError` and a full `JSX.IntrinsicElements` for HTML. It uses the precompile lowering.

### Sharing components with the client

Velt is TypeScript-shaped, so a component written in the common subset (typed props, no
`undefined`, template literals for text) compiles with both `tsc` (for the client) and `velt`
(for the server): it renders on the server in Velt and hydrates in the browser without a second
implementation. `velt check --ts-compat`
([the CLI](https://veltlang.com/docs/tooling/cli/#code-shared-with-typescript---ts-compat))
reports constructs outside the subset ([#13](https://github.com/velt-lang/velt/issues/13)).

A file in the subset passes `velt check`, passes `tsc --noEmit` (TypeScript 5.2 or newer,
`strict`, `target: ES2022`, `lib: ["ES2023", "ESNext.Disposable", "DOM"]`,
`moduleResolution: "bundler"`, `jsx: "react-jsx"` with the provider as `jsxImportSource`), and
behaves the same under both, or gets a finding saying how it differs. The lint looks only at code
Velt accepts: a file is checked first and linted only when it has no errors of its own.
TypeScript that Velt rejects is tracked separately
([#326](https://github.com/velt-lang/velt/issues/326)).

Every finding carries a code, a severity, a message (what TypeScript does, why Velt differs, what
to write) and, when the replacement is mechanical, a fix.

Errors where `tsc` rejects the code:

| Code | Construct | Fix |
|---|---|---|
| `velt-number-type` | `i8` … `i64`, `isize`, `u8` … `u64`, `usize`, `f32`, `f64` | `number` (a fix for `f64`, the same type) |
| `bool-type` | `bool` | `boolean` (a fix: the same type) |
| `number-suffix` | `5i32`, `1.5f32`, also in literal types | a fix drops the suffix where the value stays the same |
| `int-cast` | `x as i64` (any integer type) | `Math.trunc(x)`, with a `number` target |
| `struct` | `struct P { … }`, named literals `P { … }` | `class`, or a type with object literals |
| `extend` | `extend T { … }` | a function |
| `operator` | operator methods (`[Symbol.add]`) and indexers with several keys (`m[i, j]`) | a method with a name |
| `throws` | `throws E` on functions, methods, arrows and function types | a fix removes it where Velt infers it (a body) |
| `promise-error-type` | `Promise<T, E>` | `Promise<T>` (a fix on the return type of a function with a body) |
| `interface-body` | default method bodies in an interface | a base class or a function |
| `velt-import` | `velt/*` imports of values (`import type` passes: the `velt` type package declares the types) | keep them out of the shared files |
| `outside-import` | relative imports of files not being linted | lint them too, or keep the import out |
| `jsx-provider` | JSX on the default `velt/jsx` provider | a provider with both runtimes |
| `decorator` | decorators on functions, enums and parameters; on overload signatures and abstract members | keep them in modules only Velt compiles |
| `unsafe` | `unsafe` blocks, expressions and functions | keep native code out of shared files |
| `velt-global` | prelude exports and builtins TypeScript doesn't have: `spawn`, `shared`, `attempt`, `assertEq`, `deepEqual`, `JsonValue`, `Comparable`, `process`, … | a hint per name |
| `velt-member` | members TypeScript doesn't have: `xs.isEmpty()`, `m.upsert(…)`, `x.unwrapOr(d)`, `a.compareTo(b)`, `x.clone()`, `JSON.parse<T>(…)`, `Promise.withResolvers()` (ES2024), … | a hint per member |
| `map-iter-as-array` | `m.keys()`, `values()`, `entries()` used as an array (a member, an index, a declared type, a return value) | `[...m.keys()]` (a fix) |
| `catch-unknown` | `e.x` in `catch (e)` where `instanceof` doesn't narrow `e` | `if (e instanceof C)` |

Accepted by `tsc`, but behaves differently (all errors):

| Code | Construct | Fix |
|---|---|---|
| `declare-fn` | `declare function` (a `ReferenceError` in JavaScript) | |
| `jsx-pragma-comment` | `// @jsxImportSource x`: `tsc` reads the pragma only from a block comment and builds with the client's configured provider | `/** @jsxImportSource x */` |
| `int-division` | `/` (and `/=`) whose operands have declared integer types: Velt truncates | `Math.trunc(a / b)` |
| `default-sort` | `sort()` / `toSorted()` without a comparator on numbers | `(a, b) => a - b`, not for unsigned elements |

How the rules decide:

- `boolean` is the same type as `bool` and `number` the same as `f64`, so their fixes are exact.
  The other number types have no fix: `number` would change integer arithmetic.
- `throws` covers arrows and function types as well as declarations; its fix is offered only
  where Velt infers the thrown types without the clause (a function with a body).
  `promise-error-type` offers its fix on the same condition, since elsewhere (function types,
  declarations without a body) dropping `E` would lose it.
- `int-cast` reports the cast as a whole, not its type again as `velt-number-type`. It has no fix:
  `Math.trunc(x)` is a `number`, so code that expects an integer (`const i: i64 = …`) needs its
  type changed too.
- `number-suffix` offers its fix only when the literal is the same value as a `number`: not for
  integers past 2^53, nor for an `f32` literal that rounds (`0.1f32`).
- Defaults are linted too: of parameters (arrows included) and in destructuring patterns.
- `jsx-provider` reports one finding per module, at its first element. `jsx-pragma-comment`
  reports the pragma Velt uses (the first one before the first token, in a module with JSX); its
  fix rewrites a comment holding only the pragma.
- `declare-fn` is valid Velt only in a package with a native library.
- `int-division` meets only integer types that `velt-number-type` reports where they are written
  (or that come from another shared file). Unions of integer literal types (`(1 | 3)[]`) are
  numbers and divide as numbers do, so they are no finding.
- `velt-global` and `velt-member` look only at names that resolve to the prelude or to a builtin;
  a function, class or method the program declares is never one. `x.clone()` is reported wherever
  the receiver's type doesn't declare a `clone()` itself.
- `catch-unknown` treats `e` as narrowed inside an `if (e instanceof C)` branch, after
  `e instanceof C &&` and in the `?` branch of `e instanceof C ? … : …`.

**Where it runs.** `tsCompat: ["src/components", "src/models"]` in `package.velt` lists the shared
folders ([the manifest](https://veltlang.com/docs/tooling/manifest/#tscompat)): `/`-separated,
inside the package, each once and none inside another. `velt check --ts-compat` without paths
lints their files; outside a package or without `tsCompat` it fails with a message pointing at the
field. A plain `velt check` doesn't lint them: the lint is opt-in (one flag for CI), so a package's
check result doesn't change when a folder is shared. The language server lints an open document
inside the folders, publishes the findings as diagnostics (code = rule, source `velt ts-compat`)
and offers each fix as a preferred quick fix
([editors](https://veltlang.com/docs/tooling/editors/#code-shared-with-typescript)). In both, the
files in scope are those in the folders, so an import leaving them is `outside-import`.

## Compilation

- **Syntax** (`crates/velt_syntax`): `parser/jsx.rs` decides where an element starts and parses
  it, `parser/arrow.rs` recognizes generic arrows, `lexer/jsx.rs` lexes JSX text, entities and
  attribute strings when the parser asks for JSX mode. `crates/velt_fmt/src/print/jsx.rs` prints
  JSX.
- **Lowering** (`crates/velt_sema/src/body/expr/jsx/`): `provider.rs` loads the runtime and
  chooses the lowering (`lowering_of`, the one place that decides); `automatic.rs` and
  `automatic_props.rs` build TypeScript's automatic-runtime calls; `component.rs` and
  `component_call.rs` pass components; `precompile.rs`, `text_run.rs`, `sole_child.rs` and
  `list_fold.rs` build templates; `escape.rs` escapes static text at compile time. A named
  component is passed as an adapter closure `(props) => Name(props)` where it takes ownership of
  its props or is `async`, because a function value borrows its argument; the adapter shares the
  props, so nothing is copied.
- **The ts-compat lint** (`crates/velt_tscompat`): the rules on the syntax tree (`src/rules/`) walk
  each module with `velt_syntax::visit`; the rules on types (`src/typed/`) ask the checker's IDE
  analysis of the same program (`velt_sema::ide`: `type_of(span)` with a structured type view,
  `def_at` for what a name refers to; [the contract](../contracts/sema_ide.md#type-query)).
  `velt_tscompat::lint_program` takes the loaded modules and the checker's diagnostics, so the
  language server lints on its own analysis without checking anything twice. Every prelude export
  and member is classified as TypeScript-standard or Velt-only in `src/typed/prelude.rs`, and a
  test fails on one that isn't, so a new prelude member is decided on when it is added; the
  compiler's builtins (`spawn`, `console.log`, `xs.push`) are listed there by hand. The folders
  are walked by `vpm::sources::walks_into` (no `node_modules/`, `target/`, hidden, symlinked or
  nested package directories).

**The oracle.** The lint's claims are checked against the real `tsc` every night
(`crates/velt_tscompat/tests/oracle.rs`, `gh workflow run nightly -f only=oracle`). It uses a
pinned `typescript` (`tests/tscompat-oracle/package.json` and its lock file, installed with
`npm ci`) and the baseline above (`tests/tscompat-oracle/tsconfig.base.json`; JSX goes to a
stand-in provider declared in `tests/tscompat-oracle/jsx/jsx-runtime.d.ts`, so nothing is
fetched). `tsc` reads each file through its API (`diagnostics.mjs`), which lists type errors even
in a file with syntax errors.

- Every rule has a claim: `tsc` rejects it (a sample in `rejected/<code>.ts`), `tsc` accepts it but
  JavaScript runs it differently (`behaviour/<code>.ts`), or `tsc` cannot decide it, with the
  reason in the test (`velt-import`, `outside-import`, `jsx-provider`, `int-division`). A rule in
  `velt_tscompat::RULES` without a claim and its sample fails `cargo test -p velt_tscompat`, with
  or without Node.
- A sample reports only its own rule, is valid Velt (`rejected/`; the `ts_compat` test of
  `veltc`), and `tsc` reports an error on every line the lint reports. A behaviour sample
  compiles. One that compiles only because `tsc` ignores the construct is checked with the lint's
  fix applied too, where `tsc` must report the error the test names: `jsx-pragma-comment`'s sample
  names a provider that doesn't exist, which `tsc` reports (TS2875) only once the pragma is a
  block comment.
- Every rule fixture, its `.fixed` snapshot and `clean.ts` go through `tsc` one top-level
  declaration at a time: a declaration the lint passes must compile, and one it reports with a
  rule `tsc` rejects must not.
- Each behaviour sample with a `main` prints differently under `velt run` and under Node (its
  types stripped, `main()` called), and with the lint's fixes applied (from `--json`) prints the
  same under both (`crates/veltc/tests/ts_compat_node`). It runs only with `VELT_TSC_ORACLE` set,
  which the nightly job does (Node 24; a Node older than 22.7, which can't strip types, is then a
  failure); elsewhere, the pull request gate included, it is skipped with a message. `main` may be
  `async`. The pull request gate checks that at least `MIN_RUNNABLE_SAMPLES` samples have a
  `main`, so a change that removes samples lowers that floor itself instead of failing the next
  nightly.

Without Node or the installed packages the `tsc` part is skipped with a message, so the pull
request gate doesn't need Node; `VELT_TSC_ORACLE=1` makes a missing `tsc` a failure.

## Runtime cost

The precompile lowering makes a page in TSX cost what a hand-written template costs: the
TechEmpower fortunes page written in TSX on `velt/jsx` (`bench/web/velt-tsx`) serves within noise
of the same page written with template literals (`bench/web/RESULTS.md`, "Fortunes in TSX").

The typed lint rules need the checker's IDE analysis: the language server has it already, and
`velt check --ts-compat` runs it once more on the loaded program.

## Diagnostics

- An unclosed element whose start could have been a generic arrow gets a note: "if this is a
  generic arrow function, check its parameter list and return type: `<T>(x: T): T => x`".
- A generic arrow without typed parameters, where no generic function type is expected: "a
  generic arrow function needs typed parameters, or a generic function type where it is used".
- JSX in a `.ts` module is a loader error with a note to rename the file to `.tsx`.
- An async component with a provider that has no `jsxAsyncComponent`: "the JSX provider
  '<source>' does not support async components".
- Attribute and props errors use TypeScript's wording and codes (TS2322, TS2741, TS2783, …);
  [the provider contract](../contracts/jsx.md) lists them per construct.

## Compatibility

- A component in the common subset type-checks and behaves the same under `tsc` with the
  provider's TypeScript runtime and under `velt` with its Velt runtime: the same props object,
  children shape, `key` handling, evaluation order and output for booleans and `null`. What the
  compiler cannot match is a compile error, never a silent difference.
- The parser reads `<T>(x: T): T => x</T>` as a generic arrow followed by text, where `.tsx` reads
  an element.
- Velt reads a line-comment `@jsxImportSource` pragma, which `tsc` ignores (`jsx-pragma-comment`).

## Limitations

- Class components, `ref` and TypeScript's `JSX.LibraryManagedAttributes` are not supported.
- A runtime with TypeScript's automatic exports compiles through `jsx` and `jsxs` even when it also
  exports Deno's precompile functions (Preact, Hono); only a provider-contract runtime is
  precompiled.
- `outside-import` checks relative specifiers (`./`, `../`); package names and `paths` aliases are
  not checked.
- `catch-unknown` does not treat `e` as narrowed after an early exit
  (`if (!(e instanceof C)) throw e;`).
- Documented in the Reference but not linted: `i64` past 2^53, integer `/ 0`, out-of-bounds
  indexing, `-0` printing, exit codes.
