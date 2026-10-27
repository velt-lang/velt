// The requests for async.vlt, and the upstream its own requests go to.
export const requests = () =>
  ["/sleep", "/proxy", "/post", "/chunks", "/down", "/stream", "/missing"].map(
    (p) => new Request(`https://example.com${p}`),
  );

export async function fetch(url, init = {}) {
  const u = new URL(url);
  if (u.hostname === "down.example") throw new TypeError("connection refused");
  if (u.pathname === "/items/7") {
    const from = new Headers(init.headers).get("x-from");
    return Response.json({ id: 7, name: `widget for ${from}` }, { headers: { "x-upstream": "yes" } });
  }
  if (u.pathname === "/echo") return new Response(`${init.method} ${init.body}`);
  if (u.pathname === "/chunks") {
    const enc = new TextEncoder();
    const body = new ReadableStream({
      start(c) {
        for (const s of ["ab", "cde", "f"]) c.enqueue(enc.encode(s));
        c.close();
      },
    });
    return new Response(body);
  }
  return new Response("no such upstream path", { status: 404 });
}
