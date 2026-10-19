# Server-rendered pages with TSX

This guide builds HTML pages on the server with TSX: components, lists, a layout, data loaded by
async components, and pages streamed to the browser. It uses
[`velt:jsx`](../std/jsx.md), the default JSX provider; the precise rules are in
[the reference](../reference/tsx.md). The finished app, with components shared with a
TypeScript client, is [`examples/apps/ssr-blog`](../../examples/apps/ssr-blog/README.md).

## A first page

JSX works in any `.vlt` (or `.tsx`) file. A component is a function from typed props to
`JSX.Element`, and `renderToStringSync` turns an element into HTML:

```ts
import { renderToStringSync } from "velt:jsx";

function Greeting(props: { name: string }): JSX.Element {
  return <h1 class="title">Hello, {props.name}!</h1>;
}

console.log(renderToStringSync(<Greeting name="<Ada>" />));
// <h1 class="title">Hello, &lt;Ada&gt;!</h1>
```

Text and attributes are escaped as they are rendered, so user data can't inject markup. The only
way to write raw HTML is `raw(html)` from `velt:jsx`. Tags and attributes are checked:
`<h1 clas="title">` is a compile error that suggests `class`.

## Lists and conditions

A list is an array of elements, usually from `map`. A condition is a `boolean` (Velt doesn't
treat strings or numbers as conditions), so an optional part is `cond ? <x /> : null`:

```ts
import { renderToStringSync } from "velt:jsx";

type Item = { name: string; done: bool };

function TodoList(props: { items: Item[] }): JSX.Element {
  const left = props.items.filter((i) => !i.done).length;
  return (
    <section>
      <ul>
        {props.items.map((i) => <li class={i.done ? "done" : "open"}>{i.name}</li>)}
      </ul>
      {left == 0 ? <p>All done.</p> : <p>{left} left</p>}
    </section>
  );
}

const items: Item[] = [{ name: "Write", done: true }, { name: "Ship", done: false }];
console.log(renderToStringSync(<TodoList items={items} />));
```

## A layout

The layout of every page is a function that takes the page's content. Write it as a plain
function rather than a component with `children`: the content may hold async components (below),
and an element from a component's own props can't be passed on yet
([the reference](../reference/tsx.md#components)).

```ts
import { renderToStringSync } from "velt:jsx";

function page(title: string, content: JSX.Element): JSX.Element {
  return (
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <title>{title}</title>
      </head>
      <body>
        <main>{content}</main>
      </body>
    </html>
  );
}

console.log(renderToStringSync(page("Home", <p>Welcome.</p>)));
```

## Loading data with async components

An async component returns `Promise<JSX.Element>`. It starts when its element is created, so
the loads of a page run concurrently, and rendering waits for them in document order. Render
such a tree with `await renderToString(el)`:

```ts
import { renderToString } from "velt:jsx";

async function loadComments(post: string): Promise<string[]> {
  await sleep(10); // a database query
  return [`first on ${post}`, "second"];
}

async function Comments(props: { post: string }): Promise<JSX.Element> {
  const comments = await loadComments(props.post);
  return <ul>{comments.map((c) => <li>{c}</li>)}</ul>;
}

const el = (
  <article>
    <h1>Hello</h1>
    <Comments post="hello" />
  </article>
);
console.log(await renderToString(el));
```

A component that throws, or an async one that rejects, makes rendering throw a `RenderError`
naming the component.

## Streaming pages

`renderToStream` turns a page into a streamed response body (a `BodyStream` for
`new Response`, see [`velt:http`](../std/http.md#the-response)) that is sent in chunks: the
markup above each async component goes out before the component is awaited, so the browser gets
the top of the page while the rest loads. The status goes out first, so decide it before
streaming starts:

```ts
import { serve } from "velt:http";
import { renderToStream } from "velt:jsx";

async function Slow(): Promise<JSX.Element> {
  await sleep(100);
  return <p>loaded</p>;
}

async function handle(req: Request): Promise<Response> {
  const path = new URL(req.url).pathname;
  const status = path == "/" ? 200 : 404;
  return new Response(renderToStream(<main><h1>{path}</h1><Slow /></main>), {
    status,
    headers: { "content-type": "text/html; charset=utf-8" },
  });
}

const server = await serve({ port: 0 }, handle);
server.close();
```

To put something before the page, such as `<!DOCTYPE html>`, yield it from an async generator
that then passes on the page's chunks (`examples/apps/ssr-blog/src/server.vlt`).

## Sharing components with a TypeScript client

Components in the common subset of TypeScript and Velt compile with both `velt` and `tsc`, so a
browser bundle can use the same files. Keep them in folders listed under `tsCompat` in
`package.vlt` and check them with `velt check --ts-compat`
([the CLI](../tooling/cli.md#code-shared-with-typescript---ts-compat)). The provider must exist
on both sides: `examples/apps/ssr-blog` names its own (`jsx: { importSource: "./jsx" }`), which
is `velt:jsx` on the server and a small TypeScript runtime for `tsc`, and both render the same
HTML.

## Testing pages

A page is a value, so a test renders it and compares the HTML:

```ts ignore
// tests/greeting.test.vlt
import { renderToStringSync } from "velt:jsx";
import { Greeting } from "../src/greeting";

export function test_escapes_the_name() {
  assertEq(renderToStringSync(<Greeting name="<Ada>" />), `<h1 class="title">Hello, &lt;Ada&gt;!</h1>`);
}
```

## How fast is it

`velt:jsx` compiles a tree of HTML elements into constant strings with the dynamic text folded
in, as a hand-written template literal would be, and keeps the markup of nested elements as
pieces that are copied once, when the page is written. `bench/jsx` compares a TSX page with
hand-written template literals.
