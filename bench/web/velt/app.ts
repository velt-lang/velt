// TechEmpower Framework Benchmarks server on std/http + std/postgres: /json, /plaintext, /db,
// /queries?queries=N, /fortunes and /updates?queries=N (bench/web/README.md has the rules).
// Usage: server [port]; env PORT, HOST (127.0.0.1), DATABASE_URL, DB_POOL (default 2 × cores).
// The servers velt/server.vlt (a template literal) and velt-tsx/server.vlt (TSX) differ only in
// how they render the fortunes page: `startApp(render)`.

import { serve } from "velt/net/http";
import { Pool } from "velt/postgres";
import { args } from "velt/process";
import {
  Fortune,
  fortunesHtml,
  HTML,
  loadWorlds,
  Message,
  queryCount,
  updateWorlds,
  worldPool,
} from "./worlds";

export { Fortune } from "./worlds";

async function dbRoute<E>(
  pool: Pool,
  path: string,
  query: string,
  render: (fortunes: Fortune[]) => string throws E,
): Promise<Response> {
  if (path == "/db") {
    const worlds = await loadWorlds(pool, 1);
    return Response.json(worlds[0]);
  }
  if (path == "/queries") {
    return Response.json(await loadWorlds(pool, queryCount(query)));
  }
  if (path == "/updates") {
    return Response.json(await updateWorlds(pool, queryCount(query)));
  }
  if (path == "/fortunes") {
    return new Response(await fortunesHtml(pool, render), { headers: { "content-type": HTML } });
  }
  return new Response("not found", { status: 404 });
}

// Every response carries `Server` (hyper adds `Date`).
function named(res: Response): Response {
  res.headers.set("server", "velt");
  return res;
}

// The path and the query (without `?`) of `url`, an absolute URL as a server request has it
// (`http://host/path?query`): the path starts at the first `/` after the scheme (`https://` is 8
// characters).
function splitUrl(url: string): [string, string] {
  const start = url.indexOf("/", 8);
  const q = url.indexOf("?", start);
  return q < 0 ? [url.slice(start), ""] : [url.slice(start, q), url.slice(q + 1)];
}

function portArg(): number {
  const argv = args();
  const raw = argv.length > 0 ? argv[0] : process.env.PORT ?? "8080";
  return parseInt(raw, 10);
}

// Serves every route, rendering the fortunes page with `render`.
export async function startApp<E>(render: (fortunes: Fortune[]) => string throws E) {
  const pool = worldPool();
  const server = await serve({ port: portArg(), host: process.env.HOST ?? "127.0.0.1" }, async (
    req: Request,
  ): Promise<Response> => {
    const [path, query] = splitUrl(req.url);
    if (path == "/plaintext") {
      return named(new Response("Hello, World!"));
    }
    if (path == "/json") {
      return named(Response.json(new Message("Hello, World!")));
    }
    try {
      return named(await dbRoute(pool, path, query, render));
    } catch (e) {
      // `render`'s error type is the caller's, so the error is printed whole.
      console.log(`request failed: ${e}`);
      return named(new Response("internal error", { status: 500 }));
    }
  });
  console.log(`listening on http://127.0.0.1:${server.port}`);
}
