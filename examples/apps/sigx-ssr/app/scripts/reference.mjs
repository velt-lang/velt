// Renders the shared App with JavaScript sigx (@sigx/server-renderer) through Vite's
// ssrLoadModule: the same .tsx files Velt compiles, with api.server.vlt's functions replaced by
// their JavaScript twins (api.reference.js).
//   node scripts/reference.mjs [path]            the app's HTML (renderToString, data awaited)
//   node scripts/reference.mjs --stream [path]   the whole streamed document, as sigx streams
//                                                it into index.html (renderDocumentToWebStream)
import { createServer } from "vite";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const stream = process.argv[2] === "--stream";
const path = process.argv[stream ? 3 : 2] ?? "/";
const vite = await createServer({
  root,
  configFile: false,
  logLevel: "error",
  appType: "custom",
  server: { middlewareMode: true, hmr: false },
  oxc: { jsx: { runtime: "automatic", importSource: "sigx" } },
  optimizeDeps: { noDiscovery: true },
  resolve: { dedupe: ["sigx", "@sigx/server-renderer"] },
  ssr: { noExternal: ["sigx", "@sigx/server-renderer", "@sigx/velt"] },
  plugins: [
    {
      name: "api-reference",
      resolveId: (source) => (/\.server$/.test(source) ? fileURLToPath(new URL("./api.reference.js", import.meta.url)) : null),
    },
  ],
});
try {
  const { App } = await vite.ssrLoadModule("/src/shared/App.tsx");
  const server = await vite.ssrLoadModule("@sigx/server-renderer/server");
  const { jsx } = await vite.ssrLoadModule("sigx/jsx-runtime");
  const app = jsx(App, { path });
  if (stream) {
    const template = readFileSync(`${root}/index.html`, "utf8");
    const body = server.renderDocumentToWebStream(app, { template });
    process.stdout.write(await new Response(body).text());
  } else {
    process.stdout.write(await server.renderToString(app));
  }
} finally {
  await vite.close();
}
