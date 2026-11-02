// Fakes of Cloudflare's bindings for drive.mjs (crates/veltc/tests/cloudflare.rs): R2, D1, KV,
// the Cache API and Durable Object namespaces, in memory, with the shapes and behaviour the
// Workers API documents that packages/cloudflare relies on. D1 runs the few statements the
// fixture sends (by their SQL text), so the test needs no SQL engine.

import { createHash } from "node:crypto";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const bytesOf = (v) =>
  typeof v === "string" ? encoder.encode(v) : new Uint8Array(v.buffer ?? v, v.byteOffset ?? 0, v.byteLength).slice();

/** An error as Cloudflare's APIs throw them: a plain `Error` with a message. */
const failure = (message) => new Error(message);

/** `body` as a stream of pieces of at most `size` bytes, `onRead()` called at each read. */
function pieces(body, size, onRead) {
  let at = 0;
  return new ReadableStream({
    pull(c) {
      onRead();
      if (at >= body.length) return c.close();
      c.enqueue(body.slice(at, at + size));
      at += size;
    },
  });
}

class FakeR2Object {
  constructor(key, stored) {
    this.key = key;
    this.version = `v${stored.version}`;
    this.size = stored.body.length;
    this.etag = stored.etag;
    this.httpEtag = `"${stored.etag}"`;
    this.uploaded = new Date(stored.uploaded);
    this.httpMetadata = { ...stored.httpMetadata };
    this.customMetadata = { ...stored.customMetadata };
  }
}

/** An object `get` gives: its body is to be read in the context `watch` names. */
export class FakeR2ObjectBody extends FakeR2Object {
  constructor(key, stored, watch) {
    super(key, stored);
    const owner = watch.context();
    this.body = pieces(stored.body, 4096, () => {
      if (watch.context() !== owner) watch.report(`the body of R2 object ${key} read elsewhere`);
    });
  }
  get bodyUsed() {
    return this.body.locked;
  }
  async arrayBuffer() {
    return new Response(this.body).arrayBuffer();
  }
  async text() {
    return decoder.decode(await this.arrayBuffer());
  }
}

export class FakeR2 {
  objects = new Map();
  versions = 0;
  constructor(watch) {
    this.watch = watch;
  }
  /** Whether `onlyIf` holds for the object stored now (`*`: any etag). */
  holds(onlyIf, stored) {
    if (onlyIf === undefined) return true;
    const { etagMatches, etagDoesNotMatch } = onlyIf;
    const etag = stored?.etag;
    if (etagMatches !== undefined && !(etag !== undefined && (etagMatches === "*" || etagMatches === etag))) return false;
    if (etagDoesNotMatch !== undefined && etag !== undefined && (etagDoesNotMatch === "*" || etagDoesNotMatch === etag)) return false;
    return true;
  }
  async head(key) {
    const s = this.objects.get(key);
    return s === undefined ? null : new FakeR2Object(key, s);
  }
  async get(key) {
    const s = this.objects.get(key);
    return s === undefined ? null : new FakeR2ObjectBody(key, s, this.watch);
  }
  async put(key, value, options = {}) {
    if (!this.holds(options.onlyIf, this.objects.get(key))) return null;
    const body = bytesOf(value);
    const version = ++this.versions;
    const etag = createHash("md5").update(body).update(String(version)).digest("hex");
    const stored = {
      body,
      version,
      etag,
      uploaded: Date.UTC(2026, 10, 2) + version * 1000,
      httpMetadata: options.httpMetadata ?? {},
      customMetadata: options.customMetadata ?? {},
    };
    this.objects.set(key, stored);
    return new FakeR2Object(key, stored);
  }
  async delete(keys) {
    for (const k of Array.isArray(keys) ? keys : [keys]) this.objects.delete(k);
  }
  async list(options = {}) {
    const { prefix = "", cursor, limit = 1000, delimiter } = options;
    let keys = [...this.objects.keys()].filter((k) => k.startsWith(prefix)).sort();
    if (cursor !== undefined) keys = keys.filter((k) => k > cursor);
    const delimitedPrefixes = new Set();
    if (delimiter !== undefined) {
      keys = keys.filter((k) => {
        const at = k.indexOf(delimiter, prefix.length);
        if (at < 0) return true;
        delimitedPrefixes.add(k.slice(0, at + delimiter.length));
        return false;
      });
    }
    const page = keys.slice(0, limit);
    const truncated = keys.length > limit;
    return {
      objects: page.map((k) => new FakeR2Object(k, this.objects.get(k))),
      truncated,
      ...(truncated ? { cursor: page[page.length - 1] } : {}),
      delimitedPrefixes: [...delimitedPrefixes],
    };
  }
  createMultipartUpload() {}
}

/** The fixture's statements, by SQL text: each gives `{ results, meta }`. */
class FakeD1 {
  users = [];
  next = 1;
  meta(extra = {}) {
    return { served_by: "fake", duration: 0.5, changes: 0, last_row_id: 0, changed_db: false, size_after: 4096, rows_read: 0, rows_written: 0, ...extra };
  }
  execute(sql, params) {
    const row = (u) => ({ id: u.id, name: u.name, age: u.age, avatar: u.avatar === null ? null : [...u.avatar] });
    switch (sql) {
      case "INSERT INTO users (name, age, avatar) VALUES (?, ?, ?)": {
        const [name, age, avatar] = params;
        const id = this.next++;
        this.users.push({ id, name, age, avatar: avatar === null ? null : bytesOf(avatar) });
        return { results: [], meta: this.meta({ changes: 1, last_row_id: id, changed_db: true, rows_written: 1 }) };
      }
      case "SELECT count(*) AS n FROM users":
        return { results: [{ n: this.users.length }], meta: this.meta({ rows_read: this.users.length }) };
      case "SELECT id, name, age, avatar FROM users ORDER BY id":
      case "SELECT id, name, age, avatar FROM users":
        return { results: this.users.map(row), meta: this.meta({ rows_read: this.users.length }) };
      case "SELECT id, name, age, avatar FROM users WHERE name = ?":
        return { results: this.users.filter((u) => u.name === params[0]).map(row), meta: this.meta() };
      case "ECHO": {
        // What each parameter arrived as, as D1 would give it back: a blob as an array of its
        // bytes, a boolean as 1 or 0.
        const [text, num, nothing, blob, flag] = params;
        const ok = typeof text === "string" && typeof num === "number" && nothing === null &&
          blob instanceof Uint8Array && typeof flag === "boolean";
        if (!ok) throw failure(`D1_ERROR: parameters crossed as ${params.map((p) => typeof p)}`);
        return { results: [{ text, num, nothing, blob: [...blob], flag: flag ? 1 : 0 }], meta: this.meta() };
      }
      case "DELETE FROM users": {
        const changes = this.users.length;
        this.users = [];
        return { results: [], meta: this.meta({ changes }) };
      }
      default:
        throw failure(`D1_ERROR: no such table: ${sql.split(" ").pop()}: SQLITE_ERROR`);
    }
  }
  prepare(sql) {
    const db = this;
    const statement = (params) => ({
      sql,
      params,
      bind: (...values) => statement(values),
      async first() {
        return db.execute(sql, params).results[0] ?? null;
      },
      async all() {
        return { success: true, ...db.execute(sql, params) };
      },
      async run() {
        return { success: true, ...db.execute(sql, params) };
      },
    });
    return statement([]);
  }
  async batch(statements) {
    return statements.map((s) => ({ success: true, ...this.execute(s.sql, s.params) }));
  }
  async exec(sql) {
    this.execute(sql, []);
    return { count: 1, duration: 0.25 };
  }
}

class FakeKV {
  values = new Map();
  async get(key, type) {
    if (key === "fail") throw failure("KV GET failed: 500 Internal Server Error");
    if (type !== "arrayBuffer") throw failure(`the fake gets "arrayBuffer" only, not ${type}`);
    const v = this.values.get(key);
    return v === undefined ? null : v.value.slice().buffer;
  }
  async getWithMetadata() {}
  async put(key, value, options = {}) {
    const expiration = options.expirationTtl === undefined ? undefined : 1_800_000_000 + options.expirationTtl;
    this.values.set(key, { value: bytesOf(value), expiration });
  }
  async delete(key) {
    this.values.delete(key);
  }
  async list(options = {}) {
    const keys = [...this.values.keys()].filter((k) => k.startsWith(options.prefix ?? "")).sort();
    return {
      keys: keys.map((name) => {
        const { expiration } = this.values.get(name);
        return expiration === undefined ? { name } : { name, expiration };
      }),
      list_complete: true,
      cacheStatus: null,
    };
  }
}

class FakeCache {
  entries = new Map();
  async match(request) {
    const hit = this.entries.get(request.url);
    return hit === undefined ? undefined : new Response(hit.body, { status: hit.status, headers: hit.headers });
  }
  async put(request, response) {
    if (request.method !== "GET") throw new TypeError("Cannot cache response to non-GET request.");
    const body = new Uint8Array(await response.arrayBuffer());
    this.entries.set(request.url, { status: response.status, headers: [...response.headers], body });
  }
  async delete(request) {
    return this.entries.delete(request.url);
  }
}

/** Workers' `caches`. */
export function fakeCaches() {
  const named = new Map();
  return {
    default: new FakeCache(),
    async open(name) {
      if (!named.has(name)) named.set(name, new FakeCache());
      return named.get(name);
    },
  };
}

class FakeId {
  constructor(hex, name) {
    this.hex = hex;
    if (name !== undefined) this.name = name;
  }
  toString() {
    return this.hex;
  }
  equals(other) {
    return other.hex === this.hex;
  }
}

/** A namespace of counters: `/count` counts the object's requests, `/echo` echoes one. */
class FakeDurableObjects {
  counts = new Map();
  unique = 0;
  idFromName(name) {
    return new FakeId(createHash("sha256").update(name).digest("hex"), name);
  }
  idFromString(hex) {
    if (!/^[0-9a-f]{64}$/.test(hex)) throw new TypeError("Invalid Durable Object ID: must be 64 hex digits");
    return new FakeId(hex);
  }
  newUniqueId() {
    return new FakeId(createHash("sha256").update(`unique ${++this.unique}`).digest("hex"));
  }
  get(id) {
    return {
      id,
      fetch: async (request) => {
        const path = new URL(request.url).pathname;
        const headers = { "x-object": id.toString().slice(0, 8) };
        if (path === "/count") {
          const n = (this.counts.get(id.toString()) ?? 0) + 1;
          this.counts.set(id.toString(), n);
          return new Response(String(n), { status: 200, headers });
        }
        const body = await request.text();
        return new Response(`${request.method} ${path} ${request.headers.get("x-who")} ${body}`, { status: 201, headers });
      },
    };
  }
}

/** The Worker's env: the fixture's bindings, and one of another kind under `DB`'s name. */
export function fakeEnv(watch) {
  return {
    BUCKET: new FakeR2(watch),
    DB: new FakeD1(),
    KV: new FakeKV(),
    COUNTER: new FakeDurableObjects(),
  };
}
