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
// `HostSettled<T>` (std/host/promise.vlt) when the program takes it.
//
// Browser:  import { runVelt } from "./velt_web.mjs";
//           const code = await runVelt(fetch("hello.wasm"), { write: (s, bytes) => ..., imports });
// Node:     node velt_web.mjs hello.wasm [args...]   (what `velt run --target
//           wasm32-unknown-unknown` does)
// Other hosts drive the module themselves with `loadVelt` (their own entries and turns).

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
 */
export async function loadVelt(source, host = {}) {
  const write = host.write ?? defaultWrite();
  const refs = host.refs ?? new HostRefs();
  let resolve, reject, timer;
  const rt = {
    memory: null,
    exports: null,
    refs,
    ended: false,
    promises: new Map(),
    nextPromise: 1,
    done: new Promise((res, rej) => ((resolve = res), (reject = rej))),
    end(code, error) {
      if (rt.ended) return;
      rt.ended = true;
      clearTimeout(timer);
      rt.promises.clear();
      if (error === undefined) resolve(code);
      else reject(error);
    },
    enter(f) {
      if (rt.ended) return;
      try {
        f();
      } catch (e) {
        if (e instanceof VeltExit) return rt.end(e.code);
        if (e instanceof WebAssembly.RuntimeError) {
          write(2, new TextEncoder().encode(`wasm trap: ${e.message}\n`));
          return rt.end(134);
        }
        rt.end(0, e);
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
        const delay = Math.min(Math.ceil(ms), 0x7fffffff);
        if (ms >= 0) timer = setTimeout(() => rt.enter(() => rt.exports.velt_turn()), delay);
      },
      host_take(id, out) {
        const p = rt.promises.get(id >>> 0);
        rt.promises.delete(id >>> 0);
        p.take(out >>> 0);
      },
      host_forget: (id) => rt.promises.delete(id >>> 0),
    },
  };
  const module = await compile(await source);
  Object.assign(imports, hostImports(module, host.imports ?? {}, rt));
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

/** The JS values a program holds as `HostRef`s, by slot (0 is `null`); `values.size` counts them. */
export class HostRefs {
  constructor() {
    this.values = new Map();
    this.next = 1;
  }
  add(value) {
    const slot = this.next++;
    this.values.set(slot, value);
    return slot;
  }
  get(slot) {
    if (!this.values.has(slot)) throw new Error(`velt: HostRef slot ${slot} is not in use`);
    return this.values.get(slot);
  }
  release(slot) {
    this.values.delete(slot);
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
    if (!rt.promises.has(id)) return;
    rt.promises.get(id).take = (out) => {
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
    rt.promises.set(id, {});
    let promise;
    try {
      promise = Promise.resolve(fn(...args(raw)[0]));
    } catch (e) {
      promise = Promise.reject(e);
    }
    promise.then((v) => settle(id, true, v), (e) => settle(id, false, e));
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

async function main() {
  const [{ readFile }, { writeSync }] = await Promise.all([
    import("node:fs/promises"),
    import("node:fs"),
  ]);
  const [file] = process.argv.slice(2);
  if (!file) {
    process.stderr.write("usage: node velt_web.mjs <program.wasm> [args...]\n");
    process.exit(2);
  }
  const code = await runVelt(readFile(file), {
    write: (stream, bytes) => writeSync(stream, bytes),
  });
  process.exit(code);
}

if (
  typeof process !== "undefined" &&
  process.argv?.[1] &&
  import.meta.url === (await import("node:url")).pathToFileURL(process.argv[1]).href
) {
  await main();
}
