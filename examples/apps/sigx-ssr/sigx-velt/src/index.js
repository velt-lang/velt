// @sigx/velt: the Vite plugin. Vite keeps the browser side (modules, HMR, the client build);
// a Velt program renders the documents.
//
//   dev    `vite`        starts `velt dev` (hot-swapping native server), proxies document requests
//                         to it and runs the HTML through Vite's transformIndexHtml
//   build  `vite build`  builds the client into dist/client, then `velt build --release` into
//                         dist/server/app
//   run    dist/server/app serves dist/client and renders every page
//
// Plain JavaScript with types in index.d.ts, so the package needs no build step (Node will not
// strip types inside node_modules).

import { spawn, execFileSync } from "node:child_process";
import { createServer as createNetServer } from "node:net";
import { realpathSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { resolve, relative, sep, dirname } from "node:path";

const STATUS = /^velt dev: (started|hot-swapped|restarted|build failed|program exited|exited)/;
const LISTENING = /^sigx: listening on /;

/** A free TCP port on 127.0.0.1. */
function freePort() {
  return new Promise((ok, fail) => {
    const s = createNetServer();
    s.unref();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
}

function missingVelt(bin) {
  return new Error(
    `@sigx/velt: the Velt toolchain was not found (\`${bin}\`). Install it with\n` +
      `  curl -fsSL https://raw.githubusercontent.com/velt-lang/velt/main/scripts/get-velt.sh | sh\n` +
      `or point the plugin at it: velt({ bin: "/path/to/velt" }) or VELT=/path/to/velt.`,
  );
}

/** Document requests: page navigations, not Vite's modules or files. */
function isDocument(req) {
  if (req.method !== "GET" && req.method !== "HEAD") return false;
  if (!(req.headers.accept ?? "").includes("text/html")) return false;
  const path = (req.url ?? "/").split("?")[0];
  if (/^\/(@|src\/|node_modules\/|__)/.test(path)) return false;
  const last = path.slice(path.lastIndexOf("/") + 1);
  return !last.includes(".") || last.endsWith(".html");
}

// Server functions: a `*.server.vlt` module's `export async function`s are callable from the
// browser. An import of it from browser code becomes this stub module (sigx's own client,
// `POST /_sigx/fn/<key>`); the Velt server registers the same keys (`serverFn` in sigx/server).
const STUB_PREFIX = "\0sigx-velt-fns:";
const EXPORTED_FN = /^export\s+async\s+function\s+([A-Za-z_$][\w$]*)/gm;

function serverFnStubs(file, root) {
  const module = relative(root, file).replace(/\\/g, "/").replace(/\.vlt$/, "");
  const names = [...readFileSync(file, "utf8").matchAll(EXPORTED_FN)].map((m) => m[1]);
  return [
    `import { __serverFnStub } from "@sigx/server/client";`,
    ...names.map((n) => `export const ${n} = __serverFnStub(${JSON.stringify(`${module}/${n}`)}, ${JSON.stringify(n)}, "/_sigx/fn", "");`),
  ].join("\n");
}

/**
 * @param {import("./index").VeltOptions} [opts]
 * @returns {import("vite").Plugin}
 */
export default function velt(opts = {}) {
  const bin = opts.bin ?? process.env.VELT ?? "velt";
  const shared = opts.shared ?? ["src/shared"];
  const holdMs = opts.holdMs ?? 2000;
  let root = process.cwd();
  let outDir = "dist/client";

  return {
    name: "sigx-velt",

    config(user) {
      // The client build goes to dist/client, the server binary next to it in dist/server.
      if (!user.build?.outDir) return { build: { outDir: "dist/client" } };
    },

    configResolved(config) {
      root = config.root;
      outDir = config.build.outDir;
    },

    resolveId(source, importer) {
      if (!importer || !/\.server(\.vlt)?$/.test(source) || !source.startsWith(".")) return null;
      const file = resolve(dirname(importer.replace(/[?#].*$/, "")), source.replace(/\.vlt$/, "") + ".vlt");
      return existsSync(file) ? STUB_PREFIX + file : null;
    },

    load(id) {
      if (!id.startsWith(STUB_PREFIX)) return null;
      const file = id.slice(STUB_PREFIX.length);
      this.addWatchFile(file);
      return serverFnStubs(file, root);
    },

    async configureServer(server) {
      const port = await freePort();
      const log = server.config.logger;
      const args = ["dev", ...(opts.entry ? [opts.entry] : []), "--", "--dev", "--port", String(port), "--template", "index.html"];
      const child = spawn(bin, args, { cwd: root, stdio: ["ignore", "pipe", "pipe"] });

      // The server is "settled" when it has answered the last change. Document requests wait for
      // that (up to holdMs), so a refresh right after a save never gets the old server's HTML.
      let settled = null; // null: settled; else { promise, resolve }
      const unsettle = () => {
        if (settled) return;
        let done;
        const promise = new Promise((r) => (done = r));
        settled = { promise, resolve: done };
      };
      const settle = () => {
        settled?.resolve();
        settled = null;
      };
      unsettle(); // until the first "listening"

      let output = []; // lines since the last change (the diagnostics of a failed build)
      let changed = null; // the file whose change velt dev is building
      let overlay = false;

      const onLine = (line) => {
        if (line.trim() !== "") log.info(`[velt] ${line}`);
        if (LISTENING.test(line)) {
          settle();
          return;
        }
        const m = STATUS.exec(line);
        if (!m) {
          output.push(line);
          return;
        }
        const what = m[1];
        if (what === "build failed") {
          overlay = true;
          server.ws.send({
            type: "error",
            err: { message: output.join("\n").trim() || line, stack: "", plugin: "sigx-velt", id: changed ?? undefined },
          });
          settle(); // the previous version keeps serving
        } else if (what === "hot-swapped" || what === "restarted") {
          // A file Vite also serves updates the browser through Vite's HMR; anything else (server
          // code, the provider) changes only what the server renders: reload the page.
          const clientModule = changed && server.moduleGraph.getModulesByFile(changed)?.size;
          if (overlay || !clientModule) server.ws.send({ type: "full-reload" });
          overlay = false;
          if (what === "hot-swapped") settle(); // a restart settles on its "listening" line
        }
        output = [];
        changed = null;
      };
      for (const stream of [child.stdout, child.stderr]) {
        let buf = "";
        stream.setEncoding("utf8");
        stream.on("data", (chunk) => {
          buf += chunk;
          let nl;
          while ((nl = buf.indexOf("\n")) >= 0) {
            onLine(buf.slice(0, nl).replace(/\r$/, ""));
            buf = buf.slice(nl + 1);
          }
        });
      }
      child.on("error", (e) => {
        log.error(e.code === "ENOENT" ? missingVelt(bin).message : String(e));
        settle();
      });
      server.httpServer?.on("close", () => child.kill("SIGTERM"));

      // Which saves velt dev rebuilds for: Velt sources and the shared component folders. (A
      // machine-readable `velt dev` event stream would name the files it watches instead.)
      const provider = existsSync(resolve(root, "node_modules/@sigx/velt/velt"))
        ? realpathSync(resolve(root, "node_modules/@sigx/velt/velt"))
        : null;
      if (provider) server.watcher.add(provider);
      const isServerFile = (file) =>
        file.endsWith(".vlt") ||
        (provider && file.startsWith(provider + sep)) ||
        shared.some((d) => !relative(resolve(root, d), file).startsWith(".."));
      server.watcher.on("change", (file) => {
        if (!isServerFile(file)) return;
        changed = file;
        output = [];
        unsettle();
      });

      // Server-function calls go to the Velt server as they are.
      server.middlewares.use(async (req, res, next) => {
        if (!(req.url ?? "").startsWith("/_sigx/fn/")) return next();
        try {
          if (settled) await Promise.race([settled.promise, new Promise((r) => setTimeout(r, holdMs))]);
          const chunks = [];
          for await (const c of req) chunks.push(c);
          const r = await fetch(`http://127.0.0.1:${port}${req.url}`, {
            method: req.method,
            headers: { "content-type": req.headers["content-type"] ?? "", origin: req.headers.origin ?? "" },
            body: req.method === "GET" || req.method === "HEAD" ? undefined : Buffer.concat(chunks),
          });
          res.statusCode = r.status;
          res.setHeader("content-type", r.headers.get("content-type") ?? "application/json");
          res.end(Buffer.from(await r.arrayBuffer()));
        } catch (e) {
          next(e);
        }
      });

      server.middlewares.use(async (req, res, next) => {
        if (!isDocument(req)) return next();
        try {
          if (settled) await Promise.race([settled.promise, new Promise((r) => setTimeout(r, holdMs))]);
          const r = await fetch(`http://127.0.0.1:${port}${req.url}`, {
            headers: { accept: req.headers.accept ?? "text/html" },
          });
          const html = await server.transformIndexHtml(req.url, await r.text(), req.originalUrl);
          res.statusCode = r.status;
          res.setHeader("content-type", "text/html; charset=utf-8");
          res.end(html);
        } catch (e) {
          next(e);
        }
      });
    },

    // After the client build: compile the server next to it.
    closeBundle: {
      sequential: true,
      handler() {
        if (opts.build === false || this.environment?.name !== "client") return;
        const out = resolve(root, opts.serverOutFile ?? "dist/server/app");
        mkdirSync(resolve(out, ".."), { recursive: true });
        const args = ["build", ...(opts.entry ? [opts.entry] : []), ...(opts.release === false ? [] : ["--release"]), "-o", out];
        try {
          execFileSync(bin, args, { cwd: root, stdio: "inherit" });
        } catch (e) {
          throw e.code === "ENOENT" ? missingVelt(bin) : new Error(`@sigx/velt: velt ${args.join(" ")} failed`);
        }
        this.environment.logger.info(
          `\n@sigx/velt: server built: ${relative(root, out)}\n  run: ${relative(root, out)} --static ${relative(root, resolve(root, outDir))} --template ${relative(root, resolve(root, outDir, "index.html"))} --port 3000`,
        );
      },
    },
  };
}
