# TSX

Velt compiles TSX, the JSX syntax of `.tsx` files, for **server-side rendering**: an element
is a call into a *JSX provider*, which renders HTML (or builds whatever its client needs). The
default provider is [`velt:jsx`](../std/jsx.md). The syntax and the typing are TypeScript's
`jsx: "react-jsx"` mode; where Velt differs, this page says so. The provider interface is the
[JSX provider contract](../internals/contracts/jsx.md); the design is
[TSX for server-side rendering](../internals/design/tsx.md). For a walkthrough, see
[Server-rendered pages](../book/ssr-pages.md).

```ts
import { renderToStringSync } from "velt:jsx";

function Greeting(props: { name: string }): JSX.Element {
  return <p class="greeting">Hello, {props.name}!</p>;
}

console.log(renderToStringSync(<Greeting name="Ada & Bo" />));
// <p class="greeting">Hello, Ada &amp; Bo!</p>
```

## Where JSX is allowed

JSX is allowed in `.vlt` and `.tsx` files. In a `.ts` file it is an error, as in TypeScript
("JSX is not allowed in a `.ts` file"; rename the file to `.tsx`). Velt has no `<T>expr`
casts, so in `.vlt` and `.tsx` files a `<` where an expression starts is an element or a
generic arrow function (`<T>(x: T): T => x`, [Functions](functions.md)).

## The provider

The *import source* of a module decides its provider, in this order:

1. a `/** @jsxImportSource <source> */` comment among the comments before the first token
   (Velt also reads `// @jsxImportSource <source>`, but `tsc` reads only the block comment,
   so a file shared with TypeScript uses that one);
2. `jsx: { importSource: "<source>" }` in the package's [`package.vlt`](../tooling/manifest.md#jsx);
3. `velt:jsx`.

A module with JSX imports `<source>/jsx-runtime` implicitly. Its exports are the namespace
`JSX` in that module's types (`JSX.Element`, `JSX.IntrinsicElements`, …), as if the module had
`import * as JSX from "<source>/jsx-runtime"`. A module without JSX loads no provider.

## Elements

- `<div class="a">…</div>`, `<br />` and `<>…</>` (a fragment) are expressions of type
  `JSX.Element`.
- A lower-case name (`div`, `my-widget`, `clipPath`) or a namespaced one (`svg:rect`) is an
  **intrinsic element**: a tag of `JSX.IntrinsicElements` (`velt:jsx` lists the HTML, SVG 2 and
  MathML Core elements).
- A capitalized or dotted name (`Card`, `ui.Card`) is a **component**: a value in scope.
- A generic component takes type arguments on the opening tag, `<List<number> items={xs} />`;
  the closing tag has none (`</List>`).

## Attributes

- `name="text"` is a string (entities such as `&amp;` are decoded), `name={expr}` any value,
  and a bare `name` is `true`. A component's prop may also be given an element, `icon=<Star />`
  (an intrinsic element's attribute can't: an element is no `JSX.AttrValue`).
- `{...obj}` spreads the fields of `obj`, whose type must be an object type, at compile time; a
  later attribute replaces a spread field with the same name. Two written attributes with the
  same name are an error ("JSX elements cannot have multiple attributes with the same name.").
- `key={k}` (a `string` or a number) is not an attribute: it goes to the provider separately.
- An intrinsic element's attributes are checked against its field of `JSX.IntrinsicElements`:
  an unknown name is an error, with a suggestion, and each value must have the field's type and
  convert to `JSX.AttrValue`. Names with `-` or `:` (`data-*`, `aria-*`, `xlink:href`), and
  every attribute of a tag with `-` or `:` (custom elements), are only checked against
  `JSX.AttrValue`. A field that is not optional is a required attribute.

```ts error
const link = <a hreff="/docs">Docs</a>;
// error: Property 'hreff' does not exist on type 'JSX.IntrinsicElements["a"]'. Did you mean 'href'?
```

## Children

- Text between tags follows JSX's whitespace rules: a line break between two pieces of text,
  with the indentation around it, becomes one space; a line with only whitespace disappears; and
  spaces within a line stay. Entities are decoded.
- `{expr}` is one child, converted to `JSX.Child`. `{...xs}` is one child too: the array `xs`.
- `{}` and `{/* comment */}` are no children.
- What `true`, `false` and `null` render is the provider's choice (`velt:jsx`: nothing).
- A provider that writes some tags without an end tag lists them (`jsxVoidElements`;
  `velt:jsx`: HTML's void elements `br`, `img`, `input`, …), and children of those tags are an
  error. `{}`, comments and an explicit end tag (`<br></br>`) are allowed.
- `&&` follows Velt's conditions ([Variables and conditions](variables.md)). With a nullable
  object on the left, `{user && <p>{user.name}</p>}` is the element or `null`. With a
  `boolean` on the left, the right side must be a condition too, so `{flag && <b>new</b>}` is
  an error: write `{flag ? <b>new</b> : null}`.

```ts
import { renderToStringSync } from "velt:jsx";

const flag = true;
const items = ["a", "b"];
console.log(renderToStringSync(
  <ul class="list">
    {items.map((x) => <li>{x}</li>)}
    {flag ? <li>new</li> : null}
  </ul>,
));
// <ul class="list"><li>a</li><li>b</li><li>new</li></ul>
```

```ts error
const line = <p>one<br>two</br></p>;
// error: <br> is a void element and cannot have children
```

## Components

A component is a function from its props to `JSX.Element`. Its attributes form an object literal
checked against the props type: a missing required field and an unknown field are errors, with
TypeScript's wording. Its children go into the field that
`JSX.ElementChildrenAttribute` names (`children` by default): one child is passed as itself,
several as an array, each checked against the field's type; children for props without that
field are an error.

```ts
import { renderToStringSync } from "velt:jsx";

function Section(props: { title: string; children: string[] }): JSX.Element {
  return (
    <section>
      <h2>{props.title}</h2>
      {props.children.map((c) => <p>{c}</p>)}
    </section>
  );
}

console.log(renderToStringSync(<Section title="Notes">{"one"}{"two"}</Section>));
// <section><h2>Notes</h2><p>one</p><p>two</p></section>
```

```ts error
function Section(props: { title: string }): JSX.Element {
  return <h2>{props.title}</h2>;
}
const s = <Section />;
// error: Property 'title' is missing in type '{}' but required in type '{ title: string }'.
```

- The compiler does not call a component: it passes it, uncalled, to the provider with its
  props and a name (`"<module path>#<Name>"`), as TypeScript's runtime does. `velt:jsx` calls
  it as the element is created; another provider may call it later, around something, or not
  on the server at all.
- An **async component** returns `Promise<JSX.Element>`. With `velt:jsx` it starts when its
  element is created, so siblings load concurrently, and rendering awaits it
  ([`renderToString`, `renderToStream`](../std/jsx.md)). A provider without async support
  reports an error.
- **Generic components** infer their type arguments from the props and the children, as in
  TypeScript, or take them on the tag.
- A provider may declare **directives**: namespaced attributes such as `client:load` that go
  to the provider rather than into the props (sigx marks islands this way: `<Counter client:load
  start={1} />`). With a provider that doesn't declare them (`velt:jsx`), `client:load` on a
  component is an unknown prop. See [the contract](../internals/contracts/jsx.md#component-directives-optional-exports).
- Until objects are shared references, a component that takes ownership of its props (one that
  uses an element from them, such as `<main>{props.children}</main>`) is called with a copy of
  them, and props holding an element can't be copied yet: an element may hold a pending async
  component. A compile error says so. Pass such content through a plain function instead
  (`page(title, content)`), as [`examples/apps/ssr-blog`](../../examples/apps/ssr-blog/README.md)
  does; a component that only reads other props takes an element prop as it is.

## How elements are compiled

An element becomes calls of the provider's functions, chosen by what it exports
([the contract](../internals/contracts/jsx.md)):

- **Generic:** `jsx(tag, names, values, children, key)` for an intrinsic element,
  `Fragment(children, key)` and `jsxComponent(C, props, key, name)` (or `jsxAsyncComponent`).
- **Precompiled**, when the provider exports `jsxTemplate`, `jsxEscape` and `jsxAttr`
  (`velt:jsx` does): a tree of intrinsic elements becomes one template. Static tags, attributes
  and text are escaped at compile time into constant strings, dynamic text and attributes are
  folded into them, and components, fragments and other elements are its slots. A list such as
  `{rows.map((r) => <tr>…</tr>)}` whose rows are templates without slots is built from strings
  too (with the provider's `jsxList`), as a hand-written template literal would be. Both
  lowerings render the same bytes.

## Differences from TypeScript

- The provider's types are the namespace `JSX` of its `jsx-runtime` module, not a global
  `JSX` namespace or React's types; `JSX.IntrinsicElements` can't be extended by declaration
  merging (a provider builds its own: [the contract](../internals/contracts/jsx.md#extending-intrinsicelements)).
- `{flag && <x />}` with a `boolean` `flag` is an error (write `flag ? <x /> : null`); with a
  nullable object on the left it works as in TypeScript.
- Children of a void element (`<br>x</br>`) are a compile error ("<br> is a void element and
  cannot have children"), where React fails at run time.
- `velt:jsx` has no event-handler attributes: it renders on the server, so `onClick` is a
  compile error (a client-side provider declares its own handlers).
- `{...xs}` as a child passes the array `xs` as one child.
- `number` is JavaScript's number (`f64`). A provider whose `JSX.Child` and `JSX.AttrValue`
  should also take Velt's integer types writes `i64 | f64`, as `velt:jsx` does.
- Class components and `ref` are not supported (compile errors); `JSX.LibraryManagedAttributes`
  and `JSX.IntrinsicAttributes` are ignored.
