# JSX provider contract (Mac round 12)

How the compiler lowers TSX and what a **provider** (`std/jsx`, sigx, any SSR renderer) must
export. Design and rationale: [docs/design/tsx.md](../design/tsx.md). Nothing here is specific to
one framework; providers live in their own packages.

## Choosing the provider
The import source of a file is, in order:
1. a `// @jsxImportSource <source>` pragma among the file's leading comments;
2. `[jsx] importSource = "<source>"` in the package's `velt.toml` (velt_toml.md);
3. `std/jsx`.

A module that contains JSX implicitly imports `<source>/jsx-runtime` (a package path like
`sigx/jsx-runtime` or `std/jsx/jsx-runtime`). Its exports are visible in types as the namespace
`JSX` (`JSX.Element`, `JSX.IntrinsicElements`, …) in that module, exactly as if the file had
`import * as JSX from "<source>/jsx-runtime"`; the factory functions are called by generated
code only. A file that never uses JSX does not load the runtime. A relative pragma source
(`./ui`) is relative to the file; a relative `importSource` in velt.toml to the package root. A
name `JSX` the module declares or imports itself wins over the implicit namespace.

## Why this differs from TypeScript's `react-jsx` runtime
TS calls one `jsx(type, props, key)` for both tags and components and lets the runtime iterate
`props`. Velt has no overloads and no runtime reflection over object fields (objects have
compile-time shapes), so the compiler does the part TS leaves to the runtime:
- intrinsic tags get their attributes as a **name list + value list** (names are constants);
- components are passed to `jsxComponent` / `jsxAsyncComponent` **uncalled**, with their props
  and a compile-time name, exactly like TS's `jsx(Card, props)`: the provider decides when and
  around what a component runs (parent-first rendering, `provide`/`inject`-style context,
  client-only islands, error boundaries), so components shared with a TS client behave the same
  on the server;
- there is no `jsxs`: children are always passed as an array, so the static/dynamic child
  distinction TS uses for key warnings carries no information here.

## Compatibility rule
A component written in the TS/Velt common subset (docs/design/tsx.md, "Sharing components with
the client") must type-check and **behave identically** under `tsc` with the provider's TS
runtime and under `velt` with its Velt runtime: same props object, same children shape (one
child → the child, several → an array, none → field absent/`null`), same `key` handling, same
evaluation order (props first; the component runs when the provider calls it), booleans/`null`
render nothing. Anything the compiler cannot match is a compile error, never a silent
difference. Known gaps are listed at the end.

## Required exports of `<source>/jsx-runtime`
Types (any declaration kind that `import * as JSX` can name):

| Export | Meaning |
|---|---|
| `Element` | What every JSX expression evaluates to (usually a class). |
| `Child` | Union of what may appear as a child, e.g. `Element \| Element[] \| string \| number \| boolean \| null`. Each child expression is coerced to it. |
| `AttrValue` | Union of what an intrinsic attribute may hold, e.g. `string \| number \| boolean \| null`. A provider that serializes event handlers adds its own handler type here. |
| `IntrinsicElements` | Object type; each field is a lower-case tag name whose type is the object type of that tag's attributes (`{ class?: string; href?: string; … }`). A tag that isn't a field is an error. |
| `ElementChildrenAttribute` (optional) | Object type with one field; its name is the props field that receives component children. Default `children`. |

Functions:

```ts
// Intrinsic element: <a href={u} class="x">…</a>
function jsx(tag: string, names: string[], values: AttrValue[], children: Child[],
             key: string | null): Element;
// <>…</>
function Fragment(children: Child[], key: string | null): Element;
// <Card title={t}>…</Card> where Card: (props: P) => Element
//   → jsxComponent(Card, { title: t, children: … }, key, "src/card#Card")
function jsxComponent<P>(component: (props: P) => Element, props: P, key: string | null,
                         name: string): Element;
// The component's accepted return type is whatever the provider's parameter says (e.g.
// `(props: P) => Child` lets components return null, strings or arrays like React's ReactNode).
// <Posts /> where Posts: (props: P) => Promise<Element, E>
function jsxAsyncComponent<P, E>(component: (props: P) => Promise<Element, E>, props: P,
                                 key: string | null, name: string): Element;
```
- The listed parameter order is the contract; a provider may narrow types (e.g. a fixed `E`)
  and the call is then checked against its signature like any call.
- The compiler never calls a component itself, so evaluation order is TS's: the props object
  (including already-built child elements) is evaluated, and the component runs when the
  provider calls it. A provider may call it at once (`std/jsx`, no context) or store it and call
  it while rendering parent-first (a provider with `provide`/`inject`).
- A component that `throw`s: its error type must be accepted by the provider's signature;
  `std/jsx` reports errors from rendering.
- **Until semantics stage 2** (shared object references) a component passed as a value takes
  its props by copy: the compiler passes an adapter `(p: P) => Card(p)` where `Card` moves out
  of its props or is `async`. Props holding a pending async element can't be copied yet (known
  gap, documented in the report); stage 2 removes the adapter and the copy.
- `jsxAsyncComponent` is only needed by providers that support async components; without it,
  an async component is an error ("the JSX provider '<source>' does not support async
  components").
- `name` is a stable, compile-time component identity: `"<module path>#<Name>"`, e.g.
  `"src/components/card#Card"`. Providers use it for hydration/island/resumability markers.
- `key`: `key={k}` is removed from the attributes/props and passed separately (`string` or
  `number`, numbers converted to their decimal string). Missing → `null`.

## How the compiler lowers each construct
- **Tag kind:** a lower-case simple name (`div`, `my-widget`) or a namespaced name (`svg:rect`)
  is intrinsic; a capitalised or dotted name (`Card`, `ui.Card`) is a component value.
- **Intrinsic attributes:** each `name={value}` / `name="text"` / bare `name` (= `true`) is
  checked against the tag's field type in `IntrinsicElements`, then coerced to `AttrValue`.
  Unknown names are errors (TS wording: "Property 'clas' does not exist on type …"), except
  names containing `-` or `:` (`data-*`, `aria-*`, `xlink:href`, custom attributes) and all
  attributes of tags containing `-` or `:` (custom elements, `svg:rect`), which only need to be
  `AttrValue`. A field that is not `T | null` is a required attribute (TS2741 when missing).
  Spread `{...obj}` expands `obj`'s fields at compile time (its type must be an object type),
  checked the same way; a later attribute overrides a spread field in place.
- **Component props:** attributes form an object literal checked against `P` (missing required
  fields and unknown fields are errors); spreads merge like object spread. A function without
  parameters is a component with props `{}`; a generic component's type arguments are inferred from its props (typed values, then arrow functions, then children). Children go into the
  children field: one child → the child itself, several → an array, each checked against the
  field's type; children with no children field in `P` are an error.
- **Children:** text (after JSX whitespace rules and entity decoding) becomes a `string` child;
  `{expr}` is coerced to `Child`; `{...xs}` passes `xs` as one child; `{/* */}` and `{}` vanish.
  `true`, `false` and `null` must render nothing (provider responsibility).
- **Async:** a component whose return type is `Promise<…>` uses `jsxAsyncComponent`. A
  provider that calls it at creation stores the promise (hybrid promises start it immediately, so siblings load
  concurrently) and awaits it while rendering.

## SSR precompile (optional exports)
When the runtime also exports all of

```ts
type Text = string | number | boolean | null;                      // provider-defined union
function jsxEscape(value: Text): string;                           // escaped text ("" for bool/null)
function jsxAttr(name: string, value: AttrValue): string;           // ` name="…"`, ` name`, or ""
function jsxTemplate(strings: string[], slots: Element[]): Element; // strings.length == slots.length + 1
```
the compiler turns every maximal subtree of intrinsic elements into one `jsxTemplate` call (the
shape of Deno's precompile transform, with text folded into the strings):
- static tags, attributes and text are escaped at compile time (rules below);
- a dynamic child whose type is assignable to `Text` and every dynamic attribute are folded into
  the surrounding string with a template literal: `` `<td>${jsxEscape(f.message)}</td>` ``
  (template literals build in place, rt_abi_async.md §12.1, so a row costs what a hand-written
  template costs);
- every other dynamic part becomes an `Element` slot: components (`jsxComponent(C, props, …)`/
  `jsxAsyncComponent`), fragments, `Element`-typed expressions as they are, and any other
  `Child` (arrays, unions containing `Element`) as `Fragment([v], null)`;
- a subtree with no `Element` slot is `jsxTemplate([html], [])`.
- Output is HTML: void elements (`area base br col embed hr img input link meta source track
  wbr`) have no closing tag; any other self-closing element is written `<x></x>`.
- An element with an attribute spread or a `key` is not precompiled (it goes through `jsx`);
  its children may still be templates.
- Precompiled output must be byte-identical to rendering the generic lowering (the golden
  `std/jsx_precompile_equals_generic` checks `std/jsx`).

## Escaping (all providers that render HTML)
- Text: `&` `<` `>` → `&amp;` `&lt;` `&gt;`; attribute values additionally `"` → `&quot;` and
  `'` → `&#39;`. `std/html` `escapeHtml` implements this set.
- `true` attributes render as the bare name, `false`/`null` attributes are omitted.
- The precompile lowering escapes static text and attribute values at compile time with exactly
  `escapeHtml`'s five replacements (also `"` and `'` in text), so a runtime that escapes with
  `escapeHtml` renders byte-identically.
- Raw HTML only through an explicit provider API (`std/jsx` `raw(html)`), never by default.

## `std/jsx` specifics
`std/jsx` (default) implements the generic and precompile functions, has no event-handler
attributes in `IntrinsicElements` (so `onClick` is a compile error with a note to use a client
provider), and offers `renderToString(el)`, `renderToStream(el, res)` (`std/http`
`Response.stream`, flushing at async component boundaries) and `raw(html)`.
`std/jsx/generic/jsx-runtime` is the same provider without the precompile exports.

## Known compatibility gaps (each is a compile error, never a behavior difference)
- Props are copied into a component until semantics stage 2; props holding a pending async
  element are rejected until then.
- Generic components (`<List items={xs} />` with `List<T>`) infer `T` from the props like TS;
  explicit type arguments on tags (`<List<number> …>`) are not supported yet.
- Class components, `ref`, and TS's `JSX.LibraryManagedAttributes`/`IntrinsicAttributes` are
  not supported.

