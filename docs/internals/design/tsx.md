# Design: TSX for server-side rendering

Status: proposal; planned after semantics stage 2.

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
  `// @jsxImportSource sigx` on one of the first lines. The default is `velt:jsx`.
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
   boundaries).

Types:

- `JSX.Element` (the provider's node or fragment type), `JSX.IntrinsicElements` (the allowed
  attributes per tag, so typos in attributes are compile errors), `JSX.ElementChildrenAttribute`.
- Components are functions `(props: P) => JSX.Element`. **Async components**
  `(props: P) => Promise<JSX.Element>` are allowed on the server (data loading) and awaited by
  the renderer; streaming providers flush finished parts while later ones load (hybrid promises
  make sibling async components run concurrently, as in JavaScript).
- Children: `children?: JSX.Element | JSX.Element[] | string | number | null` (no `undefined`;
  `false` and `true` render nothing, as in TypeScript and React).
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
`velt_syntax::visit`; the rules on types (marked *Planned* below) need a type query on the
checked program. Every finding carries a code, a severity, a message (what TypeScript does, why
Velt differs, what to write) and, when the replacement is mechanical, a fix.

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
| `comparable` | `Comparable<T>`, `<` on such a `T` | a comparator parameter | Planned |
| `velt-global` / `velt-member` | `spawn`, `shared`, `assertEq`, `JSON.parse<T>(…)`, `isEmpty`, `upsert`, `unwrapOr`, … | case by case | Planned |
| `map-iter-as-array` | `m.keys()` used as an array | `[...m.keys()]` | Planned |
| `null-into-optional` / `undefined-into-null` | `null` into `x?:`; `Map.get`/`find`/`pop` into `T \| null` | omit it; `?? null` | Planned |
| `catch-unknown` | `e.x` in `catch (e)` without narrowing | `instanceof` | Planned |

Accepted by `tsc`, but behaves differently:

| Code | Construct | Severity | |
|---|---|---|---|
| `declare-fn` | `declare function` (a `ReferenceError` in JS) | error | |
| `int-division` | `/` on integer types (Velt truncates) | error | Planned |
| `strict-null-eq` | `=== null` on values that are `undefined` in JS | error | Planned |
| `object-in-template` | `${obj}` / `${xs}` | error | Planned |
| `default-sort` | `sort()` without a comparator on numbers | error | Planned |
| `json-map` | `JSON.stringify` of a `Map` | error | Planned |
| `nullable-in-template` | `${x}` where `x: T \| null` | warning | Planned |
| `string-offsets` | UTF-8 vs UTF-16 offsets (silent on ASCII literals) | warning | Planned |
| `unsigned-arith` | `xs.length - 1` | warning | Planned |
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

Documented but not linted: `i64` past 2^53, integer `/ 0`, out-of-bounds indexing, `-0` printing,
exit codes.

**Planned:** the typed rules (a type query, `ide::type_of(span)`, on the checked program), a
`nightly` oracle running the cases through `tsc` and Node, `tsCompat: ["src/models", …]` in
`package.vlt` for `velt check --ts-compat` without paths, and the findings with quick fixes in
the language server.

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
