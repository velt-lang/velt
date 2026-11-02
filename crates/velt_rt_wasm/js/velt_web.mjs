// JS glue for Velt programs built with `--target wasm32-unknown-unknown`.
//
// The module imports its host services from `velt` (crates/velt_rt_wasm/src/platform.rs,
// host.rs, host_turn.rs and host_promise.rs):
//   write(stream, ptr, len)  bytes for stdout (1) / stderr (2)
//   exit(code)               stop the program at once (thrown as VeltExit: `process.exit`, a panic)
//   done(code)               the program ended with `code`
//   now_ms()                 performance.now()
//   date_ms()                Date.now()
//   host_release(slot)       forget a HostRef's value (HostRefs below)
//   host_clone(slot)         a second slot for a HostRef's value
//   set_timer(ms)            call velt_turn() in `ms` milliseconds, replacing the last request
//                            (none when `ms` is negative)
//   host_take(id, out)       write how host promise `id` settled to `out`, and forget it
//   host_forget(id)          forget host promise `id`
// and exports `memory`, `velt_start()` (runs `main`'s first turn), `velt_turn()` and
// `velt_settle(id)`. Each call returns normally; the program runs in turns of the host's event
// loop, while a timer (`setTimeout`) calls `velt_turn` and a settled host promise `velt_settle`,
// until it reports its end through `done`.
//
// Host functions (`declare host("m") function f(...)`, https://veltlang.com/docs/reference/host-functions/) are
// imports from module "m", which the embedder passes as `imports: { m: { f } }`. The module's
// custom section "velt.host" lists each one, a line "m<TAB>f<TAB>params<TAB>result" (velt_vir's
// vir/host.rs), and the glue wraps the embedder's function to convert what crosses. An async one
// (result "async:<type>") returns the id of its promise, whose outcome the glue writes as
// `HostSettled<T>` (std/host/promise.vlt) when the program takes it. The glue itself provides
// the module `velt:fetch`, std's `fetch` over the host's own (`fetchHost` below).
//
// Browser:  import { runVelt } from "./velt_web.mjs";
//           const code = await runVelt(fetch("hello.wasm"), { write: (s, bytes) => ..., imports });
// Node:     node velt_web.mjs hello.wasm [args...]   (what `velt run --target
//           wasm32-unknown-unknown` does)
// Other hosts drive the module themselves with `loadVelt` (their own entries and turns), as the
// Worker glue velt_worker.mjs does. The file imports nothing, so a bundler (wrangler's) takes it
// as it is: node's modules are reached only when node runs it as its main module.

/** Thrown by the `exit` import to unwind out of the module. */
export class VeltExit extends Error {
  constructor(code) {
    super(`exit ${code}`);
    this.code = code;
  }
}

/**
 * Instantiate and run a Velt program.
 * @param {BufferSource | Response | Promise<Response>} source the .wasm bytes or a fetch()
 * @param {{ write?: (stream: number, bytes: Uint8Array) => void,
 *           imports?: Record<string, Record<string, Function>>, refs?: HostRefs }} host output
 *        sink, the functions of the program's host functions by import module, and the table of
 *        the JS values it holds as `HostRef`s (to inspect; a new one by default)
 * @returns {Promise<number>} the exit code (101 for a panic, 134 for a WebAssembly trap), once
 *          the program ended
 */
export async function runVelt(source, host = {}) {
  const rt = await loadVelt(source, host);
  rt.enter(() => rt.exports.velt_start());
  return rt.done;
}

/**
 * Instantiate a Velt program without running it. `enter(f)` runs `f`, which calls into the
 * module (`exports.velt_start()`, `exports.velt_turn()`, another entry); `done` settles with the
 * exit code once the program reports its end, exits or traps, or rejects with what a host
 * function threw (the instance is then unusable).
 *
 * A host whose calls into the module must happen in a context of its own (a Worker's request,
 * whose I/O and timers end with it) takes over two things. `host.setTimer(ms, fire)` replaces
 * the `setTimeout` that wakes the program (`fire()` runs the turn; a negative `ms` cancels).
 * `host.owner()` is called when the program starts a host promise (one that is not a
 * `Deferred`) and gives the context that made it; `host.resuming(owner)` comes before the turn
 * its settling runs, and `host.released(owner)` once the program no longer waits for it.
 * `host.ended(code, error)`, if given, takes the place of `done`: the outcome, told at once in
 * the context that ended the program.
 */
export async function loadVelt(source, host = {}) {
  const write = host.write ?? defaultWrite();
  const refs = host.refs ?? new HostRefs();
  let resolve, reject, timer;
  const rt = {
    memory: null,
    exports: null,
    host,
    refs,
    ended: false,
    // Calls into the module in progress (a settle during one waits for it to return).
    depth: 0,
    promises: new Map(),
    nextPromise: 1,
    // With `host.ended`, none: a Worker's promise would belong to the first request's context.
    done: host.ended ? undefined : new Promise((res, rej) => ((resolve = res), (reject = rej))),
    end(code, error) {
      if (rt.ended) return;
      rt.ended = true;
      clearTimeout(timer);
      host.setTimer?.(-1, null);
      for (const p of rt.promises.values()) host.released?.(p.owner);
      rt.promises.clear();
      if (host.ended) host.ended(code, error);
      else if (error === undefined) resolve(code);
      else reject(error);
    },
    enter(f) {
      if (rt.ended) return;
      rt.depth++;
      try {
        f();
      } catch (e) {
        if (e instanceof VeltExit) return rt.end(e.code);
        if (e instanceof WebAssembly.RuntimeError) {
          write(2, new TextEncoder().encode(`wasm trap: ${e.message}\n`));
          return rt.end(134);
        }
        rt.end(0, e);
      } finally {
        rt.depth--;
      }
    },
  };
  const imports = {
    velt: {
      write(stream, ptr, len) {
        write(stream, new Uint8Array(rt.memory.buffer, ptr, len).slice());
      },
      exit(code) {
        throw new VeltExit(code);
      },
      done: (code) => rt.end(code),
      now_ms: () => performance.now(),
      date_ms: () => Date.now(),
      host_release: (slot) => refs.release(slot >>> 0),
      host_clone: (slot) => refs.add(refs.get(slot >>> 0)),
      set_timer(ms) {
        clearTimeout(timer);
        // Rounded up: hosts round a delay down to whole milliseconds. Hosts fire a delay longer
        // than 2^31 - 1 ms at once; a turn that comes early asks again.
        const delay = ms < 0 ? -1 : Math.min(Math.ceil(ms), 0x7fffffff);
        const fire = () => rt.enter(() => rt.exports.velt_turn());
        if (host.setTimer) host.setTimer(delay, fire);
        else if (delay >= 0) timer = setTimeout(fire, delay);
      },
      host_take(id, out) {
        const p = rt.promises.get(id >>> 0);
        rt.promises.delete(id >>> 0);
        host.released?.(p.owner);
        p.take(out >>> 0);
      },
      host_forget(id) {
        const p = rt.promises.get(id >>> 0);
        rt.promises.delete(id >>> 0);
        host.released?.(p?.owner);
        p?.deferred?.forgotten();
      },
    },
  };
  const module = await compile(await source);
  const provided = { "velt:fetch": fetchHost(), ...host.imports };
  Object.assign(imports, hostImports(module, provided, rt));
  const instance = await WebAssembly.instantiate(module, imports);
  rt.exports = instance.exports;
  rt.memory = instance.exports.memory;
  return rt;
}

async function compile(source) {
  if (typeof Response !== "undefined" && source instanceof Response) {
    return WebAssembly.compile(await source.arrayBuffer());
  }
  return source instanceof WebAssembly.Module ? source : WebAssembly.compile(source);
}

/** Bits of a `HostRef` slot that index the table; the bits above them are its generation. */
const SLOT_INDEX_BITS = 20;
const SLOT_INDEX_MASK = (1 << SLOT_INDEX_BITS) - 1;

/**
 * The JS values a program holds as `HostRef`s, by slot (0 is `null`); `values.size` counts them.
 * A released slot's index is used again, with the next generation in the slot's high bits, so a
 * long-lived instance (a Worker) never runs out of slots, and a stale slot misses (until its
 * index was reused 4096 times) instead of reaching another value.
 */
export class HostRefs {
  constructor() {
    this.values = new Map();
    this.free = [];
    this.generations = [0];
  }
  add(value) {
    let index = this.free.pop();
    if (index === undefined) {
      index = this.generations.length;
      if (index > SLOT_INDEX_MASK) {
        throw new Error(`velt: the program holds more than ${SLOT_INDEX_MASK} HostRefs at once`);
      }
      this.generations.push(0);
    }
    const slot = ((this.generations[index] << SLOT_INDEX_BITS) | index) >>> 0;
    this.values.set(slot, value);
    return slot;
  }
  get(slot) {
    if (!this.values.has(slot)) throw new Error(`velt: HostRef slot ${slot} is not in use`);
    return this.values.get(slot);
  }
  release(slot) {
    if (!this.values.delete(slot)) return;
    const index = slot & SLOT_INDEX_MASK;
    this.generations[index] = (this.generations[index] + 1) & (0xffffffff >>> SLOT_INDEX_BITS);
    this.free.push(index);
  }
}

/**
 * What an async host function may return instead of a promise: the host settles it with
 * `resolve(value)` or `reject(error)`, which runs the program's turn at once, on the caller's
 * own stack, instead of in a reaction of a promise made when the program called the function.
 * The Worker glue settles the wait for a request from that request's own handler, so the program
 * runs in the request's I/O context. Both return whether the program still waited for it (it may
 * have dropped the call's promise).
 */
export class Deferred {
  #outcome = null;
  #settle = null;
  #forgotten;
  /** `forgotten(value)`: the program dropped its wait after the value was handed to it. */
  constructor(forgotten) {
    this.#forgotten = forgotten;
  }
  resolve(value) {
    return this.#finish(true, value);
  }
  reject(error) {
    return this.#finish(false, error);
  }
  /** Whether nothing settled it yet and the program still waits for it. */
  get pending() {
    return this.#outcome === null && (this.#settle === null || this.#settle.waiting());
  }
  /** Called by the glue once: `settle(ok, value)` delivers the outcome, `waiting()` asks. */
  bind(settle) {
    this.#settle = settle;
    if (this.#outcome !== null) queueMicrotask(() => settle.deliver(...this.#outcome));
  }
  /** The glue's: the program dropped the call's promise. */
  forgotten() {
    if (this.#outcome?.[0] === true) this.#forgotten?.(this.#outcome[1]);
  }
  #finish(ok, value) {
    if (!this.pending) return false;
    this.#outcome = [ok, value];
    if (this.#settle !== null) this.#settle.deliver(ok, value);
    return true;
  }
}

/** The import modules of the host functions the module lists, wrapping the embedder's ones. */
function hostImports(module, provided, rt) {
  const out = {};
  for (const section of WebAssembly.Module.customSections(module, "velt.host")) {
    for (const line of new TextDecoder().decode(section).split("\n")) {
      if (!line) continue;
      const [mod, name, params, result] = line.split("\t");
      const fn = provided[mod]?.[name];
      if (typeof fn !== "function") {
        throw new Error(`velt: the program imports host function ${mod}.${name}, which imports.${mod} lacks`);
      }
      const types = params ? params.split(",") : [];
      (out[mod] ??= {})[name] = hostFunction(`${mod}.${name}`, fn, types, result, rt);
    }
  }
  return out;
}

const utf8 = { decoder: new TextDecoder(), encoder: new TextEncoder() };

/** The range of each integer result type: what the host returns must be in it. */
const INT_RANGES = {
  i8: [-0x80, 0x7f],
  i16: [-0x8000, 0x7fff],
  i32: [-0x80000000, 0x7fffffff],
  u8: [0, 0xff],
  u16: [0, 0xffff],
  u32: [0, 0xffffffff],
  i64: [-(2n ** 63n), 2n ** 63n - 1n],
  u64: [0n, 2n ** 64n - 1n],
};

/**
 * `fn` (host function `label`) called with the program's arguments converted, its result
 * converted back. A result that does not fit the declared type is a TypeError, never wrapped.
 */
function hostFunction(label, fn, params, result, rt) {
  const bytes = (ptr, len) => new Uint8Array(rt.memory.buffer, ptr >>> 0, len >>> 0);
  const fromWasm = {
    i8: (v) => (v << 24) >> 24,
    i16: (v) => (v << 16) >> 16,
    i32: (v) => v | 0,
    u8: (v) => v & 0xff,
    u16: (v) => v & 0xffff,
    u32: (v) => v >>> 0,
    i64: (v) => BigInt.asIntN(64, v),
    u64: (v) => BigInt.asUintN(64, v),
    f64: (v) => v,
    bool: (v) => (v & 1) === 1,
    str: (s) => utf8.decoder.decode(bytes(rt.exports.velt_host_str_ptr(s), rt.exports.velt_host_str_len(s))),
    ref: (slot) => rt.refs.get(slot >>> 0),
    "ref?": (slot) => (slot === 0 ? null : rt.refs.get(slot >>> 0)),
  };
  const wrong = (v, what) => {
    const shown =
      typeof v === "string" ? JSON.stringify(v)
      : typeof v === "bigint" ? `${v}n`
      : Array.isArray(v) ? `[${v}]`
      : String(v);
    return new TypeError(`velt: host function ${label} returned ${shown}, which is not ${what}`);
  };
  // An integer result: a number (or, for 64 bits, a bigint) that is an integer in the range.
  const int = (type) => (v) => {
    const [min, max] = INT_RANGES[type];
    const big = typeof min === "bigint";
    const ok = (typeof v === "number" && Number.isInteger(v)) || (big && typeof v === "bigint");
    const n = big && ok ? BigInt(v) : v;
    if (!ok || n < min || n > max) throw wrong(v, `${type[0] === "u" ? "a" : "an"} ${type}`);
    return big ? BigInt.asIntN(64, n) : n;
  };
  // A string or bytes result: into a buffer of the runtime's, which makes the value at `out`.
  // Allocating may grow (and so move) the memory: bytes that are a view of it are copied first.
  const written = (data, out, make) => {
    if (data.buffer === rt.memory.buffer) data = data.slice();
    const p = rt.exports.velt_host_alloc(data.length);
    bytes(p, data.length).set(data);
    make(out, p, data.length);
  };
  const toWasm = {
    ...Object.fromEntries(Object.keys(INT_RANGES).map((t) => [t, int(t)])),
    void: () => undefined,
    f64: (v) => {
      if (typeof v !== "number") throw wrong(v, "a number");
      return v;
    },
    bool: (v) => {
      if (typeof v !== "boolean") throw wrong(v, "a boolean");
      return v ? 1 : 0;
    },
    str: (v, out) => {
      if (typeof v !== "string") throw wrong(v, "a string");
      written(utf8.encoder.encode(v), out, rt.exports.velt_host_str_new);
    },
    bytes: (v, out) => {
      if (v instanceof ArrayBuffer) v = new Uint8Array(v);
      if (!(v instanceof Uint8Array)) throw wrong(v, "a Uint8Array or an ArrayBuffer");
      written(v, out, rt.exports.velt_host_bytes_new);
    },
    ref: (v) => {
      if (v == null) throw wrong(v, "a value for a HostRef");
      return rt.refs.add(v);
    },
    "ref?": (v) => (v == null ? 0 : rt.refs.add(v)),
  };
  const args = (raw) => {
    const out = [];
    let i = 0;
    for (const p of params) {
      if (p === "bytes" || p === "rbytes") {
        const view = bytes(raw[i], raw[i + 1]);
        out.push(p === "bytes" ? view : view.slice());
        i += 2;
      } else {
        out.push(fromWasm[p](raw[i++]));
      }
    }
    return [out, raw[i]];
  };
  const promised = result.startsWith("async:") ? result.slice(6) : null;
  if (promised === null) {
    return (...raw) => {
      const [values, out] = args(raw);
      return toWasm[result](fn(...values), out);
    };
  }
  // `HostSettled<T>` at `out`: the value at +48, converted (and checked) as a result of its type
  // is; or the rejection's name at +0 and message at +24.
  const SETTERS = {
    i8: "setInt8", u8: "setUint8", bool: "setUint8", i16: "setInt16", u16: "setUint16",
    i32: "setInt32", u32: "setUint32", i64: "setBigInt64", u64: "setBigInt64", f64: "setFloat64",
    ref: "setUint32", "ref?": "setUint32",
  };
  const store = (v, at) => {
    const setter = SETTERS[promised];
    if (setter) {
      const w = toWasm[promised](v);
      new DataView(rt.memory.buffer)[setter](at, w, true);
    } else if (promised !== "void") {
      toWasm[promised](v, at);
    }
  };
  const settle = (id, ok, value) => {
    const pending = rt.promises.get(id);
    if (pending === undefined) return;
    if (rt.depth > 0) {
      // From inside a call into the module (a host function settling a Deferred): after it.
      queueMicrotask(() => settle(id, ok, value));
      return;
    }
    if (pending.deferred === undefined) rt.host.resuming?.(pending.owner);
    pending.take = (out) => {
      if (ok) {
        try {
          return store(value, out + 48);
        } catch (e) {
          value = e;
        }
      }
      const [name, message] = described(value);
      toWasm.str(name, out);
      toWasm.str(message, out + 24);
    };
    rt.enter(() => rt.exports.velt_settle(id));
  };
  return (...raw) => {
    // A u32 that no pending promise has (ids wrap in a long-lived instance).
    let id = rt.nextPromise;
    while (rt.promises.has(id)) id = (id + 1) >>> 0 || 1;
    rt.nextPromise = (id + 1) >>> 0 || 1;
    const pending = {};
    rt.promises.set(id, pending);
    let result;
    try {
      result = fn(...args(raw)[0]);
    } catch (e) {
      result = Promise.reject(e);
    }
    if (result instanceof Deferred) {
      pending.deferred = result;
      result.bind({ deliver: (ok, v) => settle(id, ok, v), waiting: () => rt.promises.has(id) });
    } else {
      pending.owner = rt.host.owner?.();
      Promise.resolve(result).then((v) => settle(id, true, v), (e) => settle(id, false, e));
    }
    return id;
  };
}

/**
 * The `IoError` code and message of what a host promise rejected with: its `name` and `message`
 * when it has them, else `Error` and the value as a string.
 */
function described(value) {
  try {
    const name = typeof value?.name === "string" && value.name !== "" ? value.name : "Error";
    const message = typeof value?.message === "string" ? value.message : String(value);
    return [name, message];
  } catch {
    return ["Error", "a host promise rejected with a value that cannot be shown"];
  }
}

/** What native `fetch` calls the failures undici (node's `fetch`) gives other codes. */
const UNDICI_CODES = {
  UND_ERR_SOCKET: "ECONNRESET",
  UND_ERR_CONNECT_TIMEOUT: "ETIMEDOUT",
  UND_ERR_HEADERS_TIMEOUT: "ETIMEDOUT",
  UND_ERR_BODY_TIMEOUT: "ETIMEDOUT",
  UND_ERR_INVALID_ARG: "EINVAL",
  UND_ERR_REQ_CONTENT_LENGTH_MISMATCH: "EINVAL",
  UND_ERR_NOT_SUPPORTED: "ENOTSUP",
};

/**
 * The `IoError` code native `fetch` gives the failure whose error code is `code`: an errno code
 * as it is, a broken compressed body (zlib's `Z_*`) `EILSEQ`, a response that isn't HTTP
 * (llhttp's `HPE_*`) `EPROTO`, a TLS or certificate failure `EILSEQ`, undici's own mapped, else
 * `UNKNOWN`.
 */
function fetchCode(code) {
  if (/^E[A-Z0-9_]+$/.test(code)) return code;
  if (code.startsWith("Z_") || /CERT|ERR_TLS|ERR_SSL/.test(code)) return "EILSEQ";
  if (code.startsWith("HPE_")) return "EPROTO";
  return UNDICI_CODES[code] ?? "UNKNOWN";
}

/** `text` as a byte string of its UTF-8 (a header value as native `fetch` sends it). */
const byteString = (text) => String.fromCharCode(...utf8.encoder.encode(text));

/** A byte string's bytes as UTF-8 (a header value as native `fetch` reads it). */
const fromByteString = (bytes) =>
  utf8.decoder.decode(Uint8Array.from(bytes, (c) => c.charCodeAt(0)));

/**
 * The import module `velt:fetch`: std's `fetch` on JS hosts over `hostFetch`
 * (std/net/http/fetch/client/transport/js_host.vlt). A call is an object holding the request's
 * AbortController, then its Response, the reader of its body and the piece last read. What a
 * call rejects with is named as native `fetch` names the failure (the program's `IoError.code`):
 * `AbortError` once the call was aborted, else the code of the error's `cause` (`fetchCode`).
 * Header values cross as UTF-8, as native `fetch` sends and reads them.
 */
export function fetchHost(hostFetch = (url, init) => globalThis.fetch(url, init)) {
  const failed = (call, e) => {
    if (call.ctrl.signal.aborted) return { name: "AbortError", message: "This operation was aborted" };
    const cause = e?.cause ?? e;
    const code = typeof cause?.code === "string" ? cause.code : "";
    return { name: fetchCode(code), message: `fetch failed: ${cause?.message ?? e}` };
  };
  const next = async (call) => {
    while (call.reader) {
      const { done, value } = await call.reader.read();
      if (done) break;
      if (value.length > 0) return value;
    }
    call.reader = null;
    return new Uint8Array(0);
  };
  const rest = async (call) => {
    const parts = [];
    for (let p; (p = await next(call)).length > 0; ) parts.push(p);
    const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    parts.reduce((at, p) => (all.set(p, at), at + p.length), 0);
    return all;
  };
  return {
    requestStart: () => ({ ctrl: new AbortController(), res: null, reader: null, piece: null }),
    async requestSend(call, method, url, headers, redirect, kind, text, body) {
      try {
        const list = headers === "" ? [] : headers.split("\n");
        const h = new Headers();
        for (let i = 0; i + 1 < list.length; i += 2) h.append(list[i], byteString(list[i + 1]));
        const init = { method, headers: h, redirect, signal: call.ctrl.signal };
        if (kind === 1) init.body = text;
        else if (kind === 2) init.body = body;
        call.res = await hostFetch(url, init);
      } catch (e) {
        throw failed(call, e);
      }
      call.reader = call.res.body?.getReader() ?? null;
    },
    requestAbort: (call) => call.ctrl.abort(),
    responseStatus: (call) => call.res.status,
    responseStatusText: (call) => call.res.statusText,
    responseUrl: (call) => call.res.url,
    responseRedirected: (call) => call.res.redirected,
    responseHeaders: (call) =>
      [...call.res.headers].map(([k, v]) => `${k}\n${fromByteString(v)}`).join("\n"),
    async bodyRead(call, all) {
      try {
        call.piece = await (all ? rest(call) : next(call));
      } catch (e) {
        throw failed(call, e);
      }
    },
    bodyTake(call) {
      const piece = call.piece;
      call.piece = null;
      return piece;
    },
    bodyCancel(call) {
      call.reader?.cancel().catch(() => {});
      call.reader = null;
    },
  };
}

/** console-based output for browsers: decodes UTF-8 per stream and logs whole lines. */
function defaultWrite() {
  const decoders = { 1: new TextDecoder(), 2: new TextDecoder() };
  const pending = { 1: "", 2: "" };
  return (stream, bytes) => {
    const text = pending[stream] + decoders[stream].decode(bytes, { stream: true });
    const lines = text.split("\n");
    pending[stream] = lines.pop();
    for (const line of lines) (stream === 2 ? console.error : console.log)(line);
  };
}

async function main(fs) {
  const [file] = process.argv.slice(2);
  if (!file) {
    process.stderr.write("usage: node velt_web.mjs <program.wasm> [args...]\n");
    process.exit(2);
  }
  const code = await runVelt(fs.readFileSync(file), {
    write: (stream, bytes) => fs.writeSync(stream, bytes),
  });
  process.exit(code);
}

// Node's modules come from `process.getBuiltinModule` (node 20.16 and newer), not an `import`, so
// no bundler goes looking for them.
const node = globalThis.process?.getBuiltinModule;
if (
  typeof node === "function" &&
  process.argv?.[1] &&
  import.meta.url === node("node:url").pathToFileURL(process.argv[1]).href
) {
  await main(node("node:fs"));
} else if (typeof node !== "function" && globalThis.process?.argv?.[1]?.endsWith("velt_web.mjs")) {
  process.stderr.write("velt_web.mjs needs node 20.16 or newer (process.getBuiltinModule)\n");
  process.exit(2);
}
