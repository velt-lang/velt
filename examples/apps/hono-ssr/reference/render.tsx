// The same shop as a real Hono app on Node: the shared routes and components, rendered by
// `hono/jsx` and served with `c.html`. It prints what demo.velt prints from the Velt server, so
// `npm run reference | diff - demo.out` checks that both render the same bytes (README).

import { Hono } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import { document, route } from "../src/shared/pages";
import { PATHS } from "../src/shared/paths";

const app = new Hono();

app.get("*", async (c) => {
  const page = route(c.req.path, c.req.query("tag") ?? null);
  const html = await document(page).toString();
  return c.html("<!DOCTYPE html>" + html, page.status as ContentfulStatusCode);
});

async function main() {
  for (const path of PATHS) {
    const res = await app.request(path);
    console.log(`GET ${path} -> ${res.status} (${res.headers.get("content-type") ?? "-"})`);
    console.log(await res.text());
  }
}

main();
