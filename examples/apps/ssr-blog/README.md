# ssr-blog

A server-rendered blog written in TSX: components in `src/`, data loaded by async components,
and every page streamed to the browser with `renderToStream`, so the
layout and the post arrive before the comments have loaded.

```sh
PORT=8080 velt run
open http://127.0.0.1:8080/
```

| Route | |
|---|---|
| `GET /` | all posts, newest first |
| `GET /?tag=t` | posts tagged `t` (`t` URL-encoded) |
| `GET /posts/:slug` | one post and its comments |
| anything else | 404 page |

## Layout

| File | |
|---|---|
| `src/shared/model.ts` | the `Post` and `Comment` types and pure helpers (reading time, excerpt) |
| `src/shared/samples.tsx` | the components with tricky data (quotes, `<`, `&`), for comparing the server's and the client's HTML |
| `src/shared/components.tsx` | the components: `page` (the layout), `PostList`, `PostCard`, `PostBody`, `CommentList`, `Tags`, `NotFound` |
| `src/store.vlt` | an in-memory stand-in for a database, with a delay on every load |
| `src/pages.tsx` | async components (`Posts`, `Comments`) that load data, and the router |
| `src/server.vlt` | the HTTP server: a streamed response per page |
| `jsx/jsx-runtime.vlt` | the package's JSX provider on the server: std/jsx (`jsx.importSource` in package.vlt) |
| `client/jsx-runtime.ts` | the same provider for TypeScript, rendering the same HTML |

**Async loading and streaming.** `Posts` and `Comments` are async components: each starts its
query when its element is created, and `renderToStream` writes and flushes the markup above a
pending one, then waits for it. The status code is decided before the stream starts
(`statusOf`), since the headers go out first.

**Shared with TypeScript.** `src/shared` is in the common subset of TypeScript and Velt
(`tsCompat` in package.vlt): plain types, `number` and `boolean`, typed props, no `velt:`
imports, and the server-only parts (the store, async loading) stay outside it. Check both sides:

```sh
velt check --ts-compat                    # the subset lint over src/shared
npx -p typescript@5.9.3 tsc -p .          # tsc, with client/jsx-runtime.ts as the provider
```

The nightly oracle job also renders `src/shared/samples.tsx` with `velt run` and, compiled by
`tsc`, with Node, and checks that the HTML is the same
(`crates/veltc/tests/ts_compat_node`, `VELT_TSC_ORACLE=1`).

The layout is a function the pages call, `page(title, content)`, rather than a component that
takes children: the content holds async components, and std/jsx can't pass a pending async
element through component props yet
([known gap](../../../docs/internals/contracts/jsx.md#known-compatibility-gaps-each-is-a-compile-error-never-a-behavior-difference)).

`demo.vlt` runs a scripted session against the real server (golden: `demo.out`), and reads one
page chunk by chunk from a server whose comments wait until it has read the post
(`startServer(port, gate)`): the post arrives before its comments, whatever the timing.
