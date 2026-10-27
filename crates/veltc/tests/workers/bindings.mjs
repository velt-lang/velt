// The requests for bindings.vlt, and in-memory stand-ins for its bindings: R2 (a Map), D1
// (node:sqlite), KV (a Map), a Durable Object (a counter) and the cache. They follow the
// Workers APIs as far as bindings.vlt uses them; workerd's own are checked by hand
// (docs/internals/design/cloudflare-workers.md "As built").
let DatabaseSync;
try {
  ({ DatabaseSync } = await import("node:sqlite"));
} catch {
  // Too old a node: drive.mjs reports the scenario as skipped.
}
export const skip = DatabaseSync === undefined ? "node:sqlite (node 22.5 or newer)" : undefined;

export const requests = () =>
  ["/r2", "/d1", "/kv", "/do", "/cache", "/missing", "/other"].map(
    (p) => new Request(`https://example.com${p}`),
  );

class Bucket {
  objects = new Map();
  #meta(key, o) {
    return {
      key,
      size: o.data.length,
      etag: o.etag,
      httpEtag: `"${o.etag}"`,
      uploaded: new Date(1700000000000),
      httpMetadata: { contentType: o.type },
      arrayBuffer: async () => o.data.slice().buffer,
    };
  }
  async get(key) {
    const o = this.objects.get(key);
    return o === undefined ? null : this.#meta(key, o);
  }
  async head(key) {
    return this.get(key);
  }
  async put(key, value, options = {}) {
    if (options.onlyIf?.get?.("if-none-match") === "*" && this.objects.has(key)) return null;
    const data = typeof value === "string" ? new TextEncoder().encode(value) : value;
    const o = { data, etag: `e${data.length}`, type: options.httpMetadata?.contentType };
    this.objects.set(key, o);
    return this.#meta(key, o);
  }
  async delete(key) {
    this.objects.delete(key);
  }
  async list({ prefix = "" } = {}) {
    const objects = [...this.objects.entries()]
      .filter(([k]) => k.startsWith(prefix))
      .map(([k, o]) => this.#meta(k, o));
    return { objects, truncated: false };
  }
}

class Database {
  #db = new DatabaseSync(":memory:");
  async exec(sql) {
    this.#db.exec(sql);
  }
  prepare(sql) {
    const db = this.#db;
    const statement = (values) => ({
      bind: (...v) => statement(v),
      first: async () => db.prepare(sql).get(...values) ?? null,
      all: async () => ({ results: db.prepare(sql).all(...values) }),
      run: async () => {
        const r = db.prepare(sql).run(...values);
        return { meta: { changes: Number(r.changes), last_row_id: Number(r.lastInsertRowid) } };
      },
      go: () => db.prepare(sql).run(...values),
    });
    return statement([]);
  }
  async batch(statements) {
    this.#db.exec("BEGIN");
    for (const s of statements) s.go();
    this.#db.exec("COMMIT");
  }
}

class Namespace {
  values = new Map();
  async get(key) {
    return this.values.get(key) ?? null;
  }
  async put(key, value) {
    this.values.set(key, value);
  }
  async delete(key) {
    this.values.delete(key);
  }
  async list({ prefix = "" } = {}) {
    return { keys: [...this.values.keys()].filter((k) => k.startsWith(prefix)).map((name) => ({ name })) };
  }
}

const counts = new Map();
const objects = {
  idFromName: (name) => name,
  get: (id) => ({
    async fetch(url, init) {
      const n = (counts.get(id) ?? 0) + 1;
      counts.set(id, n);
      return new Response(`${id}=${n} ${init.method} ${new URL(url).pathname}`);
    },
  }),
};

export const env = DatabaseSync === undefined
  ? {}
  : { BLOBS: new Bucket(), DB: new Database(), KV: new Namespace(), COUNTER: objects };

const cached = new Set(["https://example.com/cached"]);
export const caches = { default: { delete: async (url) => cached.delete(url) } };
