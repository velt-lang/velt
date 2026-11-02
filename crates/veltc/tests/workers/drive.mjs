// Drives a `velt build --host host-cloudflare` directory under node as Workers run it
// (crates/veltc/tests/workers.rs): `node drive.mjs <worker dir> <scenario>` imports the
// directory's `worker.mjs` (its `.wasm` import is the compiled module, as on Workers), sends the
// scenario's `Request`s to its `fetch` with a fake `env` and a `ctx` that records `waitUntil`,
// and prints the responses. Everything goes to stdout in the order it happens: the program's
// output, the glue's logs (`log: `) and what the scenario prints.
//
// Each request runs in a context of its own (an `AsyncLocalStorage` store), as on Workers, and
// what Workers forbid is printed (`context: …`): a `Response` made in another request's context,
// a request's body read from another, a timer outside any request, or one firing after its
// request ended (its response sent and its `waitUntil` promises settled).

import { AsyncLocalStorage } from "node:async_hooks";
import * as nodeModule from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// `import module from "./program.wasm"` gives the compiled module, as on Workers: a load hook,
// in-thread with `registerHooks` where node has it (node 26 deprecates `register`, DEP0205).
const wasmSource = (url) =>
  "import { readFileSync } from 'node:fs';\n" +
  `export default new WebAssembly.Module(readFileSync(new URL(${JSON.stringify(url)})));`;
if (typeof nodeModule.registerHooks === "function") {
  nodeModule.registerHooks({
    load(url, context, next) {
      if (!url.endsWith(".wasm")) return next(url, context);
      return { format: "module", shortCircuit: true, source: wasmSource(url) };
    },
  });
} else {
  const hooks = `export async function load(url, context, next) {
    if (!url.endsWith(".wasm")) return next(url, context);
    return { format: "module", shortCircuit: true, source: (${wasmSource})(url) };
  }`;
  nodeModule.register(`data:text/javascript,${encodeURIComponent(hooks)}`);
}

// A panic's location without the directory the test built in.
console.error = (...args) => console.log("log:", ...args.map((a) => String(a).replace(/ at \S*main\.vlt:/, " at main.vlt:")));
const contexts = new AsyncLocalStorage();
const ended = new Set();
const NativeResponse = Response;
globalThis.Response = class extends NativeResponse {
  constructor(...args) {
    super(...args);
    this.madeIn = contexts.getStore();
  }
};
const nativeSetTimeout = setTimeout;
globalThis.setTimeout = (f, ms, ...args) => {
  const owner = contexts.getStore();
  if (owner === undefined) console.log("context: a timer outside any request");
  return nativeSetTimeout(() => {
    if (ended.has(owner)) console.log(`context: a timer of request ${owner} fired after it ended`);
    f(...args);
  }, ms);
};
/** `body` (`what`'s), read only in request `id`'s context. */
function watchBody(body, id, what = `request ${id}'s body`) {
  const getReader = body.getReader.bind(body);
  body.getReader = (...args) => {
    const reader = getReader(...args);
    const read = reader.read.bind(reader);
    reader.read = () => {
      if (contexts.getStore() !== id) console.log(`context: ${what} read elsewhere`);
      return read();
    };
    return reader;
  };
  return body;
}

// The program's `fetch` of `https://upstream.test/…`: a response whose body must be read in the
// context of the request that fetched it.
const nativeFetch = globalThis.fetch;
globalThis.fetch = async (input, init) => {
  const url = String(input instanceof Request ? input.url : input);
  if (!url.startsWith("https://upstream.test/")) return nativeFetch(input, init);
  const id = contexts.getStore();
  const body = new ReadableStream({
    start(c) {
      for (const p of ["up", "stream"]) c.enqueue(encoder.encode(p));
      c.close();
    },
  });
  return new Response(watchBody(body, id, `the upstream response of request ${id}`));
};

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

let requests = 0;

/**
 * Fetch `path` from the Worker in a context of its own and print the response and how often
 * `waitUntil` was called.
 */
async function send(path, init = {}) {
  const id = ++requests;
  const waited = [];
  const ctx = { waitUntil: (p) => waited.push(p), passThroughOnException() {} };
  const request = new Request(`https://w.test/${path}`, init);
  if (request.body !== null) watchBody(request.body, id);
  const res = await contexts.run(id, () => worker.fetch(request, env, ctx));
  if (res.madeIn !== id) console.log(`context: ${path}'s response was made in request ${res.madeIn}`);
  const headers = [...res.headers].map(([k, v]) => `${k}=${v}`).join(" ");
  console.log(`${path}: ${res.status} ${headers} | ${await res.text()} | waitUntil ${waited.length}`);
  Promise.all(waited).then(() => ended.add(id));
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
    const queued = send("hello", { headers: { "x-name": "Eve" } });
    console.log("two requests wait while main does");
    env.gates.open("start");
    await first;
    await queued;

    const naps = [send("nap"), send("nap")];
    await Promise.all(naps);

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
  // relay/: a response fetched in one request's context, read from another's turn; timers go on
  // while a request's body waits for its client; a second answer leaves the instance alone.
  async relay() {
    const fetching = send("fetch-start");
    await env.gates.reached("fetch");
    await send("fetch-read");
    env.gates.open("fetch");
    await fetching;

    let release;
    const stalled = new Promise((resolve) => (release = resolve));
    let sent = false;
    const body = new ReadableStream({
      async pull(c) {
        if (!sent) {
          sent = true;
          c.enqueue(encoder.encode("abc"));
          return;
        }
        await stalled;
        c.close();
      },
    });
    const nap = send("nap");
    await new Promise((resolve) => nativeSetTimeout(resolve, 5));
    const upload = send("upload", { method: "POST", body, duplex: "half" });
    const first = await Promise.race([
      nap.then(() => "nap"),
      new Promise((resolve) => nativeSetTimeout(() => resolve("upload"), 2000)),
    ]);
    console.log(`the naps ended while the upload waited: ${first === "nap"}`);
    release();
    await nap;
    await upload;

    const twice = send("twice");
    await send("nothing");
    await twice;
  },
  // bare/: an accept loop over std alone.
  async bare() {
    await send("x");
  },
};

await scenarios[scenario]();
console.log(`instances ${instances}`);
