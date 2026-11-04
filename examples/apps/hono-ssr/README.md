# hono-ssr: Hono's JSX, rendered by Velt

A small shop whose pages are [Hono](https://hono.dev) JSX components (`hono/jsx`), written once in
`src/shared/` and compiled by two compilers:

- **tsc**, against the real `hono` npm package: `reference/render.tsx` is a Hono app on Node that
  serves the pages with `c.html(...)`;
- **Velt**, against `hono/`, a Velt JSX provider whose output matches `hono/jsx`'s server
  rendering byte for byte, served by a native Velt HTTP server (`src/server.vlt`).

Both print the same bytes for every page: `demo.out` is the Velt server's output (a golden), and
`npm run compare` renders the same pages with real Hono and diffs them against it.

```sh
PORT=8080 velt run                 # the Velt server; open http://127.0.0.1:8080/
velt test                          # the provider's unit tests (expected strings from real Hono)
velt check --ts-compat             # src/shared stays in the TypeScript/Velt common subset
npm install && npm run compare     # real Hono on Node renders the same bytes as demo.out
```

| Route | |
|---|---|
| `GET /` | all products |
| `GET /?tag=t` | products tagged `t` |
| `GET /products/:slug` | one product with its order form |
| anything else | 404 page |

## Layout

| File | |
|---|---|
| `src/shared/model.ts` | the `Product` type, the catalog and pure helpers |
| `src/shared/components.tsx` | `Layout`, `ProductList`, `ProductCard`, `ProductPage`, `NotFound` |
| `src/shared/pages.tsx` | routing: a path and query to a status, a title and an element |
| `src/server.vlt`, `src/main.vlt` | the Velt server (packages/http-server) |
| `hono/jsx/jsx-runtime.vlt` | the provider: what compiled JSX calls (docs/internals/contracts/jsx.md) |
| `hono/jsx/node.vlt` | the element type and `renderToString` |
| `hono/jsx/style.vlt` | `style={{ … }}` objects, with Hono's rules |
| `hono/jsx/index.vlt`, `hono/html.vlt` | what shared code imports: `Child`, `PropsWithChildren`; `raw` |
| `reference/render.tsx` | the same pages as a Hono app on Node |
| `demo.vlt`, `demo.out` | fetches each page from the Velt server (golden) |

## One import specifier in both compilers

`package.vlt` points `jsx.importSource` at `hono/jsx` and maps `hono/jsx` and `hono/html` to the
provider with `paths`. tsc (`tsconfig.json`) resolves the same specifiers to the npm package. So
a shared component is ordinary Hono code:

```tsx
import type { Child } from "hono/jsx";

export function Layout(props: { title: string; children?: Child }) {
  return (
    <html lang="en">
      <body>
        <main>{props.children}</main>
      </body>
    </html>
  );
}
```

## What the provider renders like Hono

Measured against hono 4.13.16 (`tests/render.test.vlt` holds the cases):

- text and attribute values escaped as `& < > " '` → `&amp; &lt; &gt; &quot; &#39;`;
- empty void elements as `<br/>`; a void element with children keeps them (`<br>no</br>`);
- the listed boolean attributes (`checked`, `disabled`, `hidden`, …) as `name=""` or nothing, any
  other attribute's boolean as text (`data-on="true"`);
- `className`, `htmlFor`, `crossOrigin`, `httpEquiv`, `itemProp`, `fetchPriority`, `noModule`,
  `formAction` renamed as Hono does;
- `style={{ fontSize: 12, lineHeight: 1.5 }}` as `font-size:12px;line-height:1.5`, with Hono's
  unitless properties and its filter for names and values that could end the declaration;
- numbers as JavaScript prints them; `null`, `undefined`, `true` and `false` children as nothing;
- event handlers (`onClick`) as nothing: they belong to a client (`hono/jsx/dom`);
- async components awaited in document order; all of them start when their elements are built,
  so they load concurrently, as with Hono's `toString()`.

## Not covered yet

velt-lang/velt#2251 tracks these; each is a compile error, never different HTML:

- `hono/jsx`'s context (`createContext`, `useContext`), `ErrorBoundary`, `Suspense` and
  `renderToReadableStream` (streaming);
- the `html` tagged template from `hono/html` (Velt has no tagged templates);
- children nested deeper than one array (`{[["a", <i />], "c"]}`);
- `className` on tags other than those `hono/jsx/jsx-runtime.vlt` lists;
- an async component's error type must be an `Error`: the provider fixes `jsxAsyncComponent`'s
  error type, because a generic one is not inferred for a component that cannot fail (#2259);
- Hono's routing itself (`new Hono()`): the Velt side uses packages/http-server, and compiling
  Hono's own sources is track B of #2251, which has the measurement.
