// JS glue for Velt programs built with `--host workers` (a Cloudflare Worker;
// docs/internals/design/cloudflare-workers.md). `velt build --host workers app.vlt` writes it next
// to `app.wasm`, with `worker.mjs` (`export default createWorker(module)`) and `wrangler.toml`.
//
// The module is instantiated once per isolate, on the first request: `velt_start()` runs `main`,
// whose `serve(...)` registers the handler (crates/velt_rt_wasm/src/workers.rs). Each request is
// then handed in with `velt_fetch(id)`; the handler reads the request and responds through the
// `velt_workers` imports below (std/http/server_io.workers.vlt), and `fetch_done(id, result)`
// says it finished. A panic or `exit()` discards the instance; the next request makes a new one.
//
// Imports from `velt`: write, exit, now_ms, date_ms (as in velt_web.mjs) and fetch_done. Strings
// and byte arrays cross as pointers to the runtime's values, read and written through the
// module's `velt_host_*` exports.

/** Thrown by the `exit` import to unwind out of the module. */
class VeltExit extends Error {
  constructor(code) {
    super(`exit ${code}`);
    this.code = code;
  }
}

/**
 * A Worker's handlers for a compiled Velt module.
 * @param {WebAssembly.Module} module the program
 * @param {{ write?: (stream: number, bytes: Uint8Array) => void }} host output sink (default:
 *   console.log / console.error, a line at a time)
 */
export function createWorker(module, host = {}) {
  let rt = null;
  return {
    async fetch(request, env, ctx) {
      try {
        rt ??= start(module, host.write ?? consoleWrite());
        return await rt.handle(request, env, ctx);
      } catch (e) {
        rt = null; // a panic, an exit or a trap: start afresh on the next request
        if (!(e instanceof VeltExit)) {
          console.error(e instanceof WebAssembly.RuntimeError ? `wasm trap: ${e.message}` : e);
        }
        return new Response("Internal Server Error", { status: 500 });
      }
    },
  };
}

function start(module, write) {
  const requests = new Map();
  let nextId = 1;
  let ex = null;
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  const memory = () => new Uint8Array(ex.memory.buffer);
  const str = (p) => {
    const ptr = ex.velt_host_str_ptr(p);
    const len = ex.velt_host_str_len(p);
    return decoder.decode(memory().subarray(ptr, ptr + len));
  };
  const bytes = (p) => {
    const ptr = ex.velt_host_bytes_ptr(p);
    const len = ex.velt_host_bytes_len(p);
    return memory().slice(ptr, ptr + len);
  };
  // Copies `data` into a buffer of the module's, hands it to `make(out, ptr, len)`, frees it.
  const pass = (out, data, make) => {
    const ptr = ex.velt_host_alloc(data.length);
    memory().set(data, ptr);
    make(out, ptr, data.length);
    ex.velt_host_free(ptr, data.length);
  };
  const setStr = (out, s) => pass(out, encoder.encode(s), ex.velt_host_str_new);
  const setBytes = (out, b) => pass(out, b, ex.velt_host_bytes_new);
  const req = (id) => {
    const r = requests.get(id);
    if (r === undefined) throw new Error(`ICE: unknown request ${id}`);
    return r;
  };

  const imports = {
    velt: {
      write(stream, ptr, len) {
        write(stream, memory().slice(ptr, ptr + len));
      },
      exit(code) {
        throw new VeltExit(code);
      },
      now_ms: () => performance.now(),
      date_ms: () => Date.now(),
      fetch_done(id, result) {
        req(id).done(result);
      },
    },
    velt_workers: {
      req_method: (id, out) => setStr(out, req(id).request.method),
      req_url: (id, out) => setStr(out, req(id).request.url),
      req_has_header: (id, name) => (req(id).request.headers.has(str(name)) ? 1 : 0),
      req_header: (id, name, out) => setStr(out, req(id).request.headers.get(str(name)) ?? ""),
      req_headers(id, out) {
        const flat = [];
        for (const [name, value] of req(id).request.headers) flat.push(name, value);
        setStr(out, flat.join("\n"));
      },
      req_has_body: (id) => (req(id).body !== null ? 1 : 0),
      req_take_body(id, out) {
        const r = req(id);
        setBytes(out, r.body ?? new Uint8Array(0));
        r.body = new Uint8Array(0);
      },
      req_drop(id) {
        const r = req(id);
        r.request = null;
        r.body = null;
      },
      respond(id, status, reason, headers, kind, text, body) {
        const fields = new Headers();
        const flat = str(headers);
        if (flat !== "") {
          const parts = flat.split("\n");
          for (let i = 0; i + 1 < parts.length; i += 2) fields.append(parts[i], parts[i + 1]);
        }
        const content = kind === 1 ? str(text) : kind === 2 ? bytes(body) : null;
        const statusText = str(reason);
        req(id).response = new Response(content, {
          status,
          headers: fields,
          ...(statusText === "" ? {} : { statusText }),
        });
      },
    },
  };

  const instance = new WebAssembly.Instance(module, imports);
  ex = instance.exports;
  const code = ex.velt_start();
  if (code !== 0) {
    throw new Error(`the program's main ended with exit code ${code}`);
  }

  return {
    async handle(request) {
      if (typeof ex.velt_fetch !== "function") {
        return new Response("this program does not serve: main never calls serve()", {
          status: 500,
        });
      }
      const id = nextId++;
      const body = request.body === null ? null : new Uint8Array(await request.arrayBuffer());
      let done;
      const finished = new Promise((resolve) => (done = resolve));
      const entry = { request, body, response: null, done };
      requests.set(id, entry);
      try {
        if (ex.velt_fetch(id) === 0) {
          return new Response("main did not call serve()", { status: 500 });
        }
        const result = await finished;
        return result === 1 && entry.response !== null
          ? entry.response
          : new Response("Internal Server Error", { status: 500 });
      } finally {
        requests.delete(id);
      }
    },
  };
}

/** console-based output: decodes UTF-8 per stream and logs whole lines. */
function consoleWrite() {
  const decoders = { 1: new TextDecoder(), 2: new TextDecoder() };
  const pending = { 1: "", 2: "" };
  return (stream, bytes) => {
    const text = pending[stream] + decoders[stream].decode(bytes, { stream: true });
    const lines = text.split("\n");
    pending[stream] = lines.pop();
    for (const line of lines) (stream === 2 ? console.error : console.log)(line);
  };
}
