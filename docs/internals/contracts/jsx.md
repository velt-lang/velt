# JSX provider contract (Mac round 12)

How the compiler lowers TSX and what a **provider** (`std/jsx`, sigx, any SSR renderer) must
export. Design and rationale: [docs/internals/design/tsx.md](../design/tsx.md). Nothing here is specific to
one framework; providers live in their own packages.

## Choosing the provider
The import source of a file is, in order:
1. a `/** @jsxImportSource <source> */` pragma among the file's leading comments (the comments
   before the first token). Velt also reads it from a line comment (`// @jsxImportSource
   <source>`), but `tsc` reads it only from a block comment, so a file shared with a TypeScript
   client uses the block form (`velt check --ts-compat` reports the line form);
2. `jsx: { importSource: "<source>" }` in the package's `package.vlt` (manifest.md);
3. `std/jsx`.

A module that contains JSX implicitly imports `<source>/jsx-runtime` (a package path like
`sigx/jsx-runtime` or `std/jsx/jsx-runtime`). Its exports are visible in types as the namespace
`JSX` (`JSX.Element`, `JSX.IntrinsicElements`, …) in that module, exactly as if the file had
`import * as JSX from "<source>/jsx-runtime"`; the factory functions are called by generated
code only. A file that never uses JSX does not load the runtime. A relative pragma source
(`./ui`) is relative to the file; a relative `importSource` in package.vlt to the package root. A
name `JSX` the module declares or imports itself wins over the implicit namespace.

JSX is allowed in `.vlt` and `.tsx` modules; in a `.ts` module it is a loader error ("JSX is not
allowed in a `.ts` file", at the first element, with a note to rename the file to `.tsx`), as in
TypeScript. The provider is chosen the same way for every kind of file.

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
A component written in the TS/Velt common subset (docs/internals/design/tsx.md, "Sharing components with
the client") must type-check and **behave identically** under `tsc` with the provider's TS
runtime and under `velt` with its Velt runtime: same props object, same children shape (one
child → the child, several → an array, none → field absent/`null`), same `key` handling, same
evaluation order (props first; the component runs when the provider calls it), the same output
for booleans and `null` (nothing, or the provider's placeholder: "Children" below). Anything the
compiler cannot match is a compile error, never a silent difference. Known gaps are listed at
the end.

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
  parameters is a component with props `{}`; a generic component's type arguments are written
  on the opening tag (`<List<number> items={xs} />`, `ast::JsxElement::type_args`, empty only
  when none are written: `<List<>>` is a syntax error; the closing tag takes none) or inferred from
  its props like TS: typed values and children first, then arrow functions (children last when
  one of them is an arrow function). Children go into the
  children field: one child → the child itself, several → an array, each checked against the
  field's type; children with no children field in `P` are an error.
- **Void elements:** a provider that writes some tags without an end tag declares them, as a
  string constant of space-separated tags (an error otherwise):

  ```ts
  export const jsxVoidElements = "area base br col embed hr img input link meta source track wbr";
  ```
  Children of those tags are then a compile error ("<br> is a void element and cannot have
  children"); `{}` and `{/* */}` are no children, and an explicit end tag (`<br></br>`) is
  allowed. Precompiled templates write these tags without an end tag. `std/jsx` and
  `std/jsx/generic` export HTML's list. Without the export, any tag may have children (an XML or
  terminal provider's `<link>`), and templates leave out the end tag of HTML's void elements.
- **Children:** text (after JSX whitespace rules and entity decoding) becomes a `string` child;
  `{expr}` is coerced to `Child`; `{...xs}` passes `xs` as one child; `{/* */}` and `{}` vanish.
  What `true`, `false` and `null` children render is the provider's choice: nothing (`std/jsx`,
  as in React), or a placeholder of its own, e.g. the empty comment `<!---->` that a hydrating
  client aligns its children on (sigx). A provider renders them the same way in both lowerings
  (its `jsxEscape` returns the placeholder) and in its TS runtime.
- **Async:** a component whose return type is `Promise<…>` uses `jsxAsyncComponent`. A
  provider that calls it at creation stores the promise (hybrid promises start it immediately, so siblings load
  concurrently) and awaits it while rendering.

## SSR precompile (optional exports)
When the runtime also exports all of

```ts
type Text = string | number | boolean | null;                      // provider-defined union
function jsxEscape(value: Text): string;                           // escaped text; bool/null: "" or a placeholder
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
- a subtree with no `Element` slot is `jsxTemplate([html], [])`, or `jsxTemplateString(html)`
  when the runtime also exports

  ```ts
  function jsxTemplateString(html: string): Element;                 // optional: no slots, no arrays
  ```
  which saves the two arrays per call: in a list, every row is such a subtree.
- Output is HTML: void elements (`area base br col embed hr img input link meta source track
  wbr`, or the provider's `jsxVoidElements` when it exports them) have no closing tag; any
  other self-closing element is written `<x></x>`.
- An element with an attribute spread or a `key` is not precompiled (it goes through `jsx`);
  its children may still be templates.
- Precompiled output must be byte-identical to rendering the generic lowering (the golden
  `lang/jsx_precompile_equals_generic` checks `std/jsx`).
- The optional exports below (`jsxTextSeparator`, `jsxSoleEmpty`) are read only for the
  precompile lowering: a provider without the four exports above may export them, and they are
  ignored (not even checked).

### Text separator (optional export)
The HTML parser merges adjacent text nodes, so a provider whose client hydrates text nodes one
by one writes a separator between them (sigx: `<!--t-->`). The generic lowering passes every
child, so the provider inserts it itself; in a template string the compiler has folded the text
together, so a precompiling provider declares the separator instead:

```ts
export const jsxTextSeparator = "<!--t-->";   // a string constant (an error otherwise)
```
The compiler then writes it between two adjacent **text parts** of one element: static text
and dynamic `Text` children, in any combination (`<p>Count: {n}</p>` →
`"<p>Count: <!--t-->" + jsxEscape(n) + "</p>"`, `<p>{name}{n}</p>` →
`jsxEscape(name) + "<!--t-->" + jsxEscape(n)`).
- Tags break adjacency (`<p>a<b>x</b>c</p>` has no separator); `{}` and `{/* */}` do not, as
  they are no children.
- `true`, `false` and `null` are boundaries, not text: no separator goes next to them (the
  provider renders its placeholder, "Children"). A child whose type is only `boolean` or `null`
  is a boundary at compile time; one whose type mixes them with text (`string | null`) is tested
  at run time, with the value read once. All other separators are part of the constant strings,
  so a template costs nothing extra.
- An empty string is text (`a{""}b` → `a<!--t--><!--t-->b`, as sigx renders it).
- Between a template string and a slot (a fragment, a component, an element child), only the
  provider knows what the slot renders, so the separator there is the provider's: a non-empty
  template string starts with text unless it starts with `<`, and ends with text unless it ends
  with `>` (escaped text contains neither, and the separator and placeholders are comments),
  and an empty one is no boundary. This is how the separator crosses fragments and components
  (`<p>{a}<>{b}</></p>` → `a<!--t-->b`).
- An empty string at a slot edge would be invisible there (`<p>{s}<Name/></p>` with `s == ""`
  must render `<p><!--t-->ada</p>`, as in the generic lowering). So a dynamic text child whose
  type has a `string` member (other than a non-empty literal) and that is next to a slot,
  possibly through other such children, is a slot `Fragment([v], null)` itself, and the
  provider renders it as in the generic lowering. Numbers, booleans, `null` and static text are
  never empty and stay in the strings. Like any slot, it is evaluated after the template's
  strings.
- A provider that exports a separator must render `true`, `false` and `null` as a non-empty
  placeholder that starts with `<` and ends with `>` (sigx: `<!---->`). The compiler does not
  check this. If it rendered them as `""`, a `null` would be a boundary inside a template
  string (`<p>{a}{null}{b}</p>` → `ab`) but invisible to the provider next to a slot
  (`<p>{a}{null}<Name/></p>` → `a<!--t-->ada`), and no generic renderer matches both.
- Without the export nothing changes. The generic lowering ignores it.

The golden `lang/jsx_text_separator` renders the cases above through a provider in both
lowerings.

### Sole child (optional export)
A runtime that receives one child as itself and several as an array may render a `true`,
`false` or `null` child differently when it is its element's only child. sigx renders
`<p>{x}</p>` with `x = null` as `<p></p>`, but `<p>{x}a</p>` as `<p><!---->a</p>`. The generic
lowering passes every child, so the provider sees how many there are. `jsxEscape` and
`Fragment` see only the value, so a precompiling provider declares what such a sole child
renders as instead:

```ts
export const jsxSoleEmpty = "";   // a string constant (an error otherwise)
```
The compiler then writes it in place of a `{expr}` or `{...expr}` child that is the only child
of a precompiled element (`{}` and `{/* */}` are no children) and is `true`, `false` or `null`:
- A child whose type is only `boolean` or `null` is the export's string at compile time
  (`<p>{false}</p>` → `"<p></p>"`); any other expression is still evaluated.
- A text child whose type mixes them with text (`string | null`) is tested at run time, with the
  value read once: `jsxEscape(v)` when it is text, the export's string otherwise.
- A slot child whose type has a `null` or boolean member (`JSX.Element | null`) is the slot
  `Fragment([v], null)` when it is something else, and `jsxTemplate([sole], [])` when it is not.
- Static text, elements, components and arrays are rendered as without the export, and so is a
  child that has siblings.

Without the export nothing changes, and the generic lowering ignores it. The golden
`lang/jsx_sole_child` renders these cases through a sigx-like provider in both lowerings.

## Escaping (all providers that render HTML)
- Text and attribute values: `&` `<` `>` `"` → `&amp;` `&lt;` `&gt;` `&quot;`, and `'` as the
  provider writes it: `&#x27;` in `std/jsx` (as react-dom), `&#39;` in sigx and `std/html`
  `escapeHtml`.
- `true` attributes render as the bare name, `false`/`null` attributes are omitted.
- The precompile lowering escapes static text and attribute values at compile time with the four
  replacements every provider shares; static text or an attribute value that contains a `'` is
  passed to `jsxEscape`/`jsxAttr` at run time instead (as one text part: no separator inside
  it), so the provider's own apostrophe appears in both lowerings.
- Raw HTML only through an explicit provider API (`std/jsx` `raw(html)`), never by default.

## `std/jsx` specifics
`std/jsx` (default) implements the generic and precompile functions, has no event-handler
attributes in `IntrinsicElements` (so `onClick` is a compile error with a note to use a client
provider), and offers `renderToString(el)`, `renderToStringSync(el)` (an element without async components),
`renderToStream(el, res)` (`std/http`
`Response.stream`, flushing at async component boundaries) and `raw(html)`.
`std/jsx/generic/jsx-runtime` is the same provider without the precompile exports.

## Extending `IntrinsicElements`
A provider usually starts from std's HTML types rather than copying them: `velt:jsx/intrinsic`
exports std's `IntrinsicElements`, and `velt:jsx/attrs` the attribute types it is made of
(`HtmlAttrs`, `ButtonAttrs`, …). An intersection extends either one, and a field present in
both parts merges recursively, so `Html & { button: Events }` gives `<button>` std's attributes
plus the handlers. `Omit` replaces an attribute's type (an intersection only narrows one):

```ts ignore
import type { IntrinsicElements as Html } from "velt:jsx/intrinsic";
import type { HtmlAttrs } from "velt:jsx/attrs";
import type { Style } from "velt:jsx";

export type Handler = () => void;
export type AttrValue = string | i64 | f64 | bool | Style | Handler | null;
type Events = { onClick?: Handler; onInput?: Handler };

export type IntrinsicElements = Html & {
  button: Events;                                       // std's ButtonAttrs & Events
  input: Events;
  section: Omit<HtmlAttrs, "style"> & { style?: string }; // only the string form
};
```

A provider whose attributes include handlers adds the handler type to `AttrValue` (every
attribute's type must convert to it) and decides what its `jsx` does with them; a server
renderer drops them. TypeScript's other route, merging declarations of a global
`JSX.IntrinsicElements` interface, does not exist: the provider module's export is the one
definition. `tests/golden/lang/_jsx_events/jsx-runtime.vlt` is a complete provider (used by
`lang/jsx_extend_intrinsic`).

## Known compatibility gaps (each is a compile error, never a behavior difference)
- Props are copied into a component until semantics stage 2; props holding a pending async
  element are rejected until then.
- Class components, `ref`, and TS's `JSX.LibraryManagedAttributes`/`IntrinsicAttributes` are
  not supported.

