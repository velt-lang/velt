// Runs src/main.vlt's build against a local HTTP server and prints its output and exit code:
// `node drive.mjs <velt_web.mjs> <main.wasm>` on a JS host (the glue's `velt:fetch` over node's
// `fetch`, with the server's URL as the host function `test.base`), or `node drive.mjs - <exe>`
// natively (the URL in `FETCH_BASE`). No external network: the server listens on 127.0.0.1.
//
// The program waits on the server's answers, never on time: `/arrived?id=…` answers once the
// request `id` arrived, `/closed?id=…` once its connection closed (the client cancelled it), and
// `/stream` sends the rest of its body once `/release` was asked for.

import http from "node:http";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { writeSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [glue, program] = process.argv.slice(2);

/** A promise per (kind, id), made by whichever side asks first. */
const events = new Map();
const event = (key) => {
  if (!events.has(key)) {
    let resolve;
    const promise = new Promise((r) => (resolve = r));
    events.set(key, { promise, resolve });
  }
  return events.get(key);
};

const BIG = Uint8Array.from({ length: 300000 }, (_, i) => i % 251);

const routes = {
  "/text": (req, res) => {
    res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
    res.end("hello from node");
  },
  "/json": (req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ name: "velt", n: [1, 2, 3] }));
  },
  "/missing": (req, res) => {
    res.writeHead(404);
    res.end("no such page");
  },
  "/big": async (req, res) => {
    res.writeHead(200, { "content-type": "application/octet-stream" });
    for (let at = 0; at < BIG.length; at += 30000) {
      if (!res.write(BIG.subarray(at, at + 30000))) await new Promise((r) => res.once("drain", r));
    }
    res.end();
  },
  // A body that says it is gzip and isn't: decoding it fails.
  "/badgzip": (req, res) => {
    res.writeHead(200, { "content-encoding": "gzip" });
    res.end("this is not gzip");
  },
  // The first piece of a body, then the connection is reset.
  "/reset": (req, res) => {
    res.writeHead(200, { "content-type": "text/plain", "content-length": "1000" });
    res.write("only this;", () => req.socket.destroy());
  },
  // The bytes of the request's `x-word` header, and a header of the server's own (node writes
  // its value as UTF-8).
  "/word": (req, res) => {
    const sent = Buffer.from(req.headers["x-word"] ?? "", "latin1");
    res.setHeader("x-reply", "då");
    res.end(sent.toString("hex"));
  },
  "/redirect": (req, res) => {
    res.writeHead(302, { location: "/text" });
    res.end("moved");
  },
  "/echo": async (req, res) => {
    const parts = [];
    for await (const part of req) parts.push(part);
    const body = Buffer.concat(parts);
    const type = req.headers["content-type"] ?? "";
    res.writeHead(200, {
      "content-type": "application/json",
      "x-two": "second",
      "set-cookie": ["a=1", "b=2"],
    });
    res.end(
      JSON.stringify({
        method: req.method,
        one: req.headers["x-one"] ?? "",
        type,
        length: body.length,
        body: type === "application/octet-stream" ? body.toString("hex") : body.toString("utf8"),
      }),
    );
  },
  "/stream": async (req, res) => {
    res.writeHead(200, { "content-type": "text/plain" });
    res.write("first;");
    // timing: a guard that must never fire. A client that waits for the whole body before
    // giving the first piece never asks for `/release`; the body then ends with "guard;" so the
    // output differs instead of the test hanging.
    const guard = setTimeout(() => event("release").resolve("guard"), 30000);
    const why = await event("release").promise;
    clearTimeout(guard);
    if (why === "guard") return res.end("guard;");
    for (let i = 0; i < 4; i++) {
      if (!res.write("x".repeat(50000))) await new Promise((r) => res.once("drain", r));
    }
    res.end();
  },
  "/release": (req, res) => {
    event("release").resolve("released");
    res.end("ok");
  },
  // Never answered: the client aborts it.
  "/hang": (req, res, id) => {
    res.on("close", () => event(`closed:${id}`).resolve());
    event(`arrived:${id}`).resolve();
  },
  // The first piece of a body whose rest never comes: the client aborts it.
  "/slow": (req, res, id) => {
    res.on("close", () => event(`closed:${id}`).resolve());
    res.writeHead(200, { "content-type": "text/plain" });
    res.write("part one;");
  },
  "/arrived": async (req, res, id) => {
    await event(`arrived:${id}`).promise;
    res.end("arrived");
  },
  "/closed": async (req, res, id) => {
    await event(`closed:${id}`).promise;
    res.end("the request's connection closed");
  },
  "/refused-port": async (req, res) => res.end(String(await refusedPort())),
};

/** A port nothing listens on: one a server had, closed again. */
async function refusedPort() {
  const s = http.createServer();
  await new Promise((r) => s.listen(0, "127.0.0.1", r));
  const { port } = s.address();
  await new Promise((r) => s.close(r));
  return port;
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  const route = routes[url.pathname];
  if (!route) {
    res.writeHead(500);
    return res.end(`no route ${url.pathname}`);
  }
  route(req, res, url.searchParams.get("id"));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

// timing: a guard that must never fire, so a hang fails the test with a message.
const guard = setTimeout(() => {
  console.log("guard: the program did not end within 60 s");
  process.exit(1);
}, 60000);

let code;
if (glue === "-") {
  const child = spawn(program, [], { env: { ...process.env, FETCH_BASE: base }, stdio: "inherit" });
  code = await new Promise((r) => child.on("exit", (c) => r(c)));
} else {
  const { runVelt, HostRefs } = await import(pathToFileURL(glue).href);
  const refs = new HostRefs();
  const write = (stream, data) => writeSync(stream, data);
  code = await runVelt(await readFile(program), { write, refs, imports: { test: { base: () => base } } });
  // The program's module variable `kept` holds the one response it never read; anything else
  // that is still held leaked.
  if (refs.values.size !== 1) console.log(`${refs.values.size} HostRefs held at the end, not 1`);
}
clearTimeout(guard);
server.closeAllConnections();
server.close();
console.log(`exit ${code}`);
