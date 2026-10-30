// server.vlt on React or Preact: `/small` and `/large` render page.mjs's tree per request.
// Under Node it serves with node:http, under Bun with Bun.serve.
//   node server.mjs <react|preact> <port>      bun server.mjs <react|preact> <port>
// With CLUSTER=1 Node forks one worker per core (node:cluster, a shared listening socket), as
// bench/http/techempower/server.mjs does; with REUSE_PORT=1 Bun opens the port with
// SO_REUSEPORT, so several processes can share it (bench/showcase starts one per server core).
import cluster from "node:cluster";
import http from "node:http";
import os from "node:os";
import { items, load } from "./page.mjs";

const lib = process.argv[2];
const port = Number(process.argv[3] ?? 8080);

if (!globalThis.Bun && process.env.CLUSTER === "1" && cluster.isPrimary) {
  for (let i = 0; i < os.availableParallelism(); i++) {
    cluster.fork();
  }
  // bench/showcase stops the server with SIGTERM to its process group; take the workers down too.
  process.on("SIGTERM", () => {
    for (const w of Object.values(cluster.workers)) w.kill();
    process.exit(0);
  });
} else {
  const { render } = await load(lib);
  const SMALL = items(10);
  const LARGE = items(1000);

  const page = (path) => {
    if (path === "/small") return `<!DOCTYPE html>${render("small page", SMALL)}`;
    if (path === "/large") return `<!DOCTYPE html>${render("large page", LARGE)}`;
    return null;
  };

  if (globalThis.Bun) {
    Bun.serve({
      port,
      hostname: "127.0.0.1",
      reusePort: process.env.REUSE_PORT === "1",
      fetch(req) {
        const body = page(req.url.slice(req.url.indexOf("/", 8)));
        return body === null
          ? new Response("ok")
          : new Response(body, { headers: { "content-type": "text/html; charset=utf-8" } });
      },
    });
  } else {
    http
      .createServer((req, res) => {
        const body = page(req.url);
        if (body === null) {
          res.end("ok");
        } else {
          res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
          res.end(body);
        }
      })
      .listen(port, "127.0.0.1");
  }
  console.log(`listening on http://127.0.0.1:${port}`);
}
