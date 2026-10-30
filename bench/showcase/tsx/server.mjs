// server.vlt on React or Preact: `/small` and `/large` render page.mjs's tree per request.
// Under Node it serves with node:http, under Bun with Bun.serve.
//   node server.mjs <react|preact> <port>      bun server.mjs <react|preact> <port>
import http from "node:http";
import { items, load } from "./page.mjs";

const { render } = await load(process.argv[2]);
const port = Number(process.argv[3] ?? 8080);
const SMALL = items(10);
const LARGE = items(1000);

function page(path) {
  if (path === "/small") return `<!DOCTYPE html>${render("small page", SMALL)}`;
  if (path === "/large") return `<!DOCTYPE html>${render("large page", LARGE)}`;
  return null;
}

if (globalThis.Bun) {
  Bun.serve({
    port,
    hostname: "127.0.0.1",
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
