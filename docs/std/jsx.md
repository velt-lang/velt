# velt:jsx
`import { renderToString, renderToStream, raw, Element } from "velt:jsx"`. Server-side rendering
of TSX to HTML: the default JSX provider ([provider contract](../internals/contracts/jsx.md)).
A module containing JSX compiles to calls into `velt:jsx/jsx-runtime` and sees its types as
`JSX.Element`, `JSX.IntrinsicElements`, …; this module renders the result.

- `renderToString(el: Element): Promise<string>` (throws `RenderError`): the HTML, after
  awaiting the async components. `renderToStringSync(el): string` for a tree without async
  components (throws `RenderError` if it has one).
- `renderToStream(el: Element, w: ResponseWriter): Promise<bool>` (throws `RenderError`): writes
  into a [`Response.stream`](http.md) body, flushing before each pending async component so
  the markup above it reaches the client while it loads. Returns `false`, and stops, once the
  client has gone away. Thrown from the stream producer, a `RenderError` cuts the response off.
- `raw(html: string): Element`: markup inserted without escaping, the only way to emit HTML
  from a string (never pass it user input). `Fragment` is `<>…</>`.
- Types: `Element` (what every JSX expression is), `Child` (`Element | Element[] | string |
  i64 | f64 | bool | null`), `AttrValue` (`string | i64 | f64 | bool | Style | null`), `Style`
  (`Record<string, string | f64>`, the object form of `style`), `Text`
  (`string | i64 | f64 | bool | null`, text the precompiler folds into strings),
  `IntrinsicElements`, `ElementChildrenAttribute`, `RenderError { component; message }`.
- `velt:jsx/attrs` exports the attribute types `IntrinsicElements` is made of: `HtmlAttrs`
  (the global attributes) and one type per element with attributes of its own (`AnchorAttrs`,
  `ButtonAttrs`, `InputAttrs`, …, each `HtmlAttrs & { … }`), for providers that extend them
  ([Extending `IntrinsicElements`](../internals/contracts/jsx.md#extending-intrinsicelements)).

Rendering rules:
- Text is escaped (`&` `<` `>`), attribute values too (plus `"` and `'`), so user data cannot
  inject markup. Numbers render like `${n}`; `true`, `false` and `null` children render nothing.
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
  tag (and no children); any other empty element is written `<x></x>`. `key` is not rendered.
- `IntrinsicElements` lists every HTML element with its attributes and the global ones, under
  their HTML names (`class`, `for`, `tabindex`), so a misspelled tag or attribute is a compile
  error. Hyphenated attributes (`data-*`, `aria-*`, `http-equiv`) and custom elements
  (`<my-widget>`) are not checked. There are no event handler attributes (`onclick`): std/jsx
  renders on the server, and client-side frameworks bring their own provider. SVG and MathML
  elements are not listed yet.
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
- Elements render as they are created: a tree without async components is already its HTML
  string, so rendering it is a copy. Static markup is precompiled to constant strings, with
  dynamic text and attributes folded in through template literals (`jsxTemplate`, `jsxEscape`,
  `jsxAttr`); `velt:jsx/generic/jsx-runtime` is the same provider without that mode
  (`/** @jsxImportSource velt:jsx/generic */`).

```tsx
import { renderToStream, Element } from "velt:jsx";
import { serve, Request, Response, ResponseWriter } from "velt:http";

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
    const res = Response.stream(async (w: ResponseWriter) => {
      // The head and heading are sent at once; the list follows when the query is done.
      await renderToStream(<Page title="Posts" />, w);
    });
    res.setHeader("content-type", "text/html; charset=utf-8");
    return res;
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
