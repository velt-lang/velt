// Renders the shared App with JavaScript sigx (@sigx/server-renderer) through Vite's
// ssrLoadModule, the same .tsx files Velt compiles. Prints the app's HTML (what goes in place of
// <!--ssr-outlet-->). Usage: node scripts/reference.mjs [path]
import { createServer } from "vite";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const path = process.argv[2] ?? "/";
const vite = await createServer({
  root,
  configFile: false,
  logLevel: "error",
  appType: "custom",
  server: { middlewareMode: true, hmr: false },
  oxc: { jsx: { runtime: "automatic", importSource: "sigx" } },
  optimizeDeps: { noDiscovery: true },
  ssr: { noExternal: ["sigx", "@sigx/server-renderer"] },
});
try {
  const { App } = await vite.ssrLoadModule("/src/shared/App.tsx");
  const { renderToString } = await vite.ssrLoadModule("@sigx/server-renderer/server");
  const { jsx } = await vite.ssrLoadModule("sigx/jsx-runtime");
  process.stdout.write(await renderToString(jsx(App, { path })));
} finally {
  await vite.close();
}
