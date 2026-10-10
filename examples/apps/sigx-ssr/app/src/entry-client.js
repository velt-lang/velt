// The browser entry: hydrates the server-rendered app. Plain JavaScript because Velt compiles
// .ts/.tsx files and does not parse `import.meta` or `import()`; its tools skip .js files.
//
// In dev it first installs @sigx/vite's HMR hook: @sigx/vite registers it asynchronously, after
// the first module graph has run, so components defined in that graph would never hot-update
// (a sigx-side fix belongs in @sigx/vite). The app is imported only once the hook is in place.
import { defineApp } from "sigx";
import { jsx } from "sigx/jsx-runtime";
import { ssrClientPlugin } from "@sigx/server-renderer/client";

if (import.meta.hot) {
  await (await import("@sigx/vite/hmr")).installHMRPlugin();
}
if (window.__SIGX_BOUNDARIES__) {
  // An islands page: only its islands hydrate, each loaded by name when its directive fires.
  await import("virtual:sigx-islands");
  const { hydrateIslands } = await import("@sigx/ssr-islands/client");
  await hydrateIslands();
} else {
  const { App } = await import("./shared/App.tsx");
  await defineApp(jsx(App, { path: window.location.pathname })).use(ssrClientPlugin).hydrate("#app");
}
// For tests: interactive from here on.
document.documentElement.dataset.hydrated = "";
