// The host module `cloudflare` (its functions are declared in src/host.vlt): Cloudflare's
// bindings, looked up by name on the Worker's `env` (`host.env`, the context host-cloudflare's
// entry passes) each time a function is called, so a wrapper never holds an object of another
// request's.
//
// What crosses (docs/internals/contracts/workers.md in the Velt repository): strings, bytes,
// numbers and `HostRef`s. Metadata and options cross as JSON text; D1's parameters and rows as
// the frames of src/frame.vlt (`writeValue` and `readValue` below are their JavaScript side). An
// async function rejects with an error whose `name` becomes the `IoError`'s code. A synchronous
// one never throws (a throw would end the program's instance): it records the failure, which
// `failure()` hands over, and returns an empty result.

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** An error named `name` (the program's `IoError.code`). */
function named(name, message) {
  const e = new Error(message);
  e.name = name;
  return e;
}

/** The value tags of a frame (src/frame.vlt). */
const NULL = 0, INT = 1, FLOAT = 2, TEXT = 3, BLOB = 4, BOOL = 5, JSON_TEXT = 6;

/** Bytes appended little-endian, growing as needed. */
class Writer {
  bytes = new Uint8Array(256);
  view = new DataView(this.bytes.buffer);
  length = 0;
  room(n) {
    if (this.length + n <= this.bytes.length) return;
    const grown = new Uint8Array(Math.max(this.bytes.length * 2, this.length + n));
    grown.set(this.bytes.subarray(0, this.length));
    this.bytes = grown;
    this.view = new DataView(grown.buffer);
  }
  u8(v) {
    this.room(1);
    this.bytes[this.length++] = v;
  }
  u32(v) {
    this.room(4);
    this.view.setUint32(this.length, v, true);
    this.length += 4;
  }
  raw(b) {
    this.u32(b.length);
    this.room(b.length);
    this.bytes.set(b, this.length);
    this.length += b.length;
  }
  text(s) {
    this.raw(encoder.encode(s));
  }
  done() {
    return this.bytes.slice(0, this.length);
  }
}

/** Reads what a `Writer` (or src/frame.vlt's `FrameWriter`) wrote. */
class Reader {
  constructor(bytes) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.at = 0;
  }
  u8() {
    return this.bytes[this.at++];
  }
  u32() {
    const v = this.view.getUint32(this.at, true);
    this.at += 4;
    return v;
  }
  raw() {
    const n = this.u32();
    const b = this.bytes.slice(this.at, this.at + n);
    this.at += n;
    return b;
  }
  text() {
    return decoder.decode(this.raw());
  }
}

/** A cell of a D1 row as a frame value: numbers as floats (exact integers read into integer fields). */
function writeValue(w, v) {
  if (v === null || v === undefined) {
    w.u8(NULL);
  } else if (typeof v === "bigint") {
    w.u8(INT);
    w.room(8);
    w.view.setBigInt64(w.length, BigInt.asIntN(64, v), true);
    w.length += 8;
  } else if (typeof v === "number") {
    w.u8(FLOAT);
    w.room(8);
    w.view.setFloat64(w.length, v, true);
    w.length += 8;
  } else if (typeof v === "string") {
    w.u8(TEXT);
    w.text(v);
  } else if (typeof v === "boolean") {
    w.u8(BOOL);
    w.u8(v ? 1 : 0);
  } else if (v instanceof ArrayBuffer || ArrayBuffer.isView(v)) {
    w.u8(BLOB);
    w.raw(new Uint8Array(v.buffer ?? v, v.byteOffset ?? 0, v.byteLength));
  } else if (Array.isArray(v) && v.every((b) => Number.isInteger(b) && b >= 0 && b <= 255)) {
    // D1 gives a BLOB as an array of its bytes.
    w.u8(BLOB);
    w.raw(Uint8Array.from(v));
  } else {
    w.u8(JSON_TEXT);
    w.text(JSON.stringify(v));
  }
}

/** A parameter of a D1 statement, as src/frame.vlt's `FrameWriter` wrote it. */
function readValue(r) {
  const tag = r.u8();
  switch (tag) {
    case NULL:
      return null;
    case FLOAT: {
      const v = r.view.getFloat64(r.at, true);
      r.at += 8;
      return v;
    }
    case TEXT:
      return r.text();
    case BLOB:
      return r.raw();
    case BOOL:
      return r.u8() === 1;
    default:
      throw named("EINVAL", `a D1 parameter frame holds the unknown tag ${tag}`);
  }
}

/** `values`, a parameter frame: a count, then the values. */
function readParams(r) {
  const n = r.u32();
  const out = [];
  for (let i = 0; i < n; i++) out.push(readValue(r));
  return out;
}

/** D1's `meta` with every field the program reads. */
function d1Meta(m = {}) {
  const num = (v) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  return {
    duration: num(m.duration),
    changes: num(m.changes),
    last_row_id: num(m.last_row_id),
    changed_db: m.changed_db === true,
    size_after: num(m.size_after),
    rows_read: num(m.rows_read),
    rows_written: num(m.rows_written),
  };
}

/** A D1 result frame: its meta as JSON, its columns, then its rows' cells. */
function writeResult(w, meta, rows) {
  w.text(meta === null ? "" : JSON.stringify(d1Meta(meta)));
  const columns = rows.length > 0 ? Object.keys(rows[0]) : [];
  w.u32(columns.length);
  for (const c of columns) w.text(c);
  w.u32(rows.length);
  for (const row of rows) for (const c of columns) writeValue(w, row[c]);
}

/** An R2 object's metadata as src/r2_object.vlt reads it. */
function r2Info(o) {
  const h = o.httpMetadata ?? {};
  // Absent fields stay out (the struct's fields are optional).
  const text = (v) => (typeof v === "string" ? v : undefined);
  return {
    key: String(o.key),
    version: String(o.version ?? ""),
    size: Number(o.size ?? 0),
    etag: String(o.etag ?? ""),
    httpEtag: String(o.httpEtag ?? ""),
    uploaded: o.uploaded instanceof Date ? o.uploaded.getTime() : Number(o.uploaded ?? 0),
    httpMetadata: {
      contentType: text(h.contentType),
      contentLanguage: text(h.contentLanguage),
      contentDisposition: text(h.contentDisposition),
      contentEncoding: text(h.contentEncoding),
      cacheControl: text(h.cacheControl),
    },
    customMetadata: { ...(o.customMetadata ?? {}) },
  };
}

/** `o` without its null and undefined fields (options Cloudflare's APIs take). */
function present(o) {
  if (o === null || typeof o !== "object" || Array.isArray(o)) return o;
  const out = {};
  for (const [k, v] of Object.entries(o)) if (v !== null && v !== undefined) out[k] = present(v);
  return out;
}

/** Header lines `name\nvalue\n…` as `[name, value]` pairs. */
function headerPairs(flat) {
  const parts = flat === "" ? [] : flat.split("\n");
  const out = [];
  for (let i = 0; i + 1 < parts.length; i += 2) out.push([parts[i], parts[i + 1]]);
  return out;
}

/** A `Response` read whole, as `responseStatus`/`responseHeaders`/`responseBody` hand it over. */
async function received(res) {
  const headers = [];
  for (const [k, v] of res.headers) headers.push(k, v);
  return { status: res.status, headers: headers.join("\n"), body: new Uint8Array(await res.arrayBuffer()) };
}

export default (host) => {
  let failed = "";
  /** The binding `name` of `env`, which `fits` (it has the methods of a `what`). */
  const binding = (name, what, fits) => {
    const env = host.env;
    const b = env != null && Object.prototype.hasOwnProperty.call(env, name) ? env[name] : undefined;
    if (b == null || !fits(b)) throw named("ENOENT", `the Worker's env has no ${what} binding \`${name}\``);
    return b;
  };
  const r2 = (name) => binding(name, "R2 bucket", (b) => typeof b.head === "function");
  const d1 = (name) => binding(name, "D1 database", (b) => typeof b.prepare === "function");
  const kv = (name) =>
    binding(name, "KV namespace", (b) => typeof b.getWithMetadata === "function");
  const durable = (name) =>
    binding(name, "Durable Object namespace", (b) => typeof b.idFromName === "function");
  const cache = async (name) => (name === "" ? globalThis.caches.default : globalThis.caches.open(name));
  // The reader of each R2 object body the program reads piece by piece.
  const readers = new WeakMap();
  /** `request` as a Fetch `Request`. */
  const request = (method, url, headers, body) =>
    new Request(url, { method, headers: headerPairs(headers), body: body.length > 0 ? body.slice() : undefined });
  /** A synchronous call's result: `f()`, or (it threw) `empty` with the failure recorded. */
  const guarded = (empty, f) => {
    try {
      failed = "";
      return f();
    } catch (e) {
      failed = `${typeof e?.name === "string" && e.name !== "" ? e.name : "Error"}\n${e?.message ?? e}`;
      return empty;
    }
  };

  return {
    failure: () => failed,
    takeBytes: (v) => (v instanceof Uint8Array ? v : new Uint8Array(v)),

    r2Head: async (bucket, key) => (await r2(bucket).head(key)) ?? null,
    r2Get: async (bucket, key) => (await r2(bucket).get(key)) ?? null,
    r2Put: async (bucket, key, value, options) =>
      (await r2(bucket).put(key, value.slice(), present(JSON.parse(options)))) ?? null,
    r2Delete: async (bucket, keys) => {
      await r2(bucket).delete(JSON.parse(keys));
    },
    r2List: async (bucket, options) => {
      const l = await r2(bucket).list(present(JSON.parse(options)));
      return JSON.stringify({
        objects: l.objects.map(r2Info),
        truncated: l.truncated === true,
        cursor: l.truncated ? String(l.cursor) : null,
        delimitedPrefixes: [...(l.delimitedPrefixes ?? [])],
      });
    },
    r2Info: (o) => JSON.stringify(r2Info(o)),
    r2Read: async (o, all) => {
      let reader = readers.get(o);
      if (reader === undefined) {
        if (o.bodyUsed) throw named("EINVAL", "Body is unusable: Body has already been read");
        reader = o.body.getReader();
        readers.set(o, reader);
      }
      const pieces = [];
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value.length === 0) continue;
        if (!all) return value;
        pieces.push(value);
      }
      const out = new Uint8Array(pieces.reduce((n, p) => n + p.length, 0));
      let at = 0;
      for (const p of pieces) {
        out.set(p, at);
        at += p.length;
      }
      return out;
    },

    d1Query: async (db, sql, params, mode) => {
      const stmt = d1(db).prepare(sql).bind(...readParams(new Reader(params)));
      const w = new Writer();
      if (mode === 1) {
        const row = await stmt.first();
        writeResult(w, null, row == null ? [] : [row]);
      } else {
        const r = await (mode === 2 ? stmt.run() : stmt.all());
        writeResult(w, r.meta, mode === 2 ? [] : (r.results ?? []));
      }
      return w.done();
    },
    d1Batch: async (db, statements) => {
      const database = d1(db);
      const r = new Reader(statements);
      const prepared = [];
      for (let n = r.u32(); n > 0; n--) {
        const sql = r.text();
        prepared.push(database.prepare(sql).bind(...readParams(r)));
      }
      const results = await database.batch(prepared);
      const w = new Writer();
      w.u32(results.length);
      for (const res of results) {
        const one = new Writer();
        writeResult(one, res.meta, res.results ?? []);
        w.raw(one.done());
      }
      return w.done();
    },
    d1Exec: async (db, sql) => {
      const r = await d1(db).exec(sql);
      return JSON.stringify({ count: Number(r.count ?? 0), duration: Number(r.duration ?? 0) });
    },

    kvGet: async (ns, key) => (await kv(ns).get(key, "arrayBuffer")) ?? null,
    kvPut: async (ns, key, value, options) => {
      await kv(ns).put(key, value.slice(), present(JSON.parse(options)));
    },
    kvDelete: async (ns, key) => {
      await kv(ns).delete(key);
    },
    kvList: async (ns, options) => {
      const l = await kv(ns).list(present(JSON.parse(options)));
      return JSON.stringify({
        keys: l.keys.map((k) => ({ name: String(k.name), expiration: k.expiration ?? null })),
        list_complete: l.list_complete === true,
        cursor: l.list_complete ? null : String(l.cursor ?? ""),
      });
    },

    cacheMatch: async (name, method, url, headers) => {
      const res = await (await cache(name)).match(request(method, url, headers, new Uint8Array(0)));
      return res == null ? null : received(res);
    },
    cachePut: async (name, url, headers, status, resHeaders, body) => {
      const res = new Response(body.length > 0 ? body.slice() : null, { status, headers: headerPairs(resHeaders) });
      await (await cache(name)).put(request("GET", url, headers, new Uint8Array(0)), res);
    },
    cacheDelete: async (name, method, url, headers) =>
      (await (await cache(name)).delete(request(method, url, headers, new Uint8Array(0)))) === true,

    durableId: (ns, how, value) =>
      guarded("", () => {
        const n = durable(ns);
        const id = how === 0 ? n.idFromName(value) : how === 1 ? n.idFromString(value) : n.newUniqueId();
        return String(id);
      }),
    durableFetch: async (ns, id, method, url, headers, body) => {
      const n = durable(ns);
      return received(await n.get(n.idFromString(id)).fetch(request(method, url, headers, body)));
    },
    responseStatus: (r) => r.status,
    responseHeaders: (r) => r.headers,
    responseBody: (r) => r.body,
  };
};
