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
  / `.tsx` split isn't needed. A generic arrow needs a trailing comma, `<T,>(x: T) => x`,
  exactly as in `.tsx` files. `velt fmt` formats JSX like Prettier.

## Semantics: TypeScript's automatic runtime, with an SSR precompile mode

Configuration, like TypeScript (`jsx: "react-jsx"` plus `jsxImportSource`):

- `velt.toml` `[jsx] importSource = "sigx"` (a package or `velt:jsx`), or per file
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
in Velt, hydrate in the browser" possible without a second implementation. A
`velt check --ts-compat` lint can later flag constructs outside the subset.

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
