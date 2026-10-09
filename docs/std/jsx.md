# velt:jsx
`import { renderToString, renderToStream, raw, Element } from "velt:jsx"`. Server-side rendering
of TSX to HTML: the default JSX provider ([provider contract](../internals/contracts/jsx.md)).
A module containing JSX compiles to calls into `velt:jsx/jsx-runtime` and sees its types as
`JSX.Element`, `JSX.IntrinsicElements`, …; this module renders the result.

- `renderToString(el: Element): Promise<string>` (throws `RenderError`): the HTML, after
  awaiting the async components. `renderToStringSync(el): string` for a tree without async
  components (throws `RenderError` if it has one).
- `renderToStream(el: Element): BodyStream`: the HTML as a streamed [response
  body](http.md#the-response) (like React's `renderToReadableStream`), `new
  Response(renderToStream(el), init)`. The markup above each pending async component is sent
  before the component is awaited, so it reaches the client while it loads; rendering stops
  once the client has gone away. A component that throws a `RenderError` cuts the response off.
- `raw(html: string): Element`: markup inserted without escaping, the only way to emit HTML
  from a string (never pass it user input). `Fragment` is `<>…</>`.
- Types: `Element` (what every JSX expression is), `Child` (`Element | Element[] | string |
  i64 | f64 | bool | null`), `AttrValue` (`string | i64 | f64 | bool | Style | null`), `Style`
  (`Record<string, string | f64>`, the object form of `style`), `Text`
  (`string | i64 | f64 | bool | null`, text the precompiler folds into strings),
  `IntrinsicElements`, `ElementChildrenAttribute`, `RenderError { component; message }`.
  `Element`'s fields are internal: its markup comes from `renderToString`.
- `velt:jsx/attrs` exports the attribute types `IntrinsicElements` is made of: `HtmlAttrs`
  (the global attributes) and one type per element with attributes of its own (`AnchorAttrs`,
  `ButtonAttrs`, `InputAttrs`, …, each `HtmlAttrs & { … }`; `SvgAttrs` and `MathAttrs` and the
  types built on them, such as `CircleAttrs` and `MoAttrs`, for SVG and MathML), for providers
  that extend them
  ([Extending `IntrinsicElements`](../internals/contracts/jsx.md#extending-intrinsicelements)).

Rendering rules:
- Text and attribute values are escaped as react-dom escapes them (`&amp;` `&lt;` `&gt;`
  `&quot;` `&#x27;`; `escapeHtml` writes `'` as `&#39;`), so user data cannot inject markup. Numbers render like `${n}`; `true`, `false` and `null` children render nothing.
- A `true` attribute renders as the bare name (`<input disabled>`); `false` and `null` omit it.
- `style` takes a string or an object, `style={{ fontSize: 14, color: c }}`, rendered with
  React's rules (react-dom 19's `style` serialization, checked on the cases in the
  `std/jsx_style` golden): camelCase names in kebab-case
  (`font-size`; `WebkitX` and `msX` get their `-webkit-` / `-ms-` prefix; custom properties
  such as `"--accent"` stay as written), `px` after numbers except 0, custom properties and
  unitless properties (`lineHeight`, `opacity`, `zIndex`, `flexGrow`, `WebkitLineClamp`, …),
  values trimmed, empty strings left out, declarations joined by `;`:
  `style="font-size:14px;color:teal"`.
- Void elements (`area base br col embed hr img input link meta source track wbr`) have no end
  tag, and children of one are a compile error (`<br>x</br>`); any other empty element is
  written `<x></x>`, also inside `<svg>` and `<math>`. `key` is not rendered.
- `IntrinsicElements` lists every HTML, SVG 2 and MathML Core element with its attributes and
  the global ones, under their HTML, SVG and MathML names (`class`, `for`, `tabindex`,
  `viewBox`, `clipPath`), so a misspelled tag or attribute is a compile error. Hyphenated and
  namespaced attributes (`data-*`, `aria-*`, `http-equiv`, `stroke-width`, `xlink:href`) and
  custom elements (`<my-widget>`) are not checked. MathML's true/false attributes take the
  strings (`stretchy="false"`), as a `false` value would leave the attribute out. There are no
  event handler attributes (`onclick`): std/jsx renders on the server, and client-side
  frameworks bring their own provider.
- Components are functions `(props: P) => Element`; `velt:jsx` calls each one as its element is
  created. **Async components** `(props: P) => Promise<Element>` start then too, so siblings
  load concurrently; rendering awaits them in document order. A component that throws, or an
  async one that rejects, makes rendering throw a `RenderError` whose `component` names it
  (`"<module path>#<Name>"`) and whose `message` includes the original error formatted like
  `${e}` (the original type does not travel through `Element`); the first failure in document
  order wins.
- **Generic components** (`function List<T>(props: { items: T[]; render: (x: T) => Child })`)
  take their type arguments on the opening tag, `<List<number> items={xs} render={(n) => n} />`
  (the closing tag has none: `</List>`), or infer them like TypeScript from the props and the
  children (``<Labelled label={(v) => `${v}`}>{41}</Labelled>`` gives `T = number` when
  `children: T`); arrow function props get their parameter types from what was inferred.
- Elements render as they are created: a tree without async components is already its HTML,
  kept as a tree of template strings that each element takes over from its children instead of
  copying their markup, so rendering copies the markup once (`renderToStream` writes the pieces
  into the response). Static markup
  is precompiled to constant strings, with dynamic text and attributes folded in through
  template literals (`jsxTemplate`, `jsxTemplateString`, `jsxEscape`, `jsxAttr`), and a list
  such as `{rows.map((r) => <tr>…</tr>)}` whose rows hold only markup and text is built from
  strings (`jsxList`), as a hand-written template would be;
  `velt:jsx/generic/jsx-runtime` is the same provider without that mode
  (`/** @jsxImportSource velt:jsx/generic */`).

A complete app, with components shared with a TypeScript client, async data loading and
streamed pages: [`examples/apps/ssr-blog`](../../examples/apps/ssr-blog/README.md).

```tsx
import { renderToStream, Element } from "velt:jsx";
import { serve } from "velt:http";

async function Posts(): Promise<Element> {
  const posts = await loadPosts(); // a database query
  return <ul>{posts.map((p) => <li key={p.id}>{p.title}</li>)}</ul>;
}

function Page(props: { title: string }): Element {
  return (
    <html>
      <head><title>{props.title}</title></head>
      <body>
        <h1>{props.title}</h1>
        <Posts />
      </body>
    </html>
  );
}

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    // The head and heading are sent at once; the list follows when the query is done.
    return new Response(renderToStream(<Page title="Posts" />), {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  });
}
```

The `tsx` block above shows TSX syntax, which the compiler lowers to the `jsx-runtime`
factories. Called directly, as the compiler does:

```ts
import { jsx, jsxAsyncComponent } from "velt:jsx/jsx-runtime";
import { Element, raw, renderToString } from "velt:jsx";

async function Greeting(props: { name: string }): Promise<Element> {
  await sleep(1);
  return jsx("p", ["class"], ["hi"], [`Hello, ${props.name}`], null);
}

async function main() {
  // <div title={t}>{"<b>"}{raw("<hr>")}<Greeting name="Ann & Bo" /></div>
  const el = jsx("div", ["title"], [`"quoted"`], [
    "<b>",
    raw("<hr>"),
    jsxAsyncComponent((p: { name: string }) => Greeting(p), { name: "Ann & Bo" }, null, "doc#Greeting"),
  ], null);
  console.log(await renderToString(el));
  // <div title="&quot;quoted&quot;">&lt;b&gt;<hr><p class="hi">Hello, Ann &amp; Bo</p></div>
}
```
