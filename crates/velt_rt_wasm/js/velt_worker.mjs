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
// Imports from `velt`: write, exit, now_ms, date_ms (as in velt_web.mjs), fetch_done and
// set_timer. Strings and byte arrays cross as pointers to the runtime's values, read and written
// through the module's `velt_host_*` exports. An asynchronous import returns the future
// `velt_host_promise(id)` makes, and the JS promise behind it settles it with
// `velt_settle(id, value)` (crates/velt_rt_wasm/src/host_promise.rs).

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
 * @param {{
 *   write?: (stream: number, bytes: Uint8Array) => void,
 *   fetch?: typeof fetch,
 *   caches?: CacheStorage,
 * }} host output sink (default: console.log / console.error, a line at a time), the `fetch`
 *   the program's own requests go to and the caches (default: the global ones)
 */
export function createWorker(module, host = {}) {
  let rt = null;
  return {
    async fetch(request, env, ctx) {
      try {
        if (rt === null || rt.dead) {
          rt = start(module, {
            write: host.write ?? consoleWrite(),
            outbound: host.fetch ?? globalThis.fetch,
            caches: host.caches ?? globalThis.caches,
            env,
          });
        }
        return await rt.handle(request, env, ctx);
      } catch (e) {
        if (rt !== null) rt.fail(e);
        return new Response("Internal Server Error", { status: 500 });
      }
    },
  };
}

function start(module, { write, outbound, caches, env }) {
  const requests = new Map();
  // Fetched responses and streamed responses, by handle (above 1: a handler's result of 1
  // means "responded", 0 "failed").
  const handles = new Map();
  let nextRequest = 1;
  let nextHandle = 2;
  let nextPromise = 1;
  let timer = null;
  let ex = null;
  const state = { dead: false };
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
  const flat = (headers) => {
    const out = [];
    for (const [name, value] of headers) out.push(name, value);
    return out.join("\n");
  };
  const unflat = (text) => {
    const fields = new Headers();
    if (text !== "") {
      const parts = text.split("\n");
      for (let i = 0; i + 1 < parts.length; i += 2) fields.append(parts[i], parts[i + 1]);
    }
    return fields;
  };
  const req = (id) => {
    const r = requests.get(id);
    if (r === undefined) throw new Error(`ICE: unknown request ${id}`);
    return r;
  };
  const handle = (h) => {
    const v = handles.get(h);
    if (v === undefined) throw new Error(`ICE: unknown handle ${h}`);
    return v;
  };
  const message = (e) => String(e?.message ?? e);
  // Binding handles by name: a binding is the same object for every request of an isolate.
  const bindings = new Map();
  // An operation on a binding (std/cloudflare.vlt): settles with a result handle whose `error`,
  // `text` (JSON) and `bytes` the program reads.
  const op = (run) => {
    const h = nextHandle++;
    const r = { error: "", text: "null", bytes: null };
    handles.set(h, r);
    return promise(
      Promise.resolve()
        .then(() => run(r))
        .then(
          (v) => {
            if (v !== undefined) r.text = JSON.stringify(v);
            return h;
          },
          (e) => {
            r.error = message(e);
            return h;
          },
        ),
    );
  };
  const bound = (h) => handle(h).binding;
  const r2meta = (o) =>
    o === null
      ? null
      : {
          key: o.key,
          size: o.size,
          etag: o.etag,
          httpEtag: o.httpEtag,
          uploaded: o.uploaded.getTime(),
          contentType: o.httpMetadata?.contentType ?? "",
        };

  // A panic, an exit or a trap: every request in flight fails and the instance is not used again.
  const fail = (e) => {
    if (state.dead) return;
    state.dead = true;
    if (timer !== null) clearTimeout(timer);
    if (!(e instanceof VeltExit)) {
      console.error(e instanceof WebAssembly.RuntimeError ? `wasm trap: ${e.message}` : e);
    }
    for (const r of requests.values()) r.done(0);
    for (const h of handles.values()) h.writer?.abort(new Error("the program failed")).catch(() => {});
  };
  // Calls into the module from a callback (a settled promise, a timer).
  const enter = (f) => {
    if (state.dead) return;
    try {
      f();
    } catch (e) {
      fail(e);
    }
  };
  // An asynchronous import's result: the future the module waits on, settled by `p` (whose
  // value is a u32; a rejection must be turned into a value by the caller).
  const promise = (p) => {
    const id = nextPromise++;
    const fut = ex.velt_host_promise(id);
    p.then((v) => enter(() => ex.velt_settle(id, v >>> 0)));
    return fut;
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
        const r = req(id);
        if (result > 1) {
          // A streamed response: its handle stays until the program closes the stream.
          r.response = handle(result).response;
          r.done(1);
        } else {
          r.done(result);
        }
      },
      set_timer(ms) {
        if (timer !== null) clearTimeout(timer);
        // Rounded up: hosts round a delay down to whole milliseconds.
        timer = setTimeout(
          () => {
            timer = null;
            enter(() => ex.velt_turn());
          },
          Math.ceil(ms),
        );
      },
    },
    velt_workers: {
      // The request a handler runs for (std/http/server_io.workers.vlt).
      req_method: (id, out) => setStr(out, req(id).request.method),
      req_url: (id, out) => setStr(out, req(id).request.url),
      req_has_header: (id, name) => (req(id).request.headers.has(str(name)) ? 1 : 0),
      req_header: (id, name, out) => setStr(out, req(id).request.headers.get(str(name)) ?? ""),
      req_headers: (id, out) => setStr(out, flat(req(id).request.headers)),
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
        const content = kind === 1 ? str(text) : kind === 2 ? bytes(body) : null;
        const statusText = str(reason);
        req(id).response = new Response(content, {
          status,
          headers: unflat(str(headers)),
          ...(statusText === "" ? {} : { statusText }),
        });
      },
      // A response whose body the program writes while it is sent (std/http/stream.vlt).
      resp_stream(status, reason, headers) {
        const { readable, writable } = new TransformStream();
        const statusText = str(reason);
        const h = nextHandle++;
        handles.set(h, {
          response: new Response(readable, {
            status,
            headers: unflat(str(headers)),
            ...(statusText === "" ? {} : { statusText }),
          }),
          writer: writable.getWriter(),
          closed: false,
        });
        return h;
      },
      stream_write(w, data) {
        const s = handle(w);
        if (s.closed) return 0;
        s.writer.write(bytes(data)).catch(() => (s.closed = true));
        return 1;
      },
      stream_ready: (w) =>
        promise(
          handle(w).writer.ready.then(
            () => 1,
            () => 0,
          ),
        ),
      stream_close(w) {
        const s = handle(w);
        handles.delete(w);
        return promise(
          s.writer.close().then(
            () => 1,
            () => 0,
          ),
        );
      },
      stream_abort(w) {
        const s = handle(w);
        handles.delete(w);
        s.writer.abort(new Error("the response stream failed")).catch(() => {});
      },
      stream_drop(w) {
        handles.delete(w);
      },
      // The program's own requests (std/fetch/client_io.workers.vlt).
      fetch_send(via, method, url, headers, kind, text, body, redirect) {
        const send = via === 0 ? outbound : (u, i) => handle(via).fetcher.fetch(u, i);
        const h = nextHandle++;
        const r = { response: null, error: "", reader: null, chunk: null };
        handles.set(h, r);
        const init = {
          method: str(method),
          headers: unflat(str(headers)),
          redirect: ["follow", "error", "manual"][redirect] ?? "follow",
        };
        if (kind === 1) init.body = str(text);
        else if (kind === 2) init.body = bytes(body);
        const target = str(url);
        let sent;
        try {
          sent = send(target, init);
        } catch (e) {
          sent = Promise.reject(e);
        }
        return promise(
          sent.then(
            (res) => {
              r.response = res;
              return h;
            },
            (e) => {
              r.error = message(e);
              return h;
            },
          ),
        );
      },
      fetch_error: (h, out) => setStr(out, handle(h).error),
      resp_status: (h) => handle(h).response.status,
      resp_status_text: (h, out) => setStr(out, handle(h).response.statusText),
      resp_url: (h, out) => setStr(out, handle(h).response.url),
      resp_redirected: (h) => (handle(h).response.redirected ? 1 : 0),
      resp_headers: (h, out) => setStr(out, flat(handle(h).response.headers)),
      resp_read(h, whole) {
        const r = handle(h);
        const failed = (e) => {
          r.error = message(e);
          return 2;
        };
        if (whole) {
          return promise(
            r.response.arrayBuffer().then((b) => {
              r.chunk = new Uint8Array(b);
              return 1;
            }, failed),
          );
        }
        if (r.response.body === null) return promise(Promise.resolve(0));
        r.reader ??= r.response.body.getReader();
        return promise(
          r.reader.read().then(({ done, value }) => {
            if (done) return 0;
            r.chunk = value;
            return 1;
          }, failed),
        );
      },
      resp_take(h, out) {
        const r = handle(h);
        setBytes(out, r.chunk ?? new Uint8Array(0));
        r.chunk = null;
      },
      resp_drop(h) {
        const r = handles.get(h);
        handles.delete(h);
        r?.reader?.cancel().catch(() => {});
      },
    },
    velt_cloudflare: {
      binding(name) {
        const key = str(name);
        if (bindings.has(key)) return bindings.get(key);
        const b = env?.[key];
        if (b === undefined || b === null) return 0;
        const h = nextHandle++;
        handles.set(h, { binding: b });
        bindings.set(key, h);
        return h;
      },
      has_env_var: (name) => (typeof env?.[str(name)] === "string" ? 1 : 0),
      env_var: (name, out) => setStr(out, env?.[str(name)] ?? ""),
      random_bytes(n, out) {
        const b = new Uint8Array(n);
        // getRandomValues fills at most 65536 bytes per call.
        for (let i = 0; i < n; i += 65536) crypto.getRandomValues(b.subarray(i, Math.min(n, i + 65536)));
        setBytes(out, b);
      },
      op_error: (h, out) => setStr(out, handle(h).error),
      op_text: (h, out) => setStr(out, handle(h).text),
      op_bytes: (h, out) => setBytes(out, handle(h).bytes ?? new Uint8Array(0)),
      op_drop(h) {
        handles.delete(h);
      },
      r2_get(b, key, head) {
        const bucket = bound(b);
        const k = str(key);
        return op(async (r) => {
          const o = head ? await bucket.head(k) : await bucket.get(k);
          if (o !== null && !head) r.bytes = new Uint8Array(await o.arrayBuffer());
          return r2meta(o);
        });
      },
      r2_put(b, key, kind, text, data, contentType, onlyIfAbsent) {
        const bucket = bound(b);
        const k = str(key);
        const value = kind === 1 ? str(text) : bytes(data);
        const options = {};
        const type = str(contentType);
        if (type !== "") options.httpMetadata = { contentType: type };
        if (onlyIfAbsent) options.onlyIf = new Headers({ "if-none-match": "*" });
        return op(async () => r2meta(await bucket.put(k, value, options)));
      },
      r2_delete(b, key) {
        const bucket = bound(b);
        const k = str(key);
        return op(async () => {
          await bucket.delete(k);
        });
      },
      r2_list(b, prefix, cursor, limit) {
        const bucket = bound(b);
        const options = {};
        const p = str(prefix);
        const c = str(cursor);
        if (p !== "") options.prefix = p;
        if (c !== "") options.cursor = c;
        if (limit > 0) options.limit = limit;
        return op(async () => {
          const page = await bucket.list(options);
          return {
            objects: page.objects.map(r2meta),
            truncated: page.truncated,
            cursor: page.truncated ? page.cursor : "",
          };
        });
      },
      d1_query(d, sql, args, mode) {
        const db = bound(d);
        const query = str(sql);
        const values = JSON.parse(str(args));
        return op(async () => {
          if (mode === 3) {
            await db.exec(query);
            return null;
          }
          const statement = db.prepare(query).bind(...values);
          if (mode === 0) return (await statement.first()) ?? null;
          if (mode === 1) return (await statement.all()).results;
          const { meta } = await statement.run();
          return { changes: meta.changes, lastRowId: meta.last_row_id };
        });
      },
      d1_batch(d, statements) {
        const db = bound(d);
        const list = JSON.parse(str(statements));
        return op(async () => {
          await db.batch(list.map((s) => db.prepare(s.sql).bind(...s.args)));
          return null;
        });
      },
      kv_get(n, key) {
        const ns = bound(n);
        const k = str(key);
        return op(async () => (await ns.get(k)) ?? null);
      },
      kv_put(n, key, value, ttl) {
        const ns = bound(n);
        const k = str(key);
        const v = str(value);
        return op(async () => {
          await ns.put(k, v, ttl > 0 ? { expirationTtl: ttl } : undefined);
        });
      },
      kv_delete(n, key) {
        const ns = bound(n);
        const k = str(key);
        return op(async () => {
          await ns.delete(k);
        });
      },
      kv_list(n, prefix) {
        const ns = bound(n);
        const p = str(prefix);
        return op(async () => (await ns.list(p === "" ? {} : { prefix: p })).keys.map((k) => k.name));
      },
      do_stub(n, name) {
        const ns = bound(n);
        const h = nextHandle++;
        handles.set(h, { fetcher: ns.get(ns.idFromName(str(name))) });
        return h;
      },
      cache_delete(url) {
        const u = str(url);
        return op(async () => {
          if (caches === undefined) throw new Error("this host has no cache");
          return await caches.default.delete(u);
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
    get dead() {
      return state.dead;
    },
    fail,
    async handle(request) {
      if (typeof ex.velt_fetch !== "function") {
        return new Response("this program does not serve: main never calls serve()", {
          status: 500,
        });
      }
      const id = nextRequest++;
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
