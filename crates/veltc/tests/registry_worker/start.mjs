// Starts the registry's Worker under workerd with its real bindings and the migrations its build
// scaffolded applied, the reserved names among them (crates/veltc/tests/registry_worker.rs).

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/**
 * The Worker of `dir` under the Miniflare in `modules`, with the text bindings `vars`; requests it
 * sends out reach `outbound`.
 */
export async function start(modules, dir, vars = {}, outbound = undefined) {
  const miniflare = pathToFileURL(join(modules, "miniflare/dist/src/index.js")).href;
  const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = await import(miniflare);

  const toml = readFileSync(join(dir, "wrangler.toml"), "utf8");
  const date = /^compatibility_date\s*=\s*"([^"]+)"/m.exec(toml)[1];
  const list = /^compatibility_flags\s*=\s*\[([^\]]*)\]/m.exec(toml)?.[1] ?? "";
  const flags = [...list.matchAll(/"([^"]+)"/g)].map((m) => m[1]);

  const walk = (at) =>
    readdirSync(at, { withFileTypes: true }).flatMap((e) => {
      const path = join(at, e.name);
      return e.isDirectory() ? walk(path) : /\.(mjs|wasm)$/.test(e.name) ? [path] : [];
    });
  const entry = join(dir, "worker.mjs");
  const files = [entry, ...walk(dir).filter((f) => f !== entry).sort()];

  const mf = new Miniflare(convertV4MiniflareOptions({
    log: new Log(LogLevel.WARN),
    cf: false,
    workers: [{
      name: "registry",
      compatibilityDate: date,
      compatibilityFlags: flags,
      modulesRoot: dir,
      modules: files.map((path) => ({
        type: path.endsWith(".wasm") ? "CompiledWasm" : "ESModule",
        path,
      })),
      bindings: vars,
      outboundService: outbound,
      r2Buckets: ["BLOBS"],
      d1Databases: ["DB"],
    }],
  }));
  const url = await mf.ready;

  // Every migration, in order, as `wrangler d1 migrations apply` does.
  const db = await mf.getD1Database("DB");
  const migrations = readdirSync(join(dir, "migrations")).filter((f) => f.endsWith(".sql")).sort();
  for (const file of migrations) {
    const sql = readFileSync(join(dir, "migrations", file), "utf8");
    const statements = sql.replace(/--.*$/gm, "").split(";").map((s) => s.trim()).filter(Boolean);
    for (const statement of statements) await db.prepare(statement).run();
  }
  return { mf, url };
}
