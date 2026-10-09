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
  velt/src/*.vlt           component, reactivity, jsx-runtime (with precompile), server
  velt/tests/*.test.vlt    provider unit tests (expected strings taken from real sigx output)
app/                       what a user's app looks like
  package.json             sigx, @sigx/vite, @sigx/velt from npm
  package.vlt              dependencies: { sigx: { path: "node_modules/@sigx/velt/velt" } }
  vite.config.ts           plugins: [sigx(), velt()]
  index.html               <!--ssr-outlet--> and the client entry
  src/shared/*.tsx         components, compiled by Vite (JS sigx) and by Velt (the provider)
  src/client.tsx           hydrate
  src/entry-client.js      the browser entry (works around a sigx HMR ordering issue, below)
  src/server.vlt           the native server: serveApp(options(), (path) => <App path={path} />)
  scripts/reference.mjs    renders the same components with JavaScript sigx
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

## Develop, build, run

```sh
cd app && pnpm install         # needs `velt` on PATH, or VELT=/path/to/velt
pnpm dev                       # vite: http://localhost:5173
pnpm build                     # dist/client (Vite) + dist/server/app (velt build --release)
./dist/server/app --port 3000  # serves dist/client and renders the pages
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
| `src/client.tsx`, CSS | Vite HMR | not involved | nothing |
| `src/server.vlt`, the provider | Vite doesn't see it | hot-swap, or a restart when a layout or signature changed (the port is kept) | a full reload once Velt reports the swap |
| a Velt compile error | Vite's error overlay shows the diagnostics | the previous version keeps serving | clears the overlay once the error is fixed |

Between a save and Velt's status line, document requests wait (up to 2 s), so a refresh never
gets the old server's HTML.

## Checked by `scripts/e2e.mjs`

1. Velt's HTML for each page is byte-identical to JavaScript sigx's for the same `.tsx` files.
2. The production binary serves the client build, the page hydrates, two clicks give
   `Count: 3`, and the browser logs no warnings or errors.
3. Under `vite`:
   - editing a shared component hot-updates the browser without a reload, and the server renders
     the new markup;
   - a server-only edit reloads the page with the new HTML;
   - a compile error shows in the overlay while the old server keeps serving.

`velt test` in `sigx-velt/velt` runs the provider's unit tests, and `npx tsc -p .` in `app`
type-checks the shared components against JavaScript sigx. This example has no `package.vlt` at
its top level, so the gate's example-apps test skips it: it needs `pnpm install` first.

## What this found

**Velt**
- `typeof v === "number"` does not narrow `string | i64 | f64 | bool | (() => void) | null`; it
  does without the function member. The provider's `AttrValue` uses `f64` only until the fix is
  in.
- `arr.length` is `usize`, which is not a JSX child: `ctx.signal(xs.length)` needs
  `ctx.signal<number>(…)` in shared code. TypeScript sees `number`.
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
   is mostly a Windows hurdle (Visual Studio Build Tools).
5. **Embedded assets.** For example `import assets from "../dist/client" with { type: "dir" }`
   with a `velt:http` `serveStatic`, so production is one file. `sigx/server` has a small static
   server today.
6. **Render-local context** (#386 / #213). With it, `signal()` inside setup finds the render's
   runtime, so shared code needs no `ctx.signal`. Deep reactive objects (#385) are the largest
   remaining source difference.
7. **Clean `-o` output.** Intermediates go to `target/`, and only the executable goes to `-o`.
