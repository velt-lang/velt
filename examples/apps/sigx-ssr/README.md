# sigx-ssr: packaging a sigx app with a native Velt server

This is a proof of concept for how a [sigx](https://github.com/signalxjs/core) app ships with a Velt
backend. Vite builds and hot-reloads the browser side. A native Velt binary server-renders the
pages, and the real sigx client hydrates them. The components are written once, in
`src/shared/*.tsx`, and both compilers build them.

It builds on the server-rendering POC in signalxjs/core `experiments/velt` (#379). That POC
showed byte-identical HTML and 2–8× faster renders. This example answers the next question: how
a user installs, develops, builds and deploys such an app.

```
sigx-velt/                 the npm package @sigx/velt: both halves in one package
  package.json             exports the Vite plugin; "files" ships velt/ too
  src/index.js, index.d.ts velt(): runs `velt dev`, proxies documents, builds the server
  velt/package.vlt         a Velt package named `sigx`: the JSX provider (Velt code)
  src/router.js, data.js   the browser halves of @sigx/velt/router and @sigx/velt/data
  velt/src/*.vlt           component, reactivity, jsx-runtime (with precompile), router, data,
                           server (streaming documents, static files, server functions)
  velt/tests/*.test.vlt    provider unit tests (expected strings taken from real sigx output)
app/                       what a user's app looks like
  package.json             sigx, @sigx/vite and @sigx/velt (here linked from ../sigx-velt)
  package.vlt              dependencies: { sigx: { path: "node_modules/@sigx/velt/velt" } },
                           paths: { "@sigx/velt/*": "node_modules/@sigx/velt/velt/src/*" }
  vite.config.ts           plugins: [sigx(), velt()]
  index.html               <!--ssr-outlet--> and the client entry
  src/shared/*.tsx         components, compiled by Vite (JS sigx) and by Velt (the provider)
  src/entry-client.js      the browser entry: hydrates (plain JS, which Velt's tools skip; it
                           also works around a sigx HMR ordering issue, below)
  src/api.server.vlt       server functions, written in Velt (api.server.d.ts types them for TS)
  src/server.vlt           the native server: serveApp(options(), render, serverFns)
  scripts/reference.mjs    renders the same components with JavaScript sigx (api.reference.js
                           stands in for the server functions)
  scripts/e2e.mjs          the end-to-end check
```

## One package, both halves

Velt cannot load npm packages: it never reads `package.json`, its package names cannot be
scoped, and it resolves dependencies from a Velt registry or a local path. So `@sigx/velt` ships
the Velt provider as a complete Velt package inside the npm tarball (`velt/`). The app's
`package.vlt` depends on it by path, and `pnpm install` brings both halves at one version.
The dependency goes through pnpm's symlinked `node_modules` and resolves as is.

The provider's Velt package is named `sigx`. A shared component therefore imports from the same
specifier in both worlds:

```ts
import { component } from "sigx"; // Vite: npm sigx · Velt: node_modules/@sigx/velt/velt

export const Counter = component<{ start: number; label: string }>((ctx) => {
  const count = ctx.signal(ctx.props.start);
  return () => <button onClick={() => count.value++}>Count: {count.value}</button>;
});
```

This is unchanged JavaScript sigx source. It needs `const X = component(...)` (module constants
initialized by pure calls, #383 / #773) and uses `ctx.signal`, which JavaScript sigx already
has. On the server, handlers are typed through the provider's `IntrinsicElements` and render
nothing.

## Routing, data and server functions

Some modules exist in both a JavaScript and a Velt version under one import, as sigx itself does.
`@sigx/velt/router` and `@sigx/velt/data` resolve:
- under Vite, through the npm package's `exports`;
- under Velt, through a `paths` alias to the same package's Velt half.

```tsx
import { createRouter, Link } from "@sigx/velt/router";
import { useData } from "@sigx/velt/data";
import { getStats } from "../api.server";   // Velt server functions

export const App = component<{ path: string }>((ctx) => {
  const router = createRouter(ctx, ctx.props.path);
  const stats = useData(ctx, "stats", () => getStats());
  return () => (
    <main>
      <Link router={router} href="/about" label="About" />
      {router.path.value === "/about" ? <About /> : <Home />}
      {stats.match({ pending: () => <p>Loading…</p>, ready: (s: Stats) => <p>{s.renderer}</p> })}
    </main>
  );
});
```

- **Router.** `<Link>` navigates in the browser without a page load, and the back button
  works. The server renders the requested path.
- **`useData` and streaming.** The server streams the document exactly as sigx's
  `renderDocumentToWebStream` does: the shell with the `pending` state first, then a
  `$SIGX_REPLACE` script with the `ready` content and the data (`__SIGX_ASYNC__`) once it
  arrives, then the completion signal. The browser restores the data without fetching it again.
- **Server functions.** These are `export async function`s in a `*.server.vlt` file.
  - The server calls them directly while rendering.
  - When browser code imports `./api.server`, the Vite plugin turns the import into sigx client
    stubs (`__serverFnStub`) that `POST /_sigx/fn/<key>`.
  - `serveApp` answers those calls in sigx's wire format, and in dev the plugin proxies them to
    `velt dev`.

Both take `ctx` where JavaScript sigx finds the component itself (`useRouter()`, `useData(key,
…)`): Velt has no "current component" yet (#386).

## Islands

`/islands` is a page that is static HTML except for its islands, as in sigx's islands apps:

```tsx
// src/islands/Clicker.tsx: island modules live in src/islands/ and are known by export name
export const Clicker = component<{ start: number; label: string }>((ctx) => { … });

// src/shared/IslandsPage.tsx
<Clicker client:load start={1} label="load" />
<Clicker client:visible start={5} label="visible" />
<Clicker client:only start={9} label="only" />
```

- **Directives.** The provider declares `client` as a directive prefix. This uses the JSX
  contract extension in #815.
- **What the server writes.** It records each island in sigx's `__SIGX_BOUNDARIES__` table:
  strategy, export name and props as JSON. `client:only` gets sigx's empty placeholder.
- **Production.** The server reads the build's islands manifest
  (`dist/client/.vite/sigx-islands-manifest.json`), adds each island's chunk, and preloads it
  from the head.
- **The browser.** The entry hydrates only the islands (`hydrateIslands()` with `sigxIslands()`'s
  registry). The app's code is never loaded.
- **Known gap.** sigx also records each island's signals by name (`"state":{"count":1}`); it
  learns the names from its Vite transform, which rewrites `const count = ctx.signal(…)`. Velt
  has no such names, so the browser re-runs setup from the props. That gives the same result
  here, but not when state on the server differs from what the props give. The e2e compares
  islands byte for byte apart from that field.
- **Resume** (`@sigx/resume`) is not ported. Its handler names come from the Vite build's
  extraction of JavaScript source, so a Velt server would need that build to hand it each
  component's handler sites.

## Develop, build, run

```sh
cd app && pnpm install
pnpm dev                       # vite: http://localhost:5173 (needs `velt` on PATH, or VELT=…)
pnpm build                     # dist/client (Vite) + dist/server/app (velt build --release)
./dist/server/app --port 3000  # serves dist/client and renders the pages
# deploy: HOST=0.0.0.0 PORT=8080 ./dist/server/app (default 127.0.0.1:3000)
pnpm e2e                       # after pnpm build
```

The `velt()` plugin:

- **Dev.** It starts `velt dev` with `--dev --port <free> --template index.html`. Document
  requests (HTML navigations, not Vite's modules or files) go to the Velt server, and the HTML
  passes through Vite's `transformIndexHtml`. Vite serves everything else.
- **Build.** After the client build it runs `velt build --release -o dist/server/app`.
- **Run.** The binary serves `dist/client` (hashed `/assets/` with immutable caching), then
  renders every other path into `dist/client/index.html`. It follows sigx's composition order:
  static files, then the document.

### HMR

| You edit | Browser | Velt server (`velt dev`) | What the plugin does |
|---|---|---|---|
| `src/shared/*.tsx` | sigx HMR swaps the component in place and keeps its state | hot-swaps the changed functions (~150 ms); the next load renders the new markup | nothing more |
| `src/entry-client.js`, CSS | Vite HMR | not involved | nothing |
| `src/server.vlt`, the provider | not in Vite's module graph | hot-swap, or a restart when a layout or signature changed (the port is kept) | a full reload once Velt reports the swap |
| a Velt compile error | Vite's error overlay shows the diagnostics | the previous version keeps serving | shows the diagnostics; reloads (clearing it) once a build succeeds |

Between a save and Velt's status line, document and server-function requests wait (up to 2 s),
so a refresh never
gets the old server's HTML.

## Checked by `scripts/e2e.mjs`

1. For each page, Velt's whole streamed document is byte-identical to sigx's
   `renderDocumentToWebStream` for the same `.tsx` files. The shell arrives before the data
   (about 1 ms against 300 ms), and a server function answers in sigx's wire format.
2. The production binary serves the client build, and the page hydrates with the streamed data
   restored, without a server-function call. Two clicks give `Count: 3`, and the browser logs
   no warnings or errors. Links navigate without a page load, back works, and `/about` loaded
   directly hydrates.
3. On `/islands`, the load, visible and only islands hydrate and count, and the app's code is
   not loaded. The island chunk is preloaded, and the build's manifests are not served.
4. Under `vite`:
   - editing a shared component hot-updates the browser without a reload, and the server renders
     the new markup;
   - a server-only edit reloads the page with the new HTML;
   - a compile error shows in the overlay while the old server keeps serving.

`velt test` in `sigx-velt/velt` runs the provider's unit tests, and `npx tsc -p .` in `app`
type-checks the shared components against JavaScript sigx. This example has no `package.vlt` at
its top level, so the gate's example-apps test skips it: it needs `pnpm install` first.

## Known limits

- Components that load data are streamed one after another, in page order. sigx starts all
  loads at once and streams them as they arrive.
- A load that fails ends the stream without sigx's error state.
- Two `useData` calls with the same key fetch twice.
- In dev, documents are buffered, not streamed, because Vite's `transformIndexHtml` needs the
  whole page.
- The stub generator finds server functions with a regular expression
  (`export async function name`). Ideas 8 and 3 below would replace it.

## What this found

**Velt**
- `typeof v === "number"` did not narrow a union with a function member and two number types
  (#800).
- An `async` arrow is not accepted where `() => void` is expected (`onClick={async () => …}`),
  so a click handler that awaits a server function can't be written in shared code yet; Velt
  has no `.then` either. A fix is in progress.
- Calling a setter on a captured object (`signal.value = x`) inside an async closure counts as
  modifying the captured variable, but assigning a plain field does not.
- Not supported yet:
  - type parameter defaults (`component<P = {}>`); shared code writes `component<{}>`;
  - quoted property names in object types (`{ "client:load"?: bool }`);
  - `setTimeout` with a sync callback, and `resolve()` with no argument for `Promise<void>`;
  - `paths` targets as arrays, as in TypeScript.
- After #685, std's `Response` `status` and `serve`'s `port` still take `i64`, so a `number`
  needs `as i64`.
- Velt's tools walk every `.ts`/`.tsx` file, and a browser entry with `import.meta` or `import()`
  does not parse, so `velt fmt --check` fails on it. That is why the entry here is a `.js` file.
- `velt build -o dist/server/app` leaves `app.o` and `app.link-stamp` next to the binary.

**sigx**
- `@sigx/vite/hmr` registers its component hook after an `await import("sigx/internals")`, so
  in this app the components of the first module graph were never tracked, and edits did not
  hot-update. `src/entry-client.js` installs the hook before importing the app.
- `SigxAdapter` assumes Vite builds the server bundle. `velt()` is a plain Vite plugin until an
  adapter mode exists where the server is built by something else (`serverBuild: 'none'`) and
  dev documents are proxied.

## Ideas for Velt that would make this smoother

1. **npm-aware dependencies.** `{ npm: "@sigx/velt" }` would find the package in `node_modules`
   the way Node does, and use a `"velt"` field in its `package.json` to locate the Velt package
   inside. No hand-written `node_modules/...` path.
2. **Browser-only files.** A `package.vlt` field (`browser: ["src/client"]`) that Velt's tools
   skip, or at least parsing `import()` and `import.meta` so `velt fmt` and `velt check` can
   handle mixed folders.
3. **`velt dev --events json`.** Machine-readable status for the plugin and editors: `started`,
   `swapped {files}`, `restarted {reason}`, `failed {diagnostics}`, `listening {port}`, plus the
   list of files the build reads. The plugin then needs no text parsing and no `shared` option.
4. **The toolchain on npm.** Per-platform binaries with a small JS API (`dev()`, `build()`), so
   `pnpm install` installs a pinned `velt` and the plugin needs no PATH. Today a released
   toolchain is one download: dev (`velt dev`, JIT) needs nothing else, a deployed binary needs
   nothing at all, and only `velt build` needs the system linker (`cc` / Xcode command line
   tools / MSVC). Bundling a linker (lld) with the toolchain would remove that last step, which
   is mostly a Windows hurdle (Visual Studio Build Tools): #803.
5. **Embedded assets.** For example `import assets from "../dist/client" with { type: "dir" }`
   with a `velt:http` `serveStatic`, so production is one file. `sigx/server` has a small static
   server today.
6. **Render-local context** (#386 / #213). With it, `signal()` inside setup finds the render's
   runtime, so shared code needs no `ctx.signal`. Deep reactive objects (#385) are the largest
   remaining source difference.
7. **Clean `-o` output.** Intermediates go to `target/`, and only the executable goes to `-o`.
8. **Server functions from Velt signatures.** `velt check --exports json` (or emitted `.d.ts`)
   would give the plugin each function's name and types. It would then generate both the
   browser stubs and the TypeScript declarations, and the server's registration list, which
   today are a regex, a hand-written `.d.ts` and a list in `server.vlt`.
9. **Component directives** (`<Counter client:load />`), for sigx islands: a provider declares
   directive prefixes, and the compiler passes those attributes separately instead of as
   props (#815, used by `/islands`).
10. **Binding names for providers.** The provider could learn the name a value is bound to
    (`const count = ctx.signal(0)` gives `"count"`), for example through an opt-in parameter the
    compiler fills in. This is what sigx's island state and resume need, and it replaces a
    source transform.
