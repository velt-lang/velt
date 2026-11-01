// The TechEmpower-style server of bench/http/techempower on Bun.serve: the same routes, bodies
// and headers as server.mjs (node:http), server.vlt and the axum baseline.
// Usage: bun techempower.ts <port>. With REUSE_PORT=1 the socket is opened with SO_REUSEPORT, so
// several processes can share the port (bench/showcase starts one per server core).
const port = Number(process.argv[2] ?? 8080);

function loadFortunes() {
  return [
    { id: 1, message: "fortune: No such file or directory" },
    { id: 2, message: "A computer scientist is someone who fixes things that aren't broken." },
    { id: 3, message: "After enough decimal places, nobody gives a damn." },
    { id: 4, message: "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1" },
    { id: 5, message: "A computer program does what you tell it to do, not what you want it to do." },
    { id: 6, message: "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen" },
    { id: 7, message: "Any program that runs right is obsolete." },
    { id: 8, message: "A list is only as strong as its weakest link. — Donald Knuth" },
    { id: 9, message: "Feature: A bug with seniority." },
    { id: 10, message: "Computers make very fast, very accurate mistakes." },
    { id: 11, message: '<script>alert("This should not be displayed in a browser alert box.");</script>' },
    { id: 12, message: "フレームワークのベンチマーク" },
  ];
}

const escapes: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => escapes[c]);

function fortunesPage() {
  const fortunes = loadFortunes();
  fortunes.push({ id: 0, message: "Additional fortune added at request time." });
  fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
  let html =
    "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";
  for (const f of fortunes) {
    html += `<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`;
  }
  return html + "</table></body></html>";
}

Bun.serve({
  port,
  hostname: "127.0.0.1",
  reusePort: process.env.REUSE_PORT === "1",
  fetch(req) {
    const url = req.url;
    const path = url.slice(url.indexOf("/", 8));
    if (path === "/json") {
      return new Response(JSON.stringify({ message: "Hello, World!" }), {
        headers: { "content-type": "application/json" },
      });
    }
    if (path === "/fortunes") {
      return new Response(fortunesPage(), { headers: { "content-type": "text/html; charset=utf-8" } });
    }
    return new Response("Hello, World!", { headers: { "content-type": "text/plain; charset=utf-8" } });
  },
});
console.log(`listening on http://127.0.0.1:${port}`);
