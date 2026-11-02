// Drives a `velt build --host workers` directory under node as Workers run it
// (crates/veltc/tests/workers.rs): `node drive.mjs <worker dir> <scenario>` imports the
// directory's `worker.mjs` (its `.wasm` import is the compiled module, as on Workers), sends the
// scenario's `Request`s to its `fetch` with a fake `env` and a `ctx` that records `waitUntil`,
// and prints the responses. Everything goes to stdout in the order it happens: the program's
// output, the glue's logs (`log: `) and what the scenario prints.

import { register } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// `import module from "./app.wasm"` gives the compiled module, as on Workers.
const hooks = `export async function load(url, context, next) {
  if (!url.endsWith(".wasm")) return next(url, context);
  const source = "import { readFileSync } from 'node:fs';\\n" +
    "export default new WebAssembly.Module(readFileSync(new URL(" + JSON.stringify(url) + ")));";
  return { format: "module", shortCircuit: true, source };
}`;
register(`data:text/javascript,${encodeURIComponent(hooks)}`);

// A panic's location without the directory the test built in.
console.error = (...args) => console.log("log:", ...args.map((a) => String(a).replace(/ at \S*main\.vlt:/, " at main.vlt:")));
let instances = 0;
const instantiate = WebAssembly.instantiate;
WebAssembly.instantiate = (...args) => {
  if (args[0] instanceof WebAssembly.Module) instances++;
  return instantiate(...args);
};

/** Gates the program waits at (probe's `gate`) and the scenario opens. */
class Gates {
  #open = new Map();
  #reached = new Map();
  /** The program's side: settles once the scenario opens `name`. */
  enter(name) {
    return new Promise((resolve) => {
      this.#open.set(name, resolve);
      this.#reached.get(name)?.();
      this.#reached.delete(name);
    });
  }
  /** Settles once the program waits at `name`. */
  reached(name) {
    if (this.#open.has(name)) return Promise.resolve();
    return new Promise((resolve) => this.#reached.set(name, resolve));
  }
  open(name) {
    const resolve = this.#open.get(name);
    this.#open.delete(name);
    resolve();
  }
}

const [dir, scenario] = process.argv.slice(2);
const worker = (await import(pathToFileURL(join(dir, "worker.mjs")).href)).default;
const env = { GREETING: "Hello", gates: new Gates() };
const encoder = new TextEncoder();

/** Whether the promises `waitUntil` got have all settled, once what is pending now has run. */
function settled(waited) {
  const all = Promise.all(waited).then(() => true);
  return Promise.race([all, new Promise((resolve) => setImmediate(() => resolve(false)))]);
}

/** Fetch `path` from the Worker and print the response and how often `waitUntil` was called. */
async function send(path, init = {}) {
  const waited = [];
  const ctx = { waitUntil: (p) => waited.push(p), passThroughOnException() {} };
  const res = await worker.fetch(new Request(`https://w.test/${path}`, init), env, ctx);
  const headers = [...res.headers].map(([k, v]) => `${k}=${v}`).join(" ");
  console.log(`${path}: ${res.status} ${headers} | ${await res.text()} | waitUntil ${waited.length}`);
  return waited;
}

const scenarios = {
  // hello.vlt: `main` returns without accepting.
  async never() {
    await send("a");
    await send("b");
  },
  // serve/: an accept loop after a gate, requests at once, bodies both ways, work after the
  // answer, a dropped request and a panic.
  async serve() {
    const first = send("hello", { headers: { "x-name": "Ada" } });
    await env.gates.reached("start");
    console.log("the first request waits while main does");
    env.gates.open("start");
    await first;

    const slow = send("slow");
    await env.gates.reached("slow");
    await send("hello", { method: "POST", headers: { "x-name": "Bo" } });
    env.gates.open("slow");
    await slow;

    const pieces = ["a", "", "bc", "d"];
    const body = new ReadableStream({
      start(c) {
        for (const p of pieces) c.enqueue(encoder.encode(p));
        c.close();
      },
    });
    await send("echo", { method: "POST", body, duplex: "half" });
    await send("echo", { method: "POST", body: "plain" });
    await send("stream");
    await send("nothing");

    const later = await send("later");
    await env.gates.reached("later");
    console.log(`waitUntil settled before the work was done: ${await settled(later)}`);
    env.gates.open("later");
    await Promise.all(later);
    console.log("waitUntil settled");
    const timer = await send("timer");
    await Promise.all(timer);
    console.log("waitUntil settled");

    await send("drop");

    const inFlight = send("slow");
    await env.gates.reached("slow");
    await send("panic");
    await inFlight;

    const again = send("hello", { headers: { "x-name": "Cy" } });
    await env.gates.reached("start");
    env.gates.open("start");
    await again;
  },
};

await scenarios[scenario]();
console.log(`instances ${instances}`);
